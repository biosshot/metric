//! Cancellable project deletion and bounded durable purge orchestration.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use faultkeep_domain::{
    AcceptedEvent, ProjectId, Timestamp,
    deletion::{ProjectDeletionOperationId, ProjectDeletionRequest, ProjectDeletionStatus},
};
use faultkeep_ports::{
    Clock, DurableOutcome, EventSink, EventSinkError, PortFuture, ProjectDeletionStore,
    ProjectDeletionStoreError, ProjectPurgeRequest,
};
use thiserror::Error;
use tokio::{
    sync::Notify,
    task::JoinHandle,
    time::{MissedTickBehavior, interval, timeout},
};

use crate::{projects::ProjectService, shutdown::ShutdownSignal};

#[derive(Debug, Clone, Copy)]
pub struct ProjectDeletionConfig {
    pub grace_period: Duration,
    pub delete_batch_documents: usize,
    pub completed_job_retention: Duration,
    pub slug_reservation: Duration,
    pub poll_interval: Duration,
    pub operation_timeout: Duration,
    pub drain_timeout: Duration,
    pub retry_base: Duration,
    pub retry_max: Duration,
}

impl Default for ProjectDeletionConfig {
    fn default() -> Self {
        Self {
            grace_period: Duration::from_secs(24 * 60 * 60),
            delete_batch_documents: 5_000,
            completed_job_retention: Duration::from_secs(30 * 24 * 60 * 60),
            slug_reservation: Duration::from_secs(30 * 24 * 60 * 60),
            poll_interval: Duration::from_secs(1),
            operation_timeout: Duration::from_secs(10),
            drain_timeout: Duration::from_secs(10),
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProjectDeletionError {
    #[error("project deletion configuration is invalid")]
    InvalidConfiguration,
    #[error("project deletion confirmation does not match")]
    ConfirmationMismatch,
    #[error("project deletion conflicts with an existing operation")]
    Conflict,
    #[error("project deletion target does not exist")]
    NotFound,
    #[error("project deletion can no longer be cancelled")]
    NotCancellable,
    #[error("project deletion storage is temporarily unavailable")]
    Unavailable,
}

pub struct ProjectDeletionService {
    store: Arc<dyn ProjectDeletionStore>,
    projects: Arc<ProjectService>,
    clock: Arc<dyn Clock>,
    work: Arc<ProjectWorkRegistry>,
    config: ProjectDeletionConfig,
}

impl ProjectDeletionService {
    pub fn new(
        store: Arc<dyn ProjectDeletionStore>,
        projects: Arc<ProjectService>,
        clock: Arc<dyn Clock>,
        work: Arc<ProjectWorkRegistry>,
        config: ProjectDeletionConfig,
    ) -> Result<Arc<Self>, ProjectDeletionError> {
        validate(config)?;
        Ok(Arc::new(Self {
            store,
            projects,
            clock,
            work,
            config,
        }))
    }

    pub async fn request(
        &self,
        project_id: ProjectId,
        organization_id: faultkeep_domain::OrganizationId,
        requested_by: faultkeep_domain::auth::UserId,
        operation_id: ProjectDeletionOperationId,
        confirmation: &str,
    ) -> Result<ProjectDeletionStatus, ProjectDeletionError> {
        let project =
            self.projects
                .load_project_view(project_id)
                .await
                .map_err(|error| match error {
                    crate::projects::ProjectServiceError::NotFound => {
                        ProjectDeletionError::NotFound
                    }
                    _ => ProjectDeletionError::Unavailable,
                })?;
        if project.organization_id != organization_id {
            return Err(ProjectDeletionError::NotFound);
        }
        if confirmation != project.slug.as_str() {
            return Err(ProjectDeletionError::ConfirmationMismatch);
        }
        let now = self.clock.now();
        let purge_after = add_duration(now, self.config.grace_period);
        let change = timeout(
            self.config.operation_timeout,
            self.store.request_deletion(ProjectDeletionRequest {
                operation_id,
                project_id,
                organization_id,
                requested_by,
                requested_at: now,
                purge_after,
            }),
        )
        .await
        .map_err(|_| ProjectDeletionError::Unavailable)?
        .map_err(map_store)?;
        self.projects.invalidate_keys(&change.affected_keys);
        self.work
            .fence_and_drain(project_id, self.config.drain_timeout)
            .await;
        Ok(change.status)
    }

    pub async fn cancel(
        &self,
        project_id: ProjectId,
        operation_id: ProjectDeletionOperationId,
    ) -> Result<ProjectDeletionStatus, ProjectDeletionError> {
        let change = timeout(
            self.config.operation_timeout,
            self.store.cancel_deletion(
                project_id,
                operation_id,
                self.clock.now(),
                self.config.completed_job_retention,
            ),
        )
        .await
        .map_err(|_| ProjectDeletionError::Unavailable)?
        .map_err(map_store)?;
        self.projects.invalidate_keys(&change.affected_keys);
        self.work.unfence(project_id);
        Ok(change.status)
    }

    pub async fn status(
        &self,
        project_id: ProjectId,
    ) -> Result<ProjectDeletionStatus, ProjectDeletionError> {
        timeout(
            self.config.operation_timeout,
            self.store.deletion_status(project_id),
        )
        .await
        .map_err(|_| ProjectDeletionError::Unavailable)?
        .map_err(map_store)
    }
}

#[derive(Default)]
pub struct ProjectWorkRegistry {
    state: Mutex<HashMap<ProjectId, WorkState>>,
    changed: Notify,
}

#[derive(Default)]
struct WorkState {
    fenced: bool,
    active: usize,
}

impl ProjectWorkRegistry {
    fn try_enter(self: &Arc<Self>, project_id: ProjectId) -> Option<ProjectWorkGuard> {
        let mut state = lock(&self.state);
        let entry = state.entry(project_id).or_default();
        if entry.fenced {
            return None;
        }
        entry.active = entry.active.saturating_add(1);
        Some(ProjectWorkGuard {
            registry: Arc::clone(self),
            project_id,
        })
    }

    async fn fence_and_drain(&self, project_id: ProjectId, maximum: Duration) {
        {
            let mut state = lock(&self.state);
            state.entry(project_id).or_default().fenced = true;
        }
        let drained = async {
            loop {
                if lock(&self.state)
                    .get(&project_id)
                    .is_none_or(|entry| entry.active == 0)
                {
                    return;
                }
                self.changed.notified().await;
            }
        };
        let _ = timeout(maximum, drained).await;
    }

    fn unfence(&self, project_id: ProjectId) {
        if let Some(entry) = lock(&self.state).get_mut(&project_id) {
            entry.fenced = false;
        }
        self.changed.notify_waiters();
    }
}

struct ProjectWorkGuard {
    registry: Arc<ProjectWorkRegistry>,
    project_id: ProjectId,
}

impl Drop for ProjectWorkGuard {
    fn drop(&mut self) {
        if let Some(entry) = lock(&self.registry.state).get_mut(&self.project_id) {
            entry.active = entry.active.saturating_sub(1);
        }
        self.registry.changed.notify_waiters();
    }
}

pub struct ProjectFencedEventSink {
    inner: Arc<dyn EventSink>,
    work: Arc<ProjectWorkRegistry>,
}

impl ProjectFencedEventSink {
    #[must_use]
    pub fn new(inner: Arc<dyn EventSink>, work: Arc<ProjectWorkRegistry>) -> Self {
        Self { inner, work }
    }
}

impl EventSink for ProjectFencedEventSink {
    fn persist(
        &self,
        event: AcceptedEvent,
    ) -> PortFuture<'_, Result<DurableOutcome, EventSinkError>> {
        let Some(guard) = self.work.try_enter(event.project_id) else {
            metrics::counter!("faultkeep_project_deletion_fenced_ingest_total").increment(1);
            return Box::pin(async { Err(EventSinkError::Unavailable) });
        };
        Box::pin(async move {
            let result = self.inner.persist(event).await;
            drop(guard);
            result
        })
    }
}

pub struct ProjectDeletionTask {
    join: JoinHandle<()>,
}

impl ProjectDeletionTask {
    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

pub fn start_project_deletion_worker(
    store: Arc<dyn ProjectDeletionStore>,
    clock: Arc<dyn Clock>,
    config: ProjectDeletionConfig,
    shutdown: ShutdownSignal,
) -> Result<ProjectDeletionTask, ProjectDeletionError> {
    validate(config)?;
    let join = tokio::spawn(async move {
        let mut tick = interval(config.poll_interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        tick.tick().await;
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                _ = tick.tick() => {
                    let request = ProjectPurgeRequest {
                        now: clock.now(),
                        batch_size: config.delete_batch_documents,
                        retry_base: config.retry_base,
                        retry_max: config.retry_max,
                        completed_retention: config.completed_job_retention,
                        slug_reservation: config.slug_reservation,
                    };
                    let outcome = timeout(config.operation_timeout, store.purge_next(request)).await;
                    let label = match outcome {
                        Ok(Ok(Some(_))) => "progress",
                        Ok(Ok(None)) => "idle",
                        Ok(Err(_)) => "error",
                        Err(_) => "timeout",
                    };
                    metrics::counter!(
                        "faultkeep_project_deletion_worker_runs_total",
                        "outcome" => label
                    ).increment(1);
                }
            }
        }
    });
    Ok(ProjectDeletionTask { join })
}

fn validate(config: ProjectDeletionConfig) -> Result<(), ProjectDeletionError> {
    let valid = !config.grace_period.is_zero()
        && (1..=10_000).contains(&config.delete_batch_documents)
        && !config.completed_job_retention.is_zero()
        && !config.slug_reservation.is_zero()
        && !config.poll_interval.is_zero()
        && !config.operation_timeout.is_zero()
        && !config.drain_timeout.is_zero()
        && !config.retry_base.is_zero()
        && config.retry_base <= config.retry_max;
    valid
        .then_some(())
        .ok_or(ProjectDeletionError::InvalidConfiguration)
}

fn map_store(error: ProjectDeletionStoreError) -> ProjectDeletionError {
    match error {
        ProjectDeletionStoreError::Conflict => ProjectDeletionError::Conflict,
        ProjectDeletionStoreError::NotFound => ProjectDeletionError::NotFound,
        ProjectDeletionStoreError::NotCancellable => ProjectDeletionError::NotCancellable,
        ProjectDeletionStoreError::InvalidData | ProjectDeletionStoreError::Unavailable => {
            ProjectDeletionError::Unavailable
        }
    }
}

fn add_duration(timestamp: Timestamp, duration: Duration) -> Timestamp {
    let millis = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_add(millis)).unwrap_or(timestamp)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use faultkeep_domain::{EventId, ScrubbedEventPayload};

    use super::*;

    #[derive(Default)]
    struct BlockingSink {
        entered: Notify,
        release: Notify,
        calls: AtomicUsize,
    }

    impl EventSink for BlockingSink {
        fn persist(
            &self,
            _event: AcceptedEvent,
        ) -> PortFuture<'_, Result<DurableOutcome, EventSinkError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::AcqRel);
                self.entered.notify_one();
                self.release.notified().await;
                Ok(DurableOutcome::Accepted)
            })
        }
    }

    fn event() -> AcceptedEvent {
        AcceptedEvent {
            project_id: ProjectId::new(42).unwrap(),
            event_id: EventId::from_bytes([1; 16]),
            received_at: Timestamp::from_unix_millis(1_000).unwrap(),
            policy_revision: 1,
            payload: ScrubbedEventPayload::new(Vec::from(&b"{}"[..])),
        }
    }

    #[tokio::test]
    async fn deletion_fence_drains_existing_ingest_and_rejects_new_work() {
        let inner = Arc::new(BlockingSink::default());
        let work = Arc::new(ProjectWorkRegistry::default());
        let sink = Arc::new(ProjectFencedEventSink::new(
            inner.clone(),
            Arc::clone(&work),
        ));
        let first = {
            let sink = Arc::clone(&sink);
            tokio::spawn(async move { sink.persist(event()).await })
        };
        inner.entered.notified().await;
        let drain = {
            let work = Arc::clone(&work);
            tokio::spawn(async move {
                work.fence_and_drain(ProjectId::new(42).unwrap(), Duration::from_secs(1))
                    .await;
            })
        };
        tokio::task::yield_now().await;
        assert!(!drain.is_finished());
        inner.release.notify_waiters();
        assert_eq!(first.await.unwrap(), Ok(DurableOutcome::Accepted));
        drain.await.unwrap();
        assert_eq!(
            sink.persist(event()).await,
            Err(EventSinkError::Unavailable)
        );
        work.unfence(ProjectId::new(42).unwrap());
        let accepted = {
            let sink = Arc::clone(&sink);
            tokio::spawn(async move { sink.persist(event()).await })
        };
        inner.entered.notified().await;
        inner.release.notify_waiters();
        assert_eq!(accepted.await.unwrap(), Ok(DurableOutcome::Accepted));
        assert_eq!(inner.calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn deletion_configuration_is_bounded() {
        assert!(validate(ProjectDeletionConfig::default()).is_ok());
        assert_eq!(
            validate(ProjectDeletionConfig {
                delete_batch_documents: 0,
                ..ProjectDeletionConfig::default()
            }),
            Err(ProjectDeletionError::InvalidConfiguration)
        );
    }
}
