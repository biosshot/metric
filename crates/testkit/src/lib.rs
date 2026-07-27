//! Reusable fakes implement the same narrow ports used by production composition.

pub mod incident_capsule;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use metric_domain::{
    AcceptedEvent, DsnKey, ProjectSnapshot, Timestamp,
    feedback::FeedbackRecord,
    sessions::SessionUpdate,
    signals::{LogRecord, SpanRecord},
    symbolication::{BackendSymbolicationResult, SymbolicationRequest},
};
use metric_ports::{
    Clock, DurableOutcome, EventSink, EventSinkError, FeedbackSink, FeedbackStoreError,
    IngestOutcome, LogSink, OutcomeSink, PortFuture, ProjectResolveError, ProjectResolver,
    RandomError, RandomSource, SessionSink, SignalStoreError, SpanSink, SymbolicationBackend,
    SymbolicationBackendError,
};

#[derive(Clone)]
pub struct FakeProjectResolver {
    key: DsnKey,
    snapshot: ProjectSnapshot,
}

impl FakeProjectResolver {
    #[must_use]
    pub const fn new(key: DsnKey, snapshot: ProjectSnapshot) -> Self {
        Self { key, snapshot }
    }
}

impl ProjectResolver for FakeProjectResolver {
    fn resolve(&self, key: DsnKey) -> PortFuture<'_, Result<ProjectSnapshot, ProjectResolveError>> {
        Box::pin(async move {
            if key == self.key {
                Ok(self.snapshot.clone())
            } else {
                Err(ProjectResolveError::Unauthorized)
            }
        })
    }
}

#[derive(Clone)]
pub struct FakeEventSink {
    events: Arc<Mutex<Vec<AcceptedEvent>>>,
    outcome: Result<DurableOutcome, EventSinkError>,
    delay: Duration,
}

impl FakeEventSink {
    #[must_use]
    pub fn accepting() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            outcome: Ok(DurableOutcome::Accepted),
            delay: Duration::ZERO,
        }
    }

    #[must_use]
    pub fn with_outcome(outcome: Result<DurableOutcome, EventSinkError>) -> Self {
        Self {
            outcome,
            ..Self::accepting()
        }
    }

    #[must_use]
    pub fn with_delay(delay: Duration) -> Self {
        Self {
            delay,
            ..Self::accepting()
        }
    }

    #[must_use]
    pub fn events(&self) -> Vec<AcceptedEvent> {
        self.events.lock().expect("fake sink lock poisoned").clone()
    }
}

