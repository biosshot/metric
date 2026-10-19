//! External Symbolicator HTTP adapter and private-source token contract.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use metric_domain::{
    ProjectId,
    event::NormalizedFrame,
    symbolication::{
        BackendSymbolicationResult, BackendSymbolicationStatus, RawStacktrace, RawTraceOrigin,
        SymbolicatedFrame, SymbolicatedStacktrace, SymbolicationDiagnosticCode, SymbolicationKind,
        SymbolicationRequest,
    },
};
use metric_ports::{PortFuture, SymbolicationBackend, SymbolicationBackendError};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct PrivateSourceSigner {
    current: Arc<[u8]>,
    previous: Option<Arc<[u8]>>,
}

impl PrivateSourceSigner {
    pub fn new(current: Vec<u8>, previous: Option<Vec<u8>>) -> Result<Self, &'static str> {
        if current.len() < 32 || previous.as_ref().is_some_and(|value| value.len() < 32) {
            return Err("private source HMAC keys must contain at least 32 bytes");
        }
        Ok(Self {
            current: current.into(),
            previous: previous.map(Into::into),
        })
    }

    #[must_use]
    pub fn token(&self, project_id: ProjectId) -> String {
        token_with(&self.current, project_id, PrivateSourceKind::DebugFile)
    }

    #[must_use]
    pub fn verify(&self, project_id: ProjectId, token: &str) -> bool {
        verify_with(
            &self.current,
            project_id,
            token,
            PrivateSourceKind::DebugFile,
        ) || self
            .previous
            .as_ref()
            .is_some_and(|key| verify_with(key, project_id, token, PrivateSourceKind::DebugFile))
    }

    #[must_use]
    pub fn artifact_token(&self, project_id: ProjectId) -> String {
        token_with(&self.current, project_id, PrivateSourceKind::ArtifactBundle)
    }

    #[must_use]
    pub fn verify_artifact(&self, project_id: ProjectId, token: &str) -> bool {
        verify_with(
            &self.current,
            project_id,
            token,
            PrivateSourceKind::ArtifactBundle,
        ) || self.previous.as_ref().is_some_and(|key| {
            verify_with(key, project_id, token, PrivateSourceKind::ArtifactBundle)
        })
    }
}

#[derive(Debug, Clone)]
pub struct ExternalSymbolicatorConfig {
    pub endpoint: Url,
    pub callback_base_url: Url,
    pub request_timeout: Duration,
    pub maximum_concurrency: usize,
    pub circuit_failure_threshold: u32,
    pub circuit_cooldown: Duration,
    pub maximum_response_bytes: usize,
}

pub struct ExternalSymbolicator {
    client: Client,
    config: ExternalSymbolicatorConfig,
    signer: PrivateSourceSigner,
    concurrency: Semaphore,
    consecutive_failures: AtomicU32,
    circuit_open_until_millis: AtomicU64,
}

impl ExternalSymbolicator {
    pub fn new(
        config: ExternalSymbolicatorConfig,
        signer: PrivateSourceSigner,
    ) -> Result<Self, &'static str> {
        if config.endpoint.scheme() != "http" && config.endpoint.scheme() != "https"
            || config.callback_base_url.scheme() != "http"
                && config.callback_base_url.scheme() != "https"
            || config.request_timeout.is_zero()
            || config.request_timeout > Duration::from_secs(120)
            || !(1..=1024).contains(&config.maximum_concurrency)
            || !(1..=1000).contains(&config.circuit_failure_threshold)
            || config.circuit_cooldown.is_zero()
            || !(1024..=16 * 1024 * 1024).contains(&config.maximum_response_bytes)
        {
            return Err("external Symbolicator configuration is invalid");
        }
        let client = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(|_| "external Symbolicator HTTP client is invalid")?;
        let maximum_concurrency = config.maximum_concurrency;
        Ok(Self {
            client,
            config,
            signer,
            concurrency: Semaphore::new(maximum_concurrency),
            consecutive_failures: AtomicU32::new(0),
            circuit_open_until_millis: AtomicU64::new(0),
        })
    }

    async fn execute(
        &self,
        request: SymbolicationRequest,
    ) -> Result<BackendSymbolicationResult, SymbolicationBackendError> {
        if now_millis() < self.circuit_open_until_millis.load(Ordering::Acquire) {
            return Err(SymbolicationBackendError::Unavailable);
        }
        let _permit = self
            .concurrency
            .acquire()
            .await
            .map_err(|_| SymbolicationBackendError::Unavailable)?;
        let project_id = request.project_id;
        let origins = request
            .traces
            .iter()
            .map(|trace| trace.origin)
            .collect::<Vec<_>>();
        let frame_counts = request
            .traces
            .iter()
            .map(|trace| trace.frames.len())
            .collect::<Vec<_>>();
        let debug_revision = request.debug_file_revision;
        let artifact_revision = request.artifact_revision;
        let mut js_plans = Vec::new();
        let (mut endpoint, payload) = match request.kind {
            SymbolicationKind::JavaScript => {
                let source_url = self
                    .config
                    .callback_base_url
                    .join(&format!(
                        "internal/symbolicator/projects/{}/artifact-lookup/?revision={}",
                        request.project_id.get(),
                        request.artifact_revision,
                    ))
                    .map_err(|_| SymbolicationBackendError::MalformedResponse)?;
                let mut endpoint = self.config.endpoint.clone();
                if endpoint.path().ends_with("/symbolicate") {
                    let path = endpoint.path().trim_end_matches("/symbolicate").to_owned()
                        + "/symbolicate-js";
                    endpoint.set_path(&path);
                } else {
                    endpoint.set_path("/symbolicate-js");
                }
                let (wire, plans) = JsWireRequest::from_domain(
                    request,
                    WireSource {
                        id: format!("private-js-project-{}", project_id.get()),
                        ty: "sentry",
                        url: source_url.as_str().to_owned(),
                        token: self.signer.artifact_token(project_id),
                    },
                );
                js_plans = plans;
                (
                    endpoint,
                    serde_json::to_value(wire)
                        .map_err(|_| SymbolicationBackendError::MalformedResponse)?,
                )
            }
            SymbolicationKind::Native => {
                let source_url = self
                    .config
                    .callback_base_url
                    .join(&format!(
                        "internal/symbolicator/projects/{}/debug-files/?revision={}",
                        request.project_id.get(),
                        request.debug_file_revision,
                    ))
                    .map_err(|_| SymbolicationBackendError::MalformedResponse)?;
                (
                    self.config.endpoint.clone(),
                    serde_json::to_value(WireRequest::from_domain(
                        request,
                        WireSource {
                            id: format!("private-project-{}", project_id.get()),
                            ty: "sentry",
                            url: source_url.as_str().to_owned(),
                            token: self.signer.token(project_id),
                        },
                    ))
                    .map_err(|_| SymbolicationBackendError::MalformedResponse)?,
                )
            }
            SymbolicationKind::NotRequired => {
                return Err(SymbolicationBackendError::MalformedResponse);
            }
        };
        // Both successful and negative caches must be private to a project and
        // invalidated when a newly uploaded artifact changes its source index.
        endpoint.query_pairs_mut().append_pair(
            "scope",
            &format!(
                "project-{}-debug-{debug_revision}-artifacts-{artifact_revision}",
                project_id.get()
            ),
        );
        let javascript = request_kind(&payload) != "native";
        let response = match self.client.post(endpoint).json(&payload).send().await {
            Ok(response) => response,
            Err(error) => {
                self.record_failure();
                return Err(classify_reqwest(error));
            }
        };
        if !response.status().is_success() {
            self.record_failure();
            return Err(if response.status().is_server_error() {
                SymbolicationBackendError::Unavailable
            } else {
                SymbolicationBackendError::MalformedResponse
            });
        }
        if response.content_length().is_some_and(|size| {
            usize::try_from(size).map_or(true, |size| size > self.config.maximum_response_bytes)
        }) {
            self.record_failure();
            return Err(SymbolicationBackendError::MalformedResponse);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::with_capacity(self.config.maximum_response_bytes.min(64 * 1024));
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    self.record_failure();
                    return Err(classify_reqwest(error));
                }
            };
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|size| size > self.config.maximum_response_bytes)
            {
                self.record_failure();
                return Err(SymbolicationBackendError::MalformedResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let result = if javascript {
            serde_json::from_slice::<JsWireResponse>(&bytes)
                .map_err(|_| SymbolicationBackendError::MalformedResponse)
                .and_then(|wire| wire.into_domain(js_plans))
        } else {
            serde_json::from_slice::<WireResponse>(&bytes)
                .map_err(|_| SymbolicationBackendError::MalformedResponse)
                .and_then(|wire| wire.into_domain(&origins, &frame_counts))
        }
        .inspect_err(|_| self.record_failure())?;
        self.consecutive_failures.store(0, Ordering::Release);
        Ok(result)
    }

    fn record_failure(&self) {
        let failures = self
            .consecutive_failures
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        if failures >= self.config.circuit_failure_threshold {
            let cooldown =
                u64::try_from(self.config.circuit_cooldown.as_millis()).unwrap_or(u64::MAX);
            self.circuit_open_until_millis
                .store(now_millis().saturating_add(cooldown), Ordering::Release);
            self.consecutive_failures.store(0, Ordering::Release);
        }
    }
}

