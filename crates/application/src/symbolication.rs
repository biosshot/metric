//! Application-owned symbolication classification and backend policy boundary.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use faultkeep_domain::{
    event::{CanonicalValue, EventPlatform, NormalizedEvent, NormalizedEventBody},
    symbolication::{
        BackendSymbolicationResult, BackendSymbolicationStatus, RawStacktrace, RawTraceOrigin,
        SymbolicatedStacktrace, SymbolicationDiagnosticCode, SymbolicationDisposition,
        SymbolicationKind, SymbolicationModule, SymbolicationRequest, SymbolicationResult,
        SymbolicationStatus,
    },
};
use faultkeep_ports::{SymbolicationBackend, SymbolicationBackendError};
use thiserror::Error;
use tokio::{sync::Semaphore, time::Instant};
use tokio_util::sync::CancellationToken;

const HARD_MAX_CONCURRENCY: usize = 4_096;
const HARD_MAX_COLLECTION: usize = 65_536;
const HARD_MAX_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_MODULE_TEXT_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolicationConfig {
    pub max_concurrency: usize,
    pub timeout: Duration,
    pub max_request_traces: usize,
    pub max_request_frames: usize,
    pub max_modules: usize,
    pub max_missing_debug_ids: usize,
    pub max_diagnostics: usize,
    pub max_derived_traces: usize,
    pub max_derived_frames: usize,
}

impl Default for SymbolicationConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 8,
            timeout: Duration::from_secs(10),
            max_request_traces: 128,
            max_request_frames: 4_096,
            max_modules: 1_024,
            max_missing_debug_ids: 128,
            max_diagnostics: 64,
            max_derived_traces: 128,
            max_derived_frames: 4_096,
        }
    }
}

impl SymbolicationConfig {
    pub fn validate(self) -> Result<Self, SymbolicationConfigError> {
        let collections = [
            self.max_request_traces,
            self.max_request_frames,
            self.max_modules,
            self.max_missing_debug_ids,
            self.max_diagnostics,
            self.max_derived_traces,
            self.max_derived_frames,
        ];
        if self.max_concurrency == 0
            || self.max_concurrency > HARD_MAX_CONCURRENCY
            || self.timeout.is_zero()
            || self.timeout > HARD_MAX_TIMEOUT
            || collections
                .iter()
                .any(|value| *value == 0 || *value > HARD_MAX_COLLECTION)
        {
            return Err(SymbolicationConfigError::OutOfRange);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SymbolicationConfigError {
    #[error("symbolication limit is zero or above its hard ceiling")]
    OutOfRange,
}

/// Backend-enabled stage. Processor later owns persistence of `Retryable` outcomes.
pub struct SymbolicationService<B> {
    backend: Arc<B>,
    permits: Arc<Semaphore>,
    config: SymbolicationConfig,
}

impl<B> Clone for SymbolicationService<B> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            permits: Arc::clone(&self.permits),
            config: self.config,
        }
    }
}

impl<B: SymbolicationBackend> SymbolicationService<B> {
    pub fn new(
        backend: Arc<B>,
        config: SymbolicationConfig,
    ) -> Result<Self, SymbolicationConfigError> {
        let config = config.validate()?;
        Ok(Self {
            backend,
            permits: Arc::new(Semaphore::new(config.max_concurrency)),
            config,
        })
    }

    pub async fn symbolicate(
        &self,
        event: &NormalizedEvent,
        cancellation: &CancellationToken,
    ) -> SymbolicationResult {
        self.symbolicate_with_revisions(event, 0, 0, cancellation)
            .await
    }

    pub async fn symbolicate_with_revision(
        &self,
        event: &NormalizedEvent,
        debug_file_revision: u64,
        cancellation: &CancellationToken,
    ) -> SymbolicationResult {
        self.symbolicate_with_revisions(event, debug_file_revision, 0, cancellation)
            .await
    }