impl EventSink for FakeEventSink {
    fn persist(
        &self,
        event: AcceptedEvent,
    ) -> PortFuture<'_, Result<DurableOutcome, EventSinkError>> {
        Box::pin(async move {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let outcome = self.outcome?;
            if outcome == DurableOutcome::Accepted {
                self.events
                    .lock()
                    .expect("fake sink lock poisoned")
                    .push(event);
            }
            Ok(outcome)
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeLogSink(Arc<Mutex<Vec<LogRecord>>>);

impl FakeLogSink {
    #[must_use]
    pub fn records(&self) -> Vec<LogRecord> {
        self.0.lock().expect("fake Log sink lock poisoned").clone()
    }
}

impl LogSink for FakeLogSink {
    fn persist_logs(
        &self,
        records: Vec<LogRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(async move {
            let outcomes = vec![DurableOutcome::Accepted; records.len()];
            self.0
                .lock()
                .expect("fake Log sink lock poisoned")
                .extend(records);
            Ok(outcomes)
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeSpanSink(Arc<Mutex<Vec<SpanRecord>>>);

impl FakeSpanSink {
    #[must_use]
    pub fn records(&self) -> Vec<SpanRecord> {
        self.0.lock().expect("fake Span sink lock poisoned").clone()
    }
}

impl SpanSink for FakeSpanSink {
    fn persist_spans(
        &self,
        records: Vec<SpanRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(async move {
            let outcomes = vec![DurableOutcome::Accepted; records.len()];
            self.0
                .lock()
                .expect("fake Span sink lock poisoned")
                .extend(records);
            Ok(outcomes)
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeSessionSink(Arc<Mutex<Vec<SessionUpdate>>>);

impl FakeSessionSink {
    #[must_use]
    pub fn updates(&self) -> Vec<SessionUpdate> {
        self.0
            .lock()
            .expect("fake Session sink lock poisoned")
            .clone()
    }
}

impl SessionSink for FakeSessionSink {
    fn persist_sessions(
        &self,
        updates: Vec<SessionUpdate>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(async move {
            let outcomes = vec![DurableOutcome::Accepted; updates.len()];
            self.0
                .lock()
                .expect("fake Session sink lock poisoned")
                .extend(updates);
            Ok(outcomes)
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeFeedbackSink(Arc<Mutex<Vec<FeedbackRecord>>>);

impl FakeFeedbackSink {
    #[must_use]
    pub fn records(&self) -> Vec<FeedbackRecord> {
        self.0
            .lock()
            .expect("fake Feedback sink lock poisoned")
            .clone()
    }
}

impl FeedbackSink for FakeFeedbackSink {
    fn persist_feedback(
        &self,
        feedback: FeedbackRecord,
    ) -> PortFuture<'_, Result<DurableOutcome, FeedbackStoreError>> {
        Box::pin(async move {
            self.0
                .lock()
                .expect("fake Feedback sink lock poisoned")
                .push(feedback);
            Ok(DurableOutcome::Accepted)
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeOutcomeSink(Arc<Mutex<Vec<IngestOutcome>>>);

impl FakeOutcomeSink {
    #[must_use]
    pub fn outcomes(&self) -> Vec<IngestOutcome> {
        self.0.lock().expect("fake outcome lock poisoned").clone()
    }
}

impl OutcomeSink for FakeOutcomeSink {
    fn record(&self, outcome: IngestOutcome) {
        self.0
            .lock()
            .expect("fake outcome lock poisoned")
            .push(outcome);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FixedRandom(pub u8);

impl RandomSource for FixedRandom {
    fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
        output.fill(self.0);
        Ok(())
    }
}

/// Reusable backend script for SymbolicationService and future Processor tests.
#[derive(Clone)]
pub struct ScriptedSymbolicationBackend {
    outcome: Result<BackendSymbolicationResult, SymbolicationBackendError>,
    delay: Duration,
    requests: Arc<Mutex<Vec<SymbolicationRequest>>>,
}

impl ScriptedSymbolicationBackend {
    #[must_use]
    pub fn new(outcome: Result<BackendSymbolicationResult, SymbolicationBackendError>) -> Self {
        Self {
            outcome,
            delay: Duration::ZERO,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn with_delay(
        outcome: Result<BackendSymbolicationResult, SymbolicationBackendError>,
        delay: Duration,
    ) -> Self {
        Self {
            delay,
            ..Self::new(outcome)
        }
    }

    #[must_use]
    pub fn requests(&self) -> Vec<SymbolicationRequest> {
        self.requests
            .lock()
            .expect("scripted symbolication lock poisoned")
            .clone()
    }
}

impl SymbolicationBackend for ScriptedSymbolicationBackend {
    fn symbolicate(
        &self,
        request: SymbolicationRequest,
    ) -> PortFuture<'_, Result<BackendSymbolicationResult, SymbolicationBackendError>> {
        self.requests
            .lock()
            .expect("scripted symbolication lock poisoned")
            .push(request);
        let delay = self.delay;
        let outcome = self.outcome.clone();
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            outcome
        })
    }
}

#[cfg(test)]
mod tests {
    use metric_domain::{ProjectId, symbolication::*};
    use metric_ports::SymbolicationBackend;

    use super::ScriptedSymbolicationBackend;

    #[tokio::test]
    async fn symbolication_fake_adapter_conformance_records_owned_request() {
        let outcome = BackendSymbolicationResult {
            status: BackendSymbolicationStatus::Missing,
            derived: Vec::new(),
            missing_debug_ids: vec!["debug-a".into()],
            diagnostics: vec![SymbolicationDiagnosticCode::MissingDebugFile],
        };
        let backend = ScriptedSymbolicationBackend::new(Ok(outcome.clone()));
        let request = SymbolicationRequest {
            project_id: ProjectId::new(42).unwrap(),
            debug_file_revision: 0,
            artifact_revision: 0,
            kind: SymbolicationKind::Native,
            traces: Vec::new(),
            modules: Vec::new(),
            release: None,
            dist: None,
        };
        assert_eq!(backend.symbolicate(request.clone()).await.unwrap(), outcome);
        assert_eq!(backend.requests(), vec![request]);
    }

    #[test]
    #[ignore = "requires deploy/compose.dev.yml"]
    fn infrastructure_mongodb_orchestration_is_pinned() {
        let compose = include_str!("../../../deploy/compose.dev.yml");
        assert!(compose.contains("image: mongo:8.0.12"));
    }

    #[test]
    #[ignore = "recorded only on declared benchmark hardware"]
    fn performance_empty_foundation_has_no_module_workers() {
        assert_eq!(
            std::mem::size_of::<metric_application::observability::Metrics>(),
            0
        );
        let shutdown = metric_application::shutdown::ShutdownRoot::new();
        assert!(!shutdown.is_started());
    }
}