fn request_kind(payload: &serde_json::Value) -> &str {
    payload
        .get("platform")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

impl SymbolicationBackend for ExternalSymbolicator {
    fn symbolicate(
        &self,
        request: SymbolicationRequest,
    ) -> PortFuture<'_, Result<BackendSymbolicationResult, SymbolicationBackendError>> {
        Box::pin(self.execute(request))
    }
}

#[derive(Serialize)]
struct WireRequest {
    platform: &'static str,
    sources: [WireSource; 1],
    stacktraces: Vec<WireTrace>,
    modules: Vec<WireModule>,
    options: WireOptions,
}

impl WireRequest {
    fn from_domain(request: SymbolicationRequest, source: WireSource) -> Self {
        Self {
            platform: "native",
            sources: [source],
            stacktraces: request
                .traces
                .into_iter()
                .map(|trace| WireTrace {
                    frames: trace
                        .frames
                        .into_iter()
                        .map(|frame| WireFrame {
                            instruction_addr: frame.instruction_address.map(Into::into),
                            addr_mode: "abs",
                        })
                        .collect(),
                })
                .collect(),
            modules: request
                .modules
                .into_iter()
                .map(|module| WireModule {
                    ty: module.kind.map(Into::into),
                    debug_id: module.debug_id.map(Into::into),
                    code_id: module.code_id.map(Into::into),
                    debug_file: module.code_file.map(Into::into),
                    image_addr: module.image_address.map(Into::into),
                    image_size: module.image_size,
                })
                .collect(),
            options: WireOptions {
                dif_candidates: true,
                apply_source_context: true,
                frame_order: "caller_first",
            },
        }
    }
}

#[derive(Serialize)]
struct WireSource {
    id: String,
    #[serde(rename = "type")]
    ty: &'static str,
    url: String,
    token: String,
}

#[derive(Serialize)]
struct WireTrace {
    frames: Vec<WireFrame>,
}

#[derive(Serialize)]
struct WireFrame {
    instruction_addr: Option<String>,
    addr_mode: &'static str,
}

#[derive(Serialize)]
struct WireModule {
    #[serde(rename = "type")]
    ty: Option<String>,
    debug_id: Option<String>,
    code_id: Option<String>,
    debug_file: Option<String>,
    image_addr: Option<String>,
    image_size: Option<u64>,
}

#[derive(Serialize)]
struct WireOptions {
    dif_candidates: bool,
    apply_source_context: bool,
    frame_order: &'static str,
}

#[derive(Serialize)]
struct JsWireRequest {
    platform: &'static str,
    source: WireSource,
    stacktraces: Vec<JsWireTrace>,
    modules: Vec<JsWireModule>,
    release: Option<Box<str>>,
    dist: Option<Box<str>>,
    scraping: JsScraping,
    options: JsWireOptions,
}

