//! Reusable fakes implement the same narrow ports used by production composition.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use faultkeep_domain::{AcceptedEvent, DsnKey, ProjectSnapshot, Timestamp};
use faultkeep_ports::{
    Clock, DurableOutcome, EventSink, EventSinkError, IngestOutcome, OutcomeSink, PortFuture,
    ProjectResolveError, ProjectResolver, RandomError, RandomSource,
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

#[cfg(test)]
mod tests {
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
            std::mem::size_of::<faultkeep_application::observability::Metrics>(),
            0
        );
        let shutdown = faultkeep_application::shutdown::ShutdownRoot::new();
        assert!(!shutdown.is_started());
    }
}