    pub async fn symbolicate_with_revisions(
        &self,
        event: &NormalizedEvent,
        debug_file_revision: u64,
        artifact_revision: u64,
        cancellation: &CancellationToken,
    ) -> SymbolicationResult {
        let raw = collect_raw_traces(&event.body);
        let kind = classify(&event.body, &raw);
        if kind == SymbolicationKind::NotRequired {
            return result(
                kind,
                SymbolicationStatus::NotRequired,
                SymbolicationDisposition::Continue,
                raw,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
        }
        let modules = collect_modules(&event.body, self.config.max_modules.saturating_add(1));
        if request_exceeds_limits(&raw, &modules, self.config) {
            return terminal_raw(
                kind,
                SymbolicationStatus::Malformed,
                raw,
                SymbolicationDiagnosticCode::RequestLimitExceeded,
            );
        }
        if cancellation.is_cancelled() {
            return retryable_raw(
                kind,
                SymbolicationStatus::Cancelled,
                raw,
                SymbolicationDiagnosticCode::Cancelled,
            );
        }
        let request = SymbolicationRequest {
            project_id: event.project_id,
            debug_file_revision,
            artifact_revision,
            kind,
            traces: raw.clone(),
            modules,
            release: event.body.release.clone(),
            dist: event.body.dist.clone(),
        };
        let deadline = Instant::now() + self.config.timeout;
        let permit = tokio::select! {
            () = cancellation.cancelled() => {
                return retryable_raw(kind, SymbolicationStatus::Cancelled, raw, SymbolicationDiagnosticCode::Cancelled);
            }
            permit = tokio::time::timeout_at(deadline, self.permits.clone().acquire_owned()) => {
                match permit {
                    Ok(Ok(permit)) => permit,
                    Ok(Err(_)) => return retryable_raw(kind, SymbolicationStatus::Unavailable, raw, SymbolicationDiagnosticCode::BackendUnavailable),
                    Err(_) => return retryable_raw(kind, SymbolicationStatus::Timeout, raw, SymbolicationDiagnosticCode::BackendTimeout),
                }
            }
        };
        let backend = self.backend.symbolicate(request);
        let backend_result = tokio::select! {
            () = cancellation.cancelled() => {
                drop(permit);
                return retryable_raw(kind, SymbolicationStatus::Cancelled, raw, SymbolicationDiagnosticCode::Cancelled);
            }
            response = tokio::time::timeout_at(deadline, backend) => response,
        };
        drop(permit);
        match backend_result {
            Err(_) => retryable_raw(
                kind,
                SymbolicationStatus::Timeout,
                raw,
                SymbolicationDiagnosticCode::BackendTimeout,
            ),
            Ok(Err(SymbolicationBackendError::Unavailable)) => retryable_raw(
                kind,
                SymbolicationStatus::Unavailable,
                raw,
                SymbolicationDiagnosticCode::BackendUnavailable,
            ),
            Ok(Err(SymbolicationBackendError::Timeout)) => retryable_raw(
                kind,
                SymbolicationStatus::Timeout,
                raw,
                SymbolicationDiagnosticCode::BackendTimeout,
            ),
            Ok(Err(SymbolicationBackendError::MalformedResponse)) => terminal_raw(
                kind,
                SymbolicationStatus::Malformed,
                raw,
                SymbolicationDiagnosticCode::MalformedBackendResponse,
            ),
            Ok(Ok(response)) => map_backend_result(kind, raw, response, self.config),
        }
    }
}

/// Production-safe behavior before an external backend is configured.
#[derive(Debug, Clone, Copy, Default)]
pub struct BaselineSymbolicationService;

impl BaselineSymbolicationService {
    #[must_use]
    pub fn symbolicate(event: &NormalizedEvent) -> SymbolicationResult {
        let raw = collect_raw_traces(&event.body);
        let kind = classify(&event.body, &raw);
        if kind == SymbolicationKind::NotRequired {
            result(
                kind,
                SymbolicationStatus::NotRequired,
                SymbolicationDisposition::Continue,
                raw,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        } else {
            result(
                kind,
                SymbolicationStatus::Unavailable,
                SymbolicationDisposition::FinalizeRaw,
                raw,
                Vec::new(),
                Vec::new(),
                vec![SymbolicationDiagnosticCode::BaselineBackendDisabled],
            )
        }
    }
}

#[must_use]
pub fn classify(body: &NormalizedEventBody, raw: &[RawStacktrace]) -> SymbolicationKind {
    if matches!(
        body.platform,
        EventPlatform::JavaScript | EventPlatform::Node
    ) && raw.iter().any(|trace| !trace.frames.is_empty())
    {
        SymbolicationKind::JavaScript
    } else if raw
        .iter()
        .flat_map(|trace| &trace.frames)
        .any(|frame| frame.instruction_address.is_some() || frame.symbol_address.is_some())
    {
        SymbolicationKind::Native
    } else {
        SymbolicationKind::NotRequired
    }
}

#[must_use]
pub fn collect_raw_traces(body: &NormalizedEventBody) -> Vec<RawStacktrace> {
    let mut traces = Vec::with_capacity(body.exceptions.len().saturating_mul(2).saturating_add(1));
    if !body.stacktrace.is_empty() {
        traces.push(RawStacktrace {
            origin: RawTraceOrigin::Event,
            frames: body.stacktrace.clone(),
        });
    }
    for (index, exception) in body.exceptions.iter().enumerate() {
        if !exception.stacktrace.is_empty() {
            traces.push(RawStacktrace {
                origin: RawTraceOrigin::Exception { index },
                frames: exception.stacktrace.clone(),
            });
        }
        if !exception.raw_stacktrace.is_empty() {
            traces.push(RawStacktrace {
                origin: RawTraceOrigin::ExceptionRaw { index },
                frames: exception.raw_stacktrace.clone(),
            });
        }
    }
    traces
}

fn collect_modules(body: &NormalizedEventBody, maximum: usize) -> Vec<SymbolicationModule> {
    let Some(CanonicalValue::Object(debug_meta)) = body.unknown.get("debug_meta") else {
        return Vec::new();
    };
    let Some(CanonicalValue::Array(images)) = debug_meta.get("images") else {
        return Vec::new();
    };
    images
        .iter()
        .take(maximum)
        .filter_map(|image| {
            let CanonicalValue::Object(image) = image else {
                return None;
            };
            Some(SymbolicationModule {
                kind: module_string(image.get("type")),
                debug_id: module_string(image.get("debug_id")),
                code_id: module_string(image.get("code_id")),
                code_file: module_string(image.get("code_file")),
                image_address: module_string(image.get("image_addr")),
                image_size: module_u64(image.get("image_size")),
            })
        })
        .collect()
}

fn module_string(value: Option<&CanonicalValue>) -> Option<Box<str>> {
    let CanonicalValue::String(value) = value? else {
        return None;
    };
    (value.len() <= MAX_MODULE_TEXT_BYTES).then(|| value.clone())
}

fn module_u64(value: Option<&CanonicalValue>) -> Option<u64> {
    let CanonicalValue::Number(value) = value? else {
        return None;
    };
    value.parse().ok()
}

fn request_exceeds_limits(
    raw: &[RawStacktrace],
    modules: &[SymbolicationModule],
    config: SymbolicationConfig,
) -> bool {
    raw.len() > config.max_request_traces
        || raw.iter().map(|trace| trace.frames.len()).sum::<usize>() > config.max_request_frames
        || modules.len() > config.max_modules
}

fn map_backend_result(
    kind: SymbolicationKind,
    raw: Vec<RawStacktrace>,
    response: BackendSymbolicationResult,
    config: SymbolicationConfig,
) -> SymbolicationResult {
    if response.derived.len() > config.max_derived_traces
        || response
            .derived
            .iter()
            .map(|trace| trace.frames.len())
            .sum::<usize>()
            > config.max_derived_frames
        || response.missing_debug_ids.len() > config.max_missing_debug_ids
        || response.diagnostics.len() > config.max_diagnostics
        || response
            .missing_debug_ids
            .iter()
            .any(|identifier| identifier.len() > MAX_MODULE_TEXT_BYTES)
        || !derived_matches_raw(&response.derived, &raw)
        || (response.status == BackendSymbolicationStatus::Complete
            && !complete_covers_raw(&response.derived, &raw))
    {
        return terminal_raw(
            kind,
            SymbolicationStatus::Malformed,
            raw,
            SymbolicationDiagnosticCode::MalformedBackendResponse,
        );
    }
    let (status, disposition, default_diagnostic) = match response.status {
        BackendSymbolicationStatus::Complete => (
            SymbolicationStatus::Complete,
            SymbolicationDisposition::Continue,
            None,
        ),
        BackendSymbolicationStatus::Partial => (
            SymbolicationStatus::Partial,
            SymbolicationDisposition::Continue,
            Some(SymbolicationDiagnosticCode::BackendPartial),
        ),
        BackendSymbolicationStatus::Missing => (
            SymbolicationStatus::Missing,
            SymbolicationDisposition::FinalizeRaw,
            Some(SymbolicationDiagnosticCode::MissingDebugFile),
        ),
        BackendSymbolicationStatus::Malformed => (
            SymbolicationStatus::Malformed,
            SymbolicationDisposition::FinalizeRaw,
            Some(SymbolicationDiagnosticCode::MalformedDebugFile),
        ),
    };
    let mut diagnostics = response.diagnostics;
    if let Some(diagnostic) = default_diagnostic
        && diagnostics.len() < config.max_diagnostics
        && !diagnostics.contains(&diagnostic)
    {
        diagnostics.push(diagnostic);
    }
    result(
        kind,
        status,
        disposition,
        raw,
        response.derived,
        response.missing_debug_ids,
        diagnostics,
    )
}

fn derived_matches_raw(derived: &[SymbolicatedStacktrace], raw: &[RawStacktrace]) -> bool {
    let mut origins = BTreeSet::new();
    derived.iter().all(|derived_trace| {
        if !origins.insert(origin_key(derived_trace.origin)) {
            return false;
        }
        raw.iter()
            .find(|raw_trace| raw_trace.origin == derived_trace.origin)
            .is_some_and(|raw_trace| {
                let indexes = derived_trace
                    .frames
                    .iter()
                    .map(|frame| frame.original_index)
                    .collect::<BTreeSet<_>>();
                indexes.len() == derived_trace.frames.len()
                    && indexes.iter().all(|index| *index < raw_trace.frames.len())
            })
    })
}

fn complete_covers_raw(derived: &[SymbolicatedStacktrace], raw: &[RawStacktrace]) -> bool {
    raw.iter().all(|raw_trace| {
        derived
            .iter()
            .find(|derived_trace| derived_trace.origin == raw_trace.origin)
            .is_some_and(|derived_trace| {
                derived_trace.frames.len() == raw_trace.frames.len()
                    && derived_trace
                        .frames
                        .iter()
                        .map(|frame| frame.original_index)
                        .collect::<BTreeSet<_>>()
                        == (0..raw_trace.frames.len()).collect::<BTreeSet<_>>()
            })
    })
}

fn origin_key(origin: RawTraceOrigin) -> (u8, usize) {
    match origin {
        RawTraceOrigin::Event => (0, 0),
        RawTraceOrigin::Exception { index } => (1, index),
        RawTraceOrigin::ExceptionRaw { index } => (2, index),
    }
}

fn retryable_raw(
    kind: SymbolicationKind,
    status: SymbolicationStatus,
    raw: Vec<RawStacktrace>,
    diagnostic: SymbolicationDiagnosticCode,
) -> SymbolicationResult {
    result(
        kind,
        status,
        SymbolicationDisposition::Retryable,
        raw,
        Vec::new(),
        Vec::new(),
        vec![diagnostic],
    )
}

fn terminal_raw(
    kind: SymbolicationKind,
    status: SymbolicationStatus,
    raw: Vec<RawStacktrace>,
    diagnostic: SymbolicationDiagnosticCode,
) -> SymbolicationResult {
    result(
        kind,
        status,
        SymbolicationDisposition::FinalizeRaw,
        raw,
        Vec::new(),
        Vec::new(),
        vec![diagnostic],
    )
}

fn result(
    kind: SymbolicationKind,
    status: SymbolicationStatus,
    disposition: SymbolicationDisposition,
    raw: Vec<RawStacktrace>,
    derived: Vec<SymbolicatedStacktrace>,
    missing_debug_ids: Vec<Box<str>>,
    diagnostics: Vec<SymbolicationDiagnosticCode>,
) -> SymbolicationResult {
    SymbolicationResult {
        kind,
        status,
        disposition,
        raw,
        derived,
        missing_debug_ids,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use faultkeep_domain::{
        AcceptedEvent, EventId, ProjectId, ScrubbedEventPayload, Timestamp,
        symbolication::{SymbolicatedFrame, SymbolicatedStacktrace},
    };
    use faultkeep_ports::PortFuture;

    use crate::normalizer::{Normalizer, NormalizerLimits};

    use super::*;

    #[derive(Clone)]
    struct FakeBackend {
        outcome: Result<BackendSymbolicationResult, SymbolicationBackendError>,
        delay: Duration,
        calls: Arc<AtomicUsize>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<SymbolicationRequest>>>,
    }

    impl FakeBackend {
        fn new(outcome: Result<BackendSymbolicationResult, SymbolicationBackendError>) -> Self {
            Self {
                outcome,
                delay: Duration::ZERO,
                calls: Arc::new(AtomicUsize::new(0)),
                active: Arc::new(AtomicUsize::new(0)),
                max_active: Arc::new(AtomicUsize::new(0)),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_delay(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    struct ActiveGuard(Arc<AtomicUsize>);

    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl SymbolicationBackend for FakeBackend {
        fn symbolicate(
            &self,
            request: SymbolicationRequest,
        ) -> PortFuture<'_, Result<BackendSymbolicationResult, SymbolicationBackendError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests
                .lock()
                .expect("fake request lock poisoned")
                .push(request);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            let guard = ActiveGuard(Arc::clone(&self.active));
            let delay = self.delay;
            let outcome = self.outcome.clone();
            Box::pin(async move {
                let _guard = guard;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                outcome
            })
        }
    }

    fn normalized(payload: &str) -> NormalizedEvent {
        Normalizer::new(NormalizerLimits::default())
            .unwrap()
            .normalize(&AcceptedEvent {
                project_id: ProjectId::new(42).unwrap(),
                event_id: EventId::parse("0123456789abcdef0123456789abcdef").unwrap(),
                received_at: Timestamp::from_unix_millis(1_753_200_000_000).unwrap(),
                policy_revision: 7,
                payload: ScrubbedEventPayload::new(payload.as_bytes()),
            })
            .unwrap()
    }

    fn native_event() -> NormalizedEvent {
        normalized(
            r#"{"platform":"native","stacktrace":{"frames":[{"instruction_addr":"0x10","package":"demo","filename":"main.cpp"}]},"debug_meta":{"images":[{"type":"elf","debug_id":"A","image_addr":"0x0","image_size":4096}]}}"#,
        )
    }

    fn javascript_event() -> NormalizedEvent {
        normalized(
            r#"{"platform":"javascript","release":"web@1","dist":"42","exception":{"values":[{"type":"TypeError","stacktrace":{"frames":[{"filename":"app.min.js","lineno":1,"colno":2}]}}]}}"#,
        )
    }

    fn backend_result(status: BackendSymbolicationStatus) -> BackendSymbolicationResult {
        let derived = if matches!(
            status,
            BackendSymbolicationStatus::Complete | BackendSymbolicationStatus::Partial
        ) {
            vec![SymbolicatedStacktrace {
                origin: RawTraceOrigin::Event,
                frames: vec![SymbolicatedFrame {
                    original_index: 0,
                    function: Some("main".into()),
                    filename: Some("main.cpp".into()),
                    module: Some("demo".into()),
                    line: Some(42),
                    column: None,
                }],
            }]
        } else {
            Vec::new()
        };
        BackendSymbolicationResult {
            status,
            derived,
            missing_debug_ids: if status == BackendSymbolicationStatus::Missing {
                vec!["A".into()]
            } else {
                Vec::new()
            },
            diagnostics: Vec::new(),
        }
    }

    #[tokio::test]
    async fn not_required_never_calls_backend_and_preserves_raw() {
        let event = normalized(
            r#"{"platform":"python","stacktrace":{"frames":[{"filename":"main.py","lineno":4}]}}"#,
        );
        let expected_raw = collect_raw_traces(&event.body);
        let backend = Arc::new(FakeBackend::new(Ok(backend_result(
            BackendSymbolicationStatus::Complete,
        ))));
        let service =
            SymbolicationService::new(Arc::clone(&backend), SymbolicationConfig::default())
                .unwrap();
        let result = service.symbolicate(&event, &CancellationToken::new()).await;
        assert_eq!(result.status, SymbolicationStatus::NotRequired);
        assert_eq!(result.disposition, SymbolicationDisposition::Continue);
        assert_eq!(result.raw, expected_raw);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn complete_partial_missing_and_malformed_vectors_preserve_raw() {
        let event = native_event();
        let expected_raw = collect_raw_traces(&event.body);
        let vectors = [
            (
                BackendSymbolicationStatus::Complete,
                SymbolicationStatus::Complete,
                SymbolicationDisposition::Continue,
            ),
            (
                BackendSymbolicationStatus::Partial,
                SymbolicationStatus::Partial,
                SymbolicationDisposition::Continue,
            ),
            (
                BackendSymbolicationStatus::Missing,
                SymbolicationStatus::Missing,
                SymbolicationDisposition::FinalizeRaw,
            ),
            (
                BackendSymbolicationStatus::Malformed,
                SymbolicationStatus::Malformed,
                SymbolicationDisposition::FinalizeRaw,
            ),
        ];
        for (backend_status, status, disposition) in vectors {
            let service = SymbolicationService::new(
                Arc::new(FakeBackend::new(Ok(backend_result(backend_status)))),
                SymbolicationConfig::default(),
            )
            .unwrap();
            let output = service.symbolicate(&event, &CancellationToken::new()).await;
            assert_eq!(output.status, status);
            assert_eq!(output.disposition, disposition);
            assert_eq!(output.raw, expected_raw);
        }
    }

    #[tokio::test]
    async fn timeout_unavailable_and_cancellation_are_retryable_and_preserve_raw() {
        let event = native_event();
        let expected_raw = collect_raw_traces(&event.body);
        let timeout_config = SymbolicationConfig {
            timeout: Duration::from_millis(10),
            ..SymbolicationConfig::default()
        };
        let timeout_service = SymbolicationService::new(
            Arc::new(
                FakeBackend::new(Ok(backend_result(BackendSymbolicationStatus::Complete)))
                    .with_delay(Duration::from_secs(1)),
            ),
            timeout_config,
        )
        .unwrap();
        let timeout = timeout_service
            .symbolicate(&event, &CancellationToken::new())
            .await;
        assert_eq!(timeout.status, SymbolicationStatus::Timeout);
        assert_eq!(timeout.disposition, SymbolicationDisposition::Retryable);
        assert_eq!(timeout.raw, expected_raw);

        let unavailable_service = SymbolicationService::new(
            Arc::new(FakeBackend::new(Err(
                SymbolicationBackendError::Unavailable,
            ))),
            SymbolicationConfig::default(),
        )
        .unwrap();
        let unavailable = unavailable_service
            .symbolicate(&event, &CancellationToken::new())
            .await;
        assert_eq!(unavailable.status, SymbolicationStatus::Unavailable);
        assert_eq!(unavailable.raw, expected_raw);

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let cancelled = unavailable_service.symbolicate(&event, &cancellation).await;
        assert_eq!(cancelled.status, SymbolicationStatus::Cancelled);
        assert_eq!(cancelled.raw, expected_raw);
    }

    #[tokio::test]
    async fn concurrency_is_bounded_and_backend_futures_are_not_detached() {
        let event = Arc::new(native_event());
        let backend = Arc::new(
            FakeBackend::new(Ok(backend_result(BackendSymbolicationStatus::Complete)))
                .with_delay(Duration::from_millis(20)),
        );
        let service = SymbolicationService::new(
            Arc::clone(&backend),
            SymbolicationConfig {
                max_concurrency: 2,
                timeout: Duration::from_secs(2),
                ..SymbolicationConfig::default()
            },
        )
        .unwrap();
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let service = service.clone();
            let event = Arc::clone(&event);
            tasks.push(tokio::spawn(async move {
                service.symbolicate(&event, &CancellationToken::new()).await
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap().status, SymbolicationStatus::Complete);
        }
        assert_eq!(backend.max_active.load(Ordering::SeqCst), 2);
        assert_eq!(backend.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn invalid_backend_mapping_and_request_limit_finalize_raw() {
        let event = native_event();
        let expected_raw = collect_raw_traces(&event.body);
        let invalid = BackendSymbolicationResult {
            status: BackendSymbolicationStatus::Complete,
            derived: vec![SymbolicatedStacktrace {
                origin: RawTraceOrigin::Event,
                frames: vec![SymbolicatedFrame {
                    original_index: 99,
                    function: None,
                    filename: None,
                    module: None,
                    line: None,
                    column: None,
                }],
            }],
            missing_debug_ids: Vec::new(),
            diagnostics: Vec::new(),
        };
        let service = SymbolicationService::new(
            Arc::new(FakeBackend::new(Ok(invalid))),
            SymbolicationConfig::default(),
        )
        .unwrap();
        let output = service.symbolicate(&event, &CancellationToken::new()).await;
        assert_eq!(output.status, SymbolicationStatus::Malformed);
        assert_eq!(output.raw, expected_raw);

        let oversized = normalized(
            r#"{"platform":"native","stacktrace":{"frames":[{"instruction_addr":"0x10"},{"instruction_addr":"0x20"}]}}"#,
        );
        let limited_backend = Arc::new(FakeBackend::new(Ok(backend_result(
            BackendSymbolicationStatus::Complete,
        ))));
        let limited = SymbolicationService::new(
            Arc::clone(&limited_backend),
            SymbolicationConfig {
                max_request_frames: 1,
                ..SymbolicationConfig::default()
            },
        )
        .unwrap();
        let output = limited
            .symbolicate(&oversized, &CancellationToken::new())
            .await;
        assert_eq!(output.status, SymbolicationStatus::Malformed);
        assert_eq!(output.disposition, SymbolicationDisposition::FinalizeRaw);
        assert_eq!(output.raw, collect_raw_traces(&oversized.body));
        assert_eq!(limited_backend.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn disabled_baseline_finalizes_required_work_without_false_success() {
        let native = native_event();
        let output = BaselineSymbolicationService::symbolicate(&native);
        assert_eq!(output.kind, SymbolicationKind::Native);
        assert_eq!(output.status, SymbolicationStatus::Unavailable);
        assert_eq!(output.disposition, SymbolicationDisposition::FinalizeRaw);
        assert_eq!(output.raw, collect_raw_traces(&native.body));

        let ordinary = normalized(r#"{"platform":"python","message":"boom"}"#);
        let output = BaselineSymbolicationService::symbolicate(&ordinary);
        assert_eq!(output.status, SymbolicationStatus::NotRequired);
        assert_eq!(output.disposition, SymbolicationDisposition::Continue);
    }

    #[test]
    #[ignore = "Phase 6 classification/baseline RPS runs in release mode"]
    fn performance_symbolication_baseline_rps() {
        let events = [
            normalized(r#"{"platform":"python","message":"boom"}"#),
            native_event(),
            javascript_event(),
        ];
        let iterations = 100_000_u64;
        let started = std::time::Instant::now();
        for index in 0..iterations {
            std::hint::black_box(BaselineSymbolicationService::symbolicate(
                &events[index as usize % events.len()],
            ));
        }
        let rps = iterations as f64 / started.elapsed().as_secs_f64();
        eprintln!("Symbolication Phase 6 baseline: rps={rps:.0},events={iterations}");
        assert!(rps >= 20_000.0, "baseline {rps:.0} RPS is below gate");
    }
}