impl JsWireRequest {
    fn from_domain(request: SymbolicationRequest, source: WireSource) -> (Self, Vec<JsTracePlan>) {
        let mut stacktraces = Vec::new();
        let plans = request
            .traces
            .into_iter()
            .map(|trace| JsTracePlan::cut(trace, &mut stacktraces))
            .collect();
        let wire = Self {
            platform: "javascript",
            source,
            stacktraces,
            modules: request
                .modules
                .into_iter()
                .filter_map(|module| {
                    Some(JsWireModule {
                        code_file: module.code_file?,
                        debug_id: module.debug_id?,
                        // Symbolicator's `JsModuleType` has a single variant.
                        ty: "sourcemap",
                    })
                })
                .collect(),
            release: request.release,
            dist: request.dist,
            scraping: JsScraping {
                enabled: false,
                allowed_origins: Vec::new(),
                headers: BTreeMap::new(),
            },
            options: JsWireOptions {
                apply_source_context: true,
                frame_order: "caller_first",
            },
        };
        (wire, plans)
    }
}

#[derive(Serialize)]
struct JsWireTrace {
    frames: Vec<JsWireFrame>,
}

/// How one raw trace was sent.
///
/// Symbolicator names a frame after its caller's call site, so the frames on either
/// side of one that cannot be sent must not look adjacent. Each run of sendable
/// frames goes as its own stacktrace. Frames left out get no derived frame, and
/// grouping keeps their raw identity.
struct JsTracePlan {
    origin: RawTraceOrigin,
    /// Raw frame indexes of each sent run, in request order.
    runs: Vec<Vec<usize>>,
    unsent: usize,
}

impl JsTracePlan {
    fn cut(trace: RawStacktrace, stacktraces: &mut Vec<JsWireTrace>) -> Self {
        let mut plan = Self {
            origin: trace.origin,
            runs: Vec::new(),
            unsent: 0,
        };
        let mut run = (Vec::new(), Vec::new());
        for (index, frame) in trace.frames.iter().enumerate() {
            if let Some(wire) = JsWireFrame::from_domain(frame) {
                run.0.push(index);
                run.1.push(wire);
            } else {
                plan.close(&mut run, stacktraces);
                plan.unsent += 1;
            }
        }
        plan.close(&mut run, stacktraces);
        plan
    }

    fn close(
        &mut self,
        run: &mut (Vec<usize>, Vec<JsWireFrame>),
        stacktraces: &mut Vec<JsWireTrace>,
    ) {
        if !run.0.is_empty() {
            self.runs.push(std::mem::take(&mut run.0));
            stacktraces.push(JsWireTrace {
                frames: std::mem::take(&mut run.1),
            });
        }
    }
}

#[derive(Serialize)]
struct JsWireFrame {
    function: Option<Box<str>>,
    module: Option<Box<str>>,
    filename: Option<Box<str>>,
    abs_path: Box<str>,
    lineno: u32,
    colno: Option<u64>,
    in_app: Option<bool>,
}

impl JsWireFrame {
    /// Symbolicator rejects the whole request when a frame lacks a path or a line.
    /// Sentry JavaScript SDKs send `filename` without `abs_path`, and frames of
    /// native code have no line; such frames are not sent.
    fn from_domain(frame: &NormalizedFrame) -> Option<Self> {
        Some(Self {
            function: frame.function.clone(),
            module: frame.module.clone(),
            filename: frame.filename.clone(),
            abs_path: frame
                .absolute_path
                .clone()
                .or_else(|| frame.filename.clone())?,
            lineno: frame.line.and_then(|line| u32::try_from(line).ok())?,
            colno: frame.column,
            in_app: frame.in_app,
        })
    }
}

#[derive(Serialize)]
struct JsWireModule {
    code_file: Box<str>,
    debug_id: Box<str>,
    #[serde(rename = "type")]
    ty: &'static str,
}

/// Symbolicator requires every field even when scraping is off, and a missing
/// `scraping` object would mean scraping from any origin.
#[derive(Serialize)]
struct JsScraping {
    enabled: bool,
    allowed_origins: Vec<Box<str>>,
    headers: BTreeMap<Box<str>, Box<str>>,
}

#[derive(Serialize)]
struct JsWireOptions {
    apply_source_context: bool,
    frame_order: &'static str,
}

#[derive(Deserialize)]
struct JsWireResponse {
    status: String,
    #[serde(default)]
    stacktraces: Vec<JsResponseTrace>,
    #[serde(default)]
    raw_stacktraces: Vec<serde_json::Value>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

impl JsWireResponse {
    fn into_domain(
        self,
        plans: Vec<JsTracePlan>,
    ) -> Result<BackendSymbolicationResult, SymbolicationBackendError> {
        completed_status(&self.status)?;
        let runs = plans.iter().map(|plan| plan.runs.len()).sum::<usize>();
        if self.stacktraces.len() != runs
            || (!self.raw_stacktraces.is_empty() && self.raw_stacktraces.len() != runs)
        {
            return Err(SymbolicationBackendError::MalformedResponse);
        }
        let mut responses = self.stacktraces.into_iter();
        let mut had_missing = false;
        let mut derived = Vec::with_capacity(plans.len());
        for plan in plans {
            had_missing |= plan.unsent > 0;
            // A trace with nothing sent gets no derived trace, so it is grouped by its
            // raw frames; an empty one would make grouping skip it.
            if plan.runs.is_empty() && plan.unsent > 0 {
                continue;
            }
            let mut frames = Vec::new();
            for indexes in plan.runs {
                let response = responses
                    .next()
                    .ok_or(SymbolicationBackendError::MalformedResponse)?;
                if response.frames.len() != indexes.len() {
                    return Err(SymbolicationBackendError::MalformedResponse);
                }
                frames.extend(response.frames.into_iter().zip(indexes).filter_map(
                    |(frame, original_index)| {
                        if !frame.data.symbolicated {
                            had_missing = true;
                            return None;
                        }
                        Some(SymbolicatedFrame {
                            original_index,
                            function: frame.function.map(Into::into),
                            filename: frame.filename.map(Into::into),
                            module: frame.module.map(Into::into),
                            line: frame.lineno,
                            column: frame.colno,
                            context_line: frame.context_line.map(Into::into),
                            pre_context: frame.pre_context.into_iter().map(Into::into).collect(),
                            post_context: frame.post_context.into_iter().map(Into::into).collect(),
                        })
                    },
                ));
            }
            derived.push(SymbolicatedStacktrace {
                origin: plan.origin,
                frames,
            });
        }
        let has_frames = derived.iter().any(|trace| !trace.frames.is_empty());
        let has_errors = had_missing || !self.errors.is_empty();
        let status = match (has_frames, has_errors) {
            (true, true) => BackendSymbolicationStatus::Partial,
            (true, false) => BackendSymbolicationStatus::Complete,
            (false, _) => BackendSymbolicationStatus::Missing,
        };
        Ok(BackendSymbolicationResult {
            status,
            derived,
            missing_debug_ids: Vec::new(),
            diagnostics: (status == BackendSymbolicationStatus::Partial)
                .then_some(SymbolicationDiagnosticCode::BackendPartial)
                .into_iter()
                .collect(),
        })
    }
}

#[derive(Deserialize)]
struct JsResponseTrace {
    #[serde(default)]
    frames: Vec<JsResponseFrame>,
}

#[derive(Deserialize)]
struct JsResponseFrame {
    #[serde(default)]
    data: JsResponseFrameData,
    function: Option<String>,
    filename: Option<String>,
    module: Option<String>,
    lineno: Option<u64>,
    colno: Option<u64>,
    context_line: Option<String>,
    #[serde(default)]
    pre_context: Vec<String>,
    #[serde(default)]
    post_context: Vec<String>,
}

#[derive(Default, Deserialize)]
struct JsResponseFrameData {
    #[serde(default)]
    symbolicated: bool,
}

fn completed_status(status: &str) -> Result<(), SymbolicationBackendError> {
    match status {
        "completed" => Ok(()),
        "pending" => Err(SymbolicationBackendError::Timeout),
        _ => Err(SymbolicationBackendError::MalformedResponse),
    }
}

#[derive(Deserialize)]
struct WireResponse {
    status: String,
    #[serde(default)]
    stacktraces: Vec<WireSymbolicatedTrace>,
    #[serde(default)]
    missing_debug_ids: Vec<String>,
    #[serde(default)]
    modules: Vec<WireResponseModule>,
}

#[derive(Deserialize)]
struct WireResponseModule {
    debug_status: Option<String>,
    debug_id: Option<String>,
}

impl WireResponse {
    fn into_domain(
        self,
        origins: &[RawTraceOrigin],
        frame_counts: &[usize],
    ) -> Result<BackendSymbolicationResult, SymbolicationBackendError> {
        completed_status(&self.status)?;
        if self.stacktraces.len() != origins.len() || origins.len() != frame_counts.len() {
            return Err(SymbolicationBackendError::MalformedResponse);
        }
        let mut had_missing = false;
        let derived = self
            .stacktraces
            .into_iter()
            .zip(origins.iter().copied())
            .zip(frame_counts.iter().copied())
            .map(|((trace, origin), count)| trace.into_domain(origin, count, &mut had_missing))
            .collect::<Result<Vec<_>, _>>()?;
        let has_symbols = derived.iter().any(|trace| !trace.frames.is_empty());
        let status = if had_missing && has_symbols {
            BackendSymbolicationStatus::Partial
        } else if had_missing || !has_symbols {
            BackendSymbolicationStatus::Missing
        } else {
            BackendSymbolicationStatus::Complete
        };
        let mut missing_debug_ids = self.missing_debug_ids;
        missing_debug_ids.extend(self.modules.into_iter().filter_map(|module| {
            (module.debug_status.as_deref() == Some("missing"))
                .then_some(module.debug_id)
                .flatten()
        }));
        missing_debug_ids.sort_unstable();
        missing_debug_ids.dedup();
        Ok(BackendSymbolicationResult {
            status,
            derived,
            missing_debug_ids: missing_debug_ids
                .into_iter()
                .map(String::into_boxed_str)
                .collect(),
            diagnostics: if status == BackendSymbolicationStatus::Partial {
                vec![SymbolicationDiagnosticCode::BackendPartial]
            } else {
                Vec::new()
            },
        })
    }
}

#[derive(Deserialize)]
struct WireSymbolicatedTrace {
    #[serde(default)]
    origin: Option<String>,
    frames: Vec<WireSymbolicatedFrame>,
}

impl WireSymbolicatedTrace {
    fn into_domain(
        self,
        expected_origin: RawTraceOrigin,
        original_frame_count: usize,
        had_missing: &mut bool,
    ) -> Result<SymbolicatedStacktrace, SymbolicationBackendError> {
        if let Some(origin) = &self.origin
            && parse_origin(origin)? != expected_origin
        {
            return Err(SymbolicationBackendError::MalformedResponse);
        }
        let mut frames = Vec::new();
        for frame in self.frames {
            // 26.6.0 returns frames in caller-first order, but original_index is
            // assigned after its internal reversal to callee-first order.
            let original_index = frame
                .original_index
                .checked_add(1)
                .and_then(|index| original_frame_count.checked_sub(index))
                .ok_or(SymbolicationBackendError::MalformedResponse)?;
            if frame.status.as_deref() != Some("symbolicated") {
                *had_missing = true;
                continue;
            }
            frames.push(SymbolicatedFrame {
                original_index,
                function: frame.function.map(String::into_boxed_str),
                filename: frame
                    .abs_path
                    .or(frame.filename)
                    .map(String::into_boxed_str),
                module: frame.module.map(String::into_boxed_str),
                line: frame.line,
                column: frame.column,
                context_line: frame.context_line.map(Into::into),
                pre_context: frame.pre_context.into_iter().map(Into::into).collect(),
                post_context: frame.post_context.into_iter().map(Into::into).collect(),
            });
        }
        Ok(SymbolicatedStacktrace {
            origin: expected_origin,
            frames,
        })
    }
}

#[derive(Deserialize)]
struct WireSymbolicatedFrame {
    #[serde(default)]
    status: Option<String>,
    original_index: usize,
    function: Option<String>,
    filename: Option<String>,
    abs_path: Option<String>,
    module: Option<String>,
    #[serde(rename = "lineno")]
    line: Option<u64>,
    #[serde(rename = "colno")]
    column: Option<u64>,
    context_line: Option<String>,
    #[serde(default)]
    pre_context: Vec<String>,
    #[serde(default)]
    post_context: Vec<String>,
}

fn parse_origin(value: &str) -> Result<RawTraceOrigin, SymbolicationBackendError> {
    if value == "event" {
        return Ok(RawTraceOrigin::Event);
    }
    let (kind, index) = value
        .split_once(':')
        .ok_or(SymbolicationBackendError::MalformedResponse)?;
    let index = index
        .parse()
        .map_err(|_| SymbolicationBackendError::MalformedResponse)?;
    match kind {
        "exception" => Ok(RawTraceOrigin::Exception { index }),
        "exception_raw" => Ok(RawTraceOrigin::ExceptionRaw { index }),
        _ => Err(SymbolicationBackendError::MalformedResponse),
    }
}

#[derive(Clone, Copy)]
enum PrivateSourceKind {
    DebugFile,
    ArtifactBundle,
}

impl PrivateSourceKind {
    const fn domain(self) -> &'static [u8] {
        match self {
            Self::DebugFile => b"metric/symbolicator-debug-source/v1",
            Self::ArtifactBundle => b"metric/symbolicator-artifact-source/v1",
        }
    }
}

fn token_with(key: &[u8], project_id: ProjectId, kind: PrivateSourceKind) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts arbitrary key sizes");
    mac.update(kind.domain());
    mac.update(&project_id.get().to_be_bytes());
    format!(
        "sym1.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

fn verify_with(key: &[u8], project_id: ProjectId, token: &str, kind: PrivateSourceKind) -> bool {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let Some(encoded) = token.strip_prefix("sym1.") else {
        return false;
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts arbitrary key sizes");
    mac.update(kind.domain());
    mac.update(&project_id.get().to_be_bytes());
    mac.verify_slice(&bytes).is_ok()
}

fn classify_reqwest(error: reqwest::Error) -> SymbolicationBackendError {
    if error.is_timeout() {
        SymbolicationBackendError::Timeout
    } else {
        SymbolicationBackendError::Unavailable
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use metric_domain::{
        event::NormalizedFrame,
        symbolication::{RawStacktrace, SymbolicationKind, SymbolicationModule},
    };
    use std::collections::BTreeMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn transport_failures_are_bounded_and_never_report_symbols() {
        for (status, body, delay, expected) in [
            (503, "", false, SymbolicationBackendError::Unavailable),
            (422, "", false, SymbolicationBackendError::MalformedResponse),
            (
                200,
                "not JSON",
                false,
                SymbolicationBackendError::MalformedResponse,
            ),
            (
                200,
                r#"{"status":"completed","stacktraces":{}}"#,
                false,
                SymbolicationBackendError::MalformedResponse,
            ),
            (
                200,
                r#"{"status":"pending"}"#,
                false,
                SymbolicationBackendError::Timeout,
            ),
            (200, "", true, SymbolicationBackendError::Timeout),
            (
                200,
                &"x".repeat(2048),
                false,
                SymbolicationBackendError::MalformedResponse,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let body = body.to_owned();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 8192];
                let _ = stream.read(&mut request).await;
                if delay {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                let _ = stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
            let adapter = ExternalSymbolicator::new(
                ExternalSymbolicatorConfig {
                    endpoint: Url::parse(&format!("http://{address}/symbolicate")).unwrap(),
                    callback_base_url: Url::parse("http://127.0.0.1:4001/").unwrap(),
                    request_timeout: Duration::from_millis(150),
                    maximum_concurrency: 1,
                    circuit_failure_threshold: 2,
                    circuit_cooldown: Duration::from_secs(1),
                    maximum_response_bytes: 1024,
                },
                PrivateSourceSigner::new(vec![7; 32], None).unwrap(),
            )
            .unwrap();
            let request = SymbolicationRequest {
                project_id: ProjectId::new(7).unwrap(),
                debug_file_revision: 0,
                artifact_revision: 0,
                kind: SymbolicationKind::Native,
                traces: Vec::new(),
                modules: Vec::new(),
                release: None,
                dist: None,
            };
            assert_eq!(adapter.symbolicate(request).await, Err(expected));
            server.await.unwrap();
        }
    }

    #[test]
    fn native_frame_status_is_authoritative_and_preserves_inline_indices() {
        let response: WireResponse = serde_json::from_value(serde_json::json!({
            "status": "completed", "stacktraces": [{"frames": [
                {"status": "symbolicated", "original_index": 1, "function": "inline", "abs_path": "src/main.rs", "filename": "main.rs", "lineno": 42,
                 "pre_context": ["before();"], "context_line": "", "post_context": ["after();"]},
                {"status": "symbolicated", "original_index": 1, "function": "caller"},
                {"status": "missing", "original_index": 0, "function": "untrusted_raw_name"}
            ]}], "modules": [{"debug_status": "missing", "debug_id": "missing-module"}]
        })).unwrap();
        let result = response
            .into_domain(&[RawTraceOrigin::Exception { index: 2 }], &[2])
            .unwrap();
        assert_eq!(result.status, BackendSymbolicationStatus::Partial);
        assert_eq!(
            result.derived[0].origin,
            RawTraceOrigin::Exception { index: 2 }
        );
        assert_eq!(result.derived[0].frames.len(), 2);
        assert_eq!(
            result.derived[0].frames[0].context_line.as_deref(),
            Some("")
        );
        assert_eq!(
            result.derived[0].frames[0].pre_context[0].as_ref(),
            "before();"
        );
        assert_eq!(
            result.derived[0].frames[0].post_context[0].as_ref(),
            "after();"
        );
        assert_eq!(
            result.missing_debug_ids,
            vec![Box::<str>::from("missing-module")]
        );
        assert_eq!(
            result.derived[0].frames[0].filename.as_deref(),
            Some("src/main.rs")
        );
        assert!(
            result.derived[0]
                .frames
                .iter()
                .all(|frame| frame.original_index == 0)
        );
    }

    fn javascript_plans(frame_count: usize) -> Vec<JsTracePlan> {
        vec![JsTracePlan {
            origin: RawTraceOrigin::Event,
            runs: vec![(0..frame_count).collect()],
            unsent: 0,
        }]
    }

    #[test]
    fn javascript_requires_completed_response_and_explicit_symbolication_flags() {
        for (flags, expected, indexes) in [
            (
                vec![false, false],
                BackendSymbolicationStatus::Missing,
                vec![],
            ),
            (
                vec![true, false],
                BackendSymbolicationStatus::Partial,
                vec![0],
            ),
            (
                vec![false, true],
                BackendSymbolicationStatus::Partial,
                vec![1],
            ),
            (
                vec![true, true],
                BackendSymbolicationStatus::Complete,
                vec![0, 1],
            ),
        ] {
            let frames = flags.into_iter().map(|symbolicated| serde_json::json!({
                "function": "present_even_without_symbols", "data": {"symbolicated": symbolicated}
            })).collect::<Vec<_>>();
            let response: JsWireResponse = serde_json::from_value(serde_json::json!({
                "status": "completed", "stacktraces": [{"frames": frames}]
            }))
            .unwrap();
            let result = response.into_domain(javascript_plans(2)).unwrap();
            assert_eq!(result.status, expected);
            assert_eq!(
                result.derived[0]
                    .frames
                    .iter()
                    .map(|frame| frame.original_index)
                    .collect::<Vec<_>>(),
                indexes
            );
        }
        assert_eq!(
            completed_status("pending"),
            Err(SymbolicationBackendError::Timeout)
        );
        assert_eq!(
            completed_status("complete"),
            Err(SymbolicationBackendError::MalformedResponse)
        );
        let missing_flag: JsWireResponse = serde_json::from_value(serde_json::json!({
            "status": "completed", "stacktraces": [{"frames": [{"function": "raw"}]}]
        }))
        .unwrap();
        assert_eq!(
            missing_flag
                .into_domain(javascript_plans(1))
                .unwrap()
                .status,
            BackendSymbolicationStatus::Missing
        );
        let wrong_count: JsWireResponse =
            serde_json::from_value(serde_json::json!({"status": "completed", "stacktraces": []}))
                .unwrap();
        assert_eq!(
            wrong_count.into_domain(javascript_plans(1)),
            Err(SymbolicationBackendError::MalformedResponse)
        );
    }

    #[test]
    fn javascript_preserves_original_source_context_and_accepts_older_responses() {
        for context in [
            serde_json::json!({}),
            serde_json::json!({
                "pre_context": ["const value = 42;"], "context_line": "throw new Error();", "post_context": ["}"]
            }),
        ] {
            let mut frame = serde_json::json!({"data": {"symbolicated": true}, "function": "fail", "lineno": 42});
            frame
                .as_object_mut()
                .unwrap()
                .extend(context.as_object().unwrap().clone());
            let response: JsWireResponse = serde_json::from_value(serde_json::json!({
                "status": "completed", "stacktraces": [{"frames": [frame]}]
            }))
            .unwrap();
            let result = response.into_domain(javascript_plans(1)).unwrap();
            let frame = &result.derived[0].frames[0];
            assert_eq!(
                frame.context_line.as_deref(),
                context["context_line"].as_str()
            );
            assert_eq!(
                frame.pre_context.len(),
                context["pre_context"].as_array().map_or(0, Vec::len)
            );
            assert_eq!(
                frame.post_context.len(),
                context["post_context"].as_array().map_or(0, Vec::len)
            );
        }
    }

    #[test]
    fn javascript_gaps_and_unresolved_frames_keep_source_context_on_original_indexes() {
        let mut wire_traces = Vec::new();
        let plan = JsTracePlan::cut(
            RawStacktrace {
                origin: RawTraceOrigin::Exception { index: 1 },
                frames: vec![
                    javascript_frame(Some("app.min.js"), None, Some(1), "a"),
                    javascript_frame(None, None, None, "native"),
                    javascript_frame(Some("app.min.js"), None, Some(3), "c"),
                ],
            },
            &mut wire_traces,
        );
        assert_eq!(wire_traces.len(), 2);
        let response: JsWireResponse = serde_json::from_value(serde_json::json!({
            "status": "completed",
            "stacktraces": [
                {"frames": [{"function": "raw_name", "data": {"symbolicated": false}}]},
                {"frames": [{"function": "fail", "data": {"symbolicated": true},
                    "pre_context": ["before();"], "context_line": "throw error;", "post_context": ["after();"]}]}
            ]
        })).unwrap();
        let result = response.into_domain(vec![plan]).unwrap();
        assert_eq!(result.status, BackendSymbolicationStatus::Partial);
        assert_eq!(
            result.diagnostics,
            vec![SymbolicationDiagnosticCode::BackendPartial]
        );
        assert_eq!(
            result.derived[0].origin,
            RawTraceOrigin::Exception { index: 1 }
        );
        assert_eq!(result.derived[0].frames.len(), 1);
        let frame = &result.derived[0].frames[0];
        assert_eq!(frame.original_index, 2);
        assert_eq!(frame.context_line.as_deref(), Some("throw error;"));
        assert_eq!(frame.pre_context[0].as_ref(), "before();");
        assert_eq!(frame.post_context[0].as_ref(), "after();");
    }

    #[test]
    fn native_request_uses_stacktraces_and_numeric_image_size() {
        let wire = WireRequest::from_domain(
            SymbolicationRequest {
                project_id: ProjectId::new(7).unwrap(),
                debug_file_revision: 1,
                artifact_revision: 0,
                kind: SymbolicationKind::Native,
                traces: Vec::new(),
                modules: vec![SymbolicationModule {
                    kind: Some("elf".into()),
                    debug_id: None,
                    code_id: None,
                    code_file: Some("native".into()),
                    image_address: Some("0x0".into()),
                    image_size: Some(8192),
                }],
                release: None,
                dist: None,
            },
            WireSource {
                id: "test".into(),
                ty: "sentry",
                url: "http://localhost/".into(),
                token: "synthetic".into(),
            },
        );
        let value = serde_json::to_value(wire).unwrap();
        assert!(value.get("threads").is_none());
        assert!(value["stacktraces"].is_array());
        assert_eq!(value["modules"][0]["image_size"], 8192);
    }

    #[test]
    fn private_source_token_is_project_scoped_and_rotatable() {
        let signer = PrivateSourceSigner::new(vec![1; 32], Some(vec![2; 32])).unwrap();
        let project = ProjectId::new(7).unwrap();
        let token = signer.token(project);
        assert!(signer.verify(project, &token));
        assert!(!signer.verify(ProjectId::new(8).unwrap(), &token));
        assert!(!signer.verify(project, "sym1.invalid"));
        assert!(!signer.verify_artifact(project, &token));
        assert_ne!(token, signer.artifact_token(project));
    }

    #[tokio::test]
    async fn pinned_javascript_contract_uses_artifact_revision_and_maps_frames() {
        let pinned: serde_json::Value = serde_json::from_str(include_str!(
            "../../../sdk-tests/symbolicator/26.6.0-javascript-contract.json"
        ))
        .unwrap();
        let response_body = serde_json::to_string(&pinned["response"]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (head, body) = read_request(&mut stream).await;
            assert!(
                head.starts_with("POST /symbolicate-js?scope=project-7-debug-42-artifacts-43 ")
            );
            assert!(body.to_string().contains("artifact-lookup/?revision=43"));
            assert_eq!(
                body["scraping"],
                serde_json::json!({ "enabled": false, "allowed_origins": [], "headers": {} })
            );
            // Symbolicator 26.6.0 requires `abs_path` and `lineno` on every frame and
            // accepts only the `sourcemap` module type; anything else fails the request.
            let frame = &body["stacktraces"][0]["frames"][0];
            assert_eq!(
                frame["abs_path"],
                "https://example.invalid/static/app.min.js"
            );
            assert_eq!(frame["lineno"], 1);
            assert_eq!(body["modules"][0]["type"], "sourcemap");
            respond(&mut stream, &response_body).await;
        });
        let adapter = javascript_adapter(address);
        let frame = javascript_frame(
            Some("app.min.js"),
            Some("https://example.invalid/static/app.min.js"),
            Some(1),
            "fail",
        );
        let output = adapter
            .symbolicate(SymbolicationRequest {
                project_id: ProjectId::new(7).unwrap(),
                debug_file_revision: 42,
                artifact_revision: 43,
                kind: SymbolicationKind::JavaScript,
                traces: vec![RawStacktrace {
                    origin: RawTraceOrigin::Event,
                    frames: vec![frame],
                }],
                modules: vec![SymbolicationModule {
                    kind: Some("sourcemap".into()),
                    debug_id: Some("67e9247c-814e-392b-a027-dbde6748fcbf".into()),
                    code_id: None,
                    code_file: Some("https://example.invalid/static/app.min.js".into()),
                    image_address: None,
                    image_size: None,
                }],
                release: Some("metric-phase18@1.0.0".into()),
                dist: Some("windows".into()),
            })
            .await
            .unwrap();
        assert_eq!(output.status, BackendSymbolicationStatus::Complete);
        assert_eq!(
            output.derived[0].frames[0].function.as_deref(),
            Some("fail")
        );
        assert_eq!(output.derived[0].frames[0].line, Some(2));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn javascript_frames_symbolicator_cannot_take_stay_raw() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = serve_once(
            listener,
            |body| {
                // The frame without a line splits the run, so symbolicator cannot
                // name the frame after it from the call site in the frame before.
                let stacktraces = body["stacktraces"].as_array().unwrap();
                assert_eq!(stacktraces.len(), 2);
                assert_eq!(stacktraces[0]["frames"].as_array().unwrap().len(), 1);
                // Sentry JavaScript SDKs send `filename` without `abs_path`.
                assert_eq!(stacktraces[0]["frames"][0]["abs_path"], "app.min.js");
                assert_eq!(
                    stacktraces[1]["frames"][0]["abs_path"],
                    "https://example.invalid/static/vendor.min.js"
                );
            },
            r#"{"status":"completed","stacktraces":[
                {"frames":[{"data":{"symbolicated":true},"function":"fail","filename":"app.js","lineno":6,"colno":9}]},
                {"frames":[{"data":{"symbolicated":true},"function":"boot","filename":"vendor.js","lineno":3,"colno":1}]}
            ],"raw_stacktraces":[],"errors":[]}"#,
        );
        let output = javascript_adapter(address)
            .symbolicate(SymbolicationRequest {
                project_id: ProjectId::new(7).unwrap(),
                debug_file_revision: 0,
                artifact_revision: 1,
                kind: SymbolicationKind::JavaScript,
                traces: vec![RawStacktrace {
                    origin: RawTraceOrigin::Event,
                    frames: vec![
                        javascript_frame(Some("app.min.js"), None, Some(1), "a"),
                        javascript_frame(
                            None,
                            Some("https://example.invalid/static/app.min.js"),
                            None,
                            "b",
                        ),
                        javascript_frame(
                            None,
                            Some("https://example.invalid/static/vendor.min.js"),
                            Some(2),
                            "c",
                        ),
                    ],
                }],
                modules: Vec::new(),
                release: None,
                dist: None,
            })
            .await
            .unwrap();
        assert_eq!(output.status, BackendSymbolicationStatus::Partial);
        assert_eq!(
            frame_names(&output.derived[0]),
            vec![(0, Some("fail")), (2, Some("boot"))]
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn javascript_trace_without_sendable_frames_gets_no_derived_trace() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = serve_once(
            listener,
            |body| assert_eq!(body["stacktraces"].as_array().unwrap().len(), 1),
            r#"{"status":"completed","stacktraces":[
                {"frames":[{"data":{"symbolicated":true},"function":"fail","filename":"app.js","lineno":6,"colno":9}]}
            ],"raw_stacktraces":[],"errors":[]}"#,
        );
        let output = javascript_adapter(address)
            .symbolicate(SymbolicationRequest {
                project_id: ProjectId::new(7).unwrap(),
                debug_file_revision: 0,
                artifact_revision: 1,
                kind: SymbolicationKind::JavaScript,
                traces: vec![
                    RawStacktrace {
                        origin: RawTraceOrigin::Exception { index: 0 },
                        frames: vec![javascript_frame(
                            None,
                            Some("https://example.invalid/static/app.min.js"),
                            Some(1),
                            "a",
                        )],
                    },
                    RawStacktrace {
                        origin: RawTraceOrigin::Exception { index: 1 },
                        frames: vec![javascript_frame(
                            None,
                            Some("https://example.invalid/static/app.min.js"),
                            None,
                            "b",
                        )],
                    },
                ],
                modules: Vec::new(),
                release: None,
                dist: None,
            })
            .await
            .unwrap();
        assert_eq!(output.status, BackendSymbolicationStatus::Partial);
        // An empty derived trace would make grouping skip the second exception;
        // without one it is grouped by its raw frames.
        assert_eq!(output.derived.len(), 1);
        assert_eq!(
            output.derived[0].origin,
            RawTraceOrigin::Exception { index: 0 }
        );
        assert_eq!(frame_names(&output.derived[0]), vec![(0, Some("fail"))]);
        server.await.unwrap();
    }

    fn frame_names(trace: &SymbolicatedStacktrace) -> Vec<(usize, Option<&str>)> {
        trace
            .frames
            .iter()
            .map(|frame| (frame.original_index, frame.function.as_deref()))
            .collect()
    }

    /// Answers one request with `response` once `check` has looked at its body.
    fn serve_once(
        listener: tokio::net::TcpListener,
        check: impl FnOnce(&serde_json::Value) + Send + 'static,
        response: &'static str,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (_, body) = read_request(&mut stream).await;
            check(&body);
            respond(&mut stream, response).await;
        })
    }

    fn javascript_adapter(address: std::net::SocketAddr) -> ExternalSymbolicator {
        ExternalSymbolicator::new(
            ExternalSymbolicatorConfig {
                endpoint: Url::parse(&format!("http://{address}/symbolicate")).unwrap(),
                callback_base_url: Url::parse("http://127.0.0.1:4001/").unwrap(),
                request_timeout: Duration::from_secs(2),
                maximum_concurrency: 1,
                circuit_failure_threshold: 2,
                circuit_cooldown: Duration::from_secs(30),
                maximum_response_bytes: 4096,
            },
            PrivateSourceSigner::new(vec![1; 32], None).unwrap(),
        )
        .unwrap()
    }

    fn javascript_frame(
        filename: Option<&str>,
        absolute_path: Option<&str>,
        line: Option<u64>,
        function: &str,
    ) -> NormalizedFrame {
        NormalizedFrame {
            filename: filename.map(Into::into),
            absolute_path: absolute_path.map(Into::into),
            function: Some(function.into()),
            module: None,
            package: None,
            instruction_address: None,
            symbol_address: None,
            line,
            column: Some(50),
            in_app: Some(true),
            context_line: None,
            pre_context: Vec::new(),
            post_context: Vec::new(),
            variables: BTreeMap::new(),
            unknown: BTreeMap::new(),
        }
    }

    /// Reads one HTTP request and returns its head and JSON body.
    async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, serde_json::Value) {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(
                count > 0,
                "connection closed before the request was complete"
            );
            bytes.extend_from_slice(&chunk[..count]);
            let Some(head_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            let body_start = head_end + 4;
            if bytes.len() >= body_start + length {
                let body = serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap();
                return (head, body);
            }
        }
    }

    async fn respond(stream: &mut tokio::net::TcpStream, body: &str) {
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
    }

    #[tokio::test]
    async fn fake_external_contract_carries_revision_and_circuit_opens() {
        let pinned: serde_json::Value = serde_json::from_str(include_str!(
            "../../../sdk-tests/symbolicator/26.6.0-native-contract.json"
        ))
        .unwrap();
        assert_eq!(pinned["image"], "ghcr.io/getsentry/symbolicator:26.6.0");
        let response_body = serde_json::to_string(&pinned["response"]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0_u8; 16 * 1024];
            let count = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(request.contains("revision=42"));
            assert!(request.contains("private-project-7"));
            let body = response_body;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(body.as_bytes()).await.unwrap();
        });
        let endpoint = Url::parse(&format!("http://{address}/symbolicate")).unwrap();
        let signer = PrivateSourceSigner::new(vec![1; 32], None).unwrap();
        let adapter = ExternalSymbolicator::new(
            ExternalSymbolicatorConfig {
                endpoint,
                callback_base_url: Url::parse("http://127.0.0.1:4001/").unwrap(),
                request_timeout: Duration::from_secs(2),
                maximum_concurrency: 1,
                circuit_failure_threshold: 1,
                circuit_cooldown: Duration::from_secs(30),
                maximum_response_bytes: 4096,
            },
            signer,
        )
        .unwrap();
        let request = SymbolicationRequest {
            project_id: ProjectId::new(7).unwrap(),
            debug_file_revision: 42,
            artifact_revision: 0,
            kind: SymbolicationKind::Native,
            traces: Vec::new(),
            modules: Vec::new(),
            release: None,
            dist: None,
        };
        let output = adapter.symbolicate(request).await.unwrap();
        assert_eq!(output.status, BackendSymbolicationStatus::Missing);
        server.await.unwrap();

        let unavailable = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unavailable_address = unavailable.local_addr().unwrap();
        let failure_server = tokio::spawn(async move {
            let (mut stream, _) = unavailable.accept().await.unwrap();
            let mut bytes = [0_u8; 4096];
            let _ = stream.read(&mut bytes).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let failing = ExternalSymbolicator::new(
            ExternalSymbolicatorConfig {
                endpoint: Url::parse(&format!("http://{unavailable_address}/symbolicate")).unwrap(),
                callback_base_url: Url::parse("http://127.0.0.1:4001/").unwrap(),
                request_timeout: Duration::from_secs(2),
                maximum_concurrency: 1,
                circuit_failure_threshold: 1,
                circuit_cooldown: Duration::from_secs(30),
                maximum_response_bytes: 4096,
            },
            PrivateSourceSigner::new(vec![1; 32], None).unwrap(),
        )
        .unwrap();
        let request = SymbolicationRequest {
            project_id: ProjectId::new(7).unwrap(),
            debug_file_revision: 1,
            artifact_revision: 0,
            kind: SymbolicationKind::Native,
            traces: Vec::new(),
            modules: Vec::new(),
            release: None,
            dist: None,
        };
        assert_eq!(
            failing.symbolicate(request.clone()).await,
            Err(SymbolicationBackendError::Unavailable)
        );
        failure_server.await.unwrap();
        assert_eq!(
            failing.symbolicate(request).await,
            Err(SymbolicationBackendError::Unavailable)
        );
    }
}
