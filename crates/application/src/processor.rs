//! Ordered, bounded post-acceptance Event orchestration.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use faultkeep_domain::{
    AcceptedEvent, ProjectAcceptanceState, ProjectId, Timestamp,
    event::NormalizedEvent,
    grouping::{GroupingError, GroupingResult, group},
    issue::IssueOccurrence,
    processing::{
        PendingEvent, ProcessingErrorCode, ProcessingFailure, ProcessingFailureDisposition,
        ProcessingStateChange,
    },
    symbolication::{SymbolicationDisposition, SymbolicationResult},
};
use faultkeep_ports::{
    Clock, PortFuture, ProcessingProjectError, ProcessingProjectStore, ProcessingStateError,
    ProcessingStateStore, SymbolicationBackend,
};
use futures_util::{FutureExt, future::Shared};
use thiserror::Error;
use tokio::{
    sync::{Semaphore, mpsc, oneshot},
    time::{Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

use crate::{
    finalizer::{Finalizer, FinalizerError},
    issues::{IssueServiceError, prepare_issue_occurrence},
    normalizer::{NormalizationError, Normalizer},
    symbolication::{BaselineSymbolicationService, SymbolicationService},
};

const HARD_MAX_CONCURRENCY: usize = 4_096;
const HARD_MAX_ATTEMPTS: u32 = 100;
const HARD_MAX_DURATION: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy)]
pub struct ProcessorConfig {
    pub max_concurrency: usize,
    pub max_attempts: u32,
    pub retry_base: Duration,
    pub retry_max: Duration,
    pub stage_timeout: Duration,
    pub total_timeout: Duration,
    pub state_timeout: Duration,
}

impl Default for ProcessorConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 32,
            max_attempts: 5,
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(5 * 60),
            stage_timeout: Duration::from_secs(15),
            total_timeout: Duration::from_secs(60),
            state_timeout: Duration::from_secs(5),
        }
    }
}

impl ProcessorConfig {
    pub fn validate(self) -> Result<Self, ProcessorConfigError> {
        let durations = [
            self.retry_base,
            self.retry_max,
            self.stage_timeout,
            self.total_timeout,
            self.state_timeout,
        ];
        let valid = (1..=HARD_MAX_CONCURRENCY).contains(&self.max_concurrency)
            && (1..=HARD_MAX_ATTEMPTS).contains(&self.max_attempts)
            && durations
                .iter()
                .all(|duration| !duration.is_zero() && *duration <= HARD_MAX_DURATION)
            && self.retry_base <= self.retry_max
            && self.stage_timeout <= self.total_timeout
            && self.state_timeout <= self.total_timeout;
        valid.then_some(self).ok_or(ProcessorConfigError)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("Processor configuration is invalid")]
pub struct ProcessorConfigError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageFailureClass {
    Temporary,
    Permanent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageFailure {
    pub class: StageFailureClass,
    pub code: ProcessingErrorCode,
}

impl StageFailure {
    #[must_use]
    pub const fn temporary(code: ProcessingErrorCode) -> Self {
        Self {
            class: StageFailureClass::Temporary,
            code,
        }
    }

    #[must_use]
    pub const fn permanent(code: ProcessingErrorCode) -> Self {
        Self {
            class: StageFailureClass::Permanent,
            code,
        }
    }
}

pub trait NormalizationStage: Send + Sync + 'static {
    fn normalize<'a>(
        &'a self,
        event: &'a AcceptedEvent,
    ) -> PortFuture<'a, Result<NormalizedEvent, StageFailure>>;
}

pub trait SymbolicationStage: Send + Sync + 'static {
    fn symbolicate<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        debug_file_revision: u64,
        cancellation: &'a CancellationToken,
    ) -> PortFuture<'a, Result<SymbolicationResult, StageFailure>>;
}

pub trait GroupingStage: Send + Sync + 'static {
    fn group<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        symbolication: &'a SymbolicationResult,
        revision: u64,
    ) -> PortFuture<'a, Result<GroupingResult, StageFailure>>;
}

pub trait IssuePreparationStage: Send + Sync + 'static {
    fn prepare<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        grouping: &'a GroupingResult,
    ) -> PortFuture<'a, Result<IssueOccurrence, StageFailure>>;
}

pub trait EventFinalizationStage: Send + Sync + 'static {
    fn finalize<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        symbolication: &'a SymbolicationResult,
        grouping: &'a GroupingResult,
        issue: IssueOccurrence,
    ) -> PortFuture<'a, Result<(), StageFailure>>;
}

impl NormalizationStage for Normalizer {
    fn normalize<'a>(
        &'a self,
        event: &'a AcceptedEvent,
    ) -> PortFuture<'a, Result<NormalizedEvent, StageFailure>> {
        Box::pin(async move { self.normalize(event).map_err(map_normalization_error) })
    }
}

impl SymbolicationStage for BaselineSymbolicationService {
    fn symbolicate<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        _debug_file_revision: u64,
        _cancellation: &'a CancellationToken,
    ) -> PortFuture<'a, Result<SymbolicationResult, StageFailure>> {
        Box::pin(async move { Ok(Self::symbolicate(event)) })
    }
}

impl<B: SymbolicationBackend> SymbolicationStage for SymbolicationService<B> {
    fn symbolicate<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        debug_file_revision: u64,
        cancellation: &'a CancellationToken,
    ) -> PortFuture<'a, Result<SymbolicationResult, StageFailure>> {
        Box::pin(async move {
            let result = self
                .symbolicate_with_revision(event, debug_file_revision, cancellation)
                .await;
            if result.disposition == SymbolicationDisposition::Retryable {
                Err(StageFailure::temporary(
                    ProcessingErrorCode::SymbolicationRetryable,
                ))
            } else {
                Ok(result)
            }
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GrouperStage;

impl GroupingStage for GrouperStage {
    fn group<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        symbolication: &'a SymbolicationResult,
        revision: u64,
    ) -> PortFuture<'a, Result<GroupingResult, StageFailure>> {
        Box::pin(async move {
            group(event.project_id, revision, &event.body, Some(symbolication))
                .map_err(map_grouping_error)
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct IssuePreparerStage;

impl IssuePreparationStage for IssuePreparerStage {
    fn prepare<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        grouping: &'a GroupingResult,
    ) -> PortFuture<'a, Result<IssueOccurrence, StageFailure>> {
        Box::pin(async move { prepare_issue_occurrence(event, grouping).map_err(map_issue_error) })
    }
}

impl EventFinalizationStage for Finalizer {
    fn finalize<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        symbolication: &'a SymbolicationResult,
        grouping: &'a GroupingResult,
        issue: IssueOccurrence,
    ) -> PortFuture<'a, Result<(), StageFailure>> {
        Box::pin(async move {
            let event = self
                .prepare(event, symbolication, grouping, issue)
                .map_err(map_finalizer_error)?;
            self.finalize(vec![event])
                .await
                .map(|_| ())
                .map_err(map_finalizer_error)
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FinalizerBatchConfig {
    pub channel_capacity: usize,
    pub max_wait: Duration,
    pub max_events: usize,
    pub shutdown_drain: Duration,
}

impl Default for FinalizerBatchConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 4_096,
            max_wait: Duration::from_millis(5),
            max_events: 256,
            shutdown_drain: Duration::from_secs(10),
        }
    }
}

struct FinalizerRequest {
    event: faultkeep_domain::finalization::FinalizeEvent,
    response: oneshot::Sender<Result<(), StageFailure>>,
}

pub struct FinalizerBatcher {
    finalizer: Arc<Finalizer>,
    sender: mpsc::Sender<FinalizerRequest>,
    accepting: Arc<AtomicBool>,
    shutdown: CancellationToken,
}

pub struct FinalizerBatchTask {
    join: tokio::task::JoinHandle<()>,
}

impl FinalizerBatchTask {
    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

impl FinalizerBatcher {
    pub fn start(
        finalizer: Arc<Finalizer>,
        config: FinalizerBatchConfig,
    ) -> Result<(Arc<Self>, FinalizerBatchTask), ProcessorConfigError> {
        if config.channel_capacity == 0
            || config.channel_capacity > 100_000
            || config.max_events == 0
            || config.max_events > 10_000
            || config.max_events > config.channel_capacity
            || config.max_wait.is_zero()
            || config.max_wait > Duration::from_secs(1)
            || config.shutdown_drain.is_zero()
            || config.shutdown_drain > HARD_MAX_DURATION
        {
            return Err(ProcessorConfigError);
        }
        let (sender, receiver) = mpsc::channel(config.channel_capacity);
        let accepting = Arc::new(AtomicBool::new(true));
        let shutdown = CancellationToken::new();
        let service = Arc::new(Self {
            finalizer: finalizer.clone(),
            sender,
            accepting: accepting.clone(),
            shutdown: shutdown.clone(),
        });
        let join = tokio::spawn(run_finalizer_batches(
            finalizer, receiver, accepting, shutdown, config,
        ));
        Ok((service, FinalizerBatchTask { join }))
    }

    pub fn close(&self) {
        self.accepting.store(false, Ordering::Release);
        self.shutdown.cancel();
    }
}

impl EventFinalizationStage for FinalizerBatcher {
    fn finalize<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        symbolication: &'a SymbolicationResult,
        grouping: &'a GroupingResult,
        issue: IssueOccurrence,
    ) -> PortFuture<'a, Result<(), StageFailure>> {
        Box::pin(async move {
            if !self.accepting.load(Ordering::Acquire) {
                return Err(StageFailure::temporary(
                    ProcessingErrorCode::FinalizerUnavailable,
                ));
            }
            let event = self
                .finalizer
                .prepare(event, symbolication, grouping, issue)
                .map_err(map_finalizer_error)?;
            let (response, result) = oneshot::channel();
            self.sender
                .send(FinalizerRequest { event, response })
                .await
                .map_err(|_| StageFailure::temporary(ProcessingErrorCode::FinalizerUnavailable))?;
            result
                .await
                .map_err(|_| StageFailure::temporary(ProcessingErrorCode::FinalizerUnavailable))?
        })
    }
}

async fn run_finalizer_batches(
    finalizer: Arc<Finalizer>,
    mut receiver: mpsc::Receiver<FinalizerRequest>,
    accepting: Arc<AtomicBool>,
    shutdown: CancellationToken,
    config: FinalizerBatchConfig,
) {
    loop {
        let first = tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                accepting.store(false, Ordering::Release);
                receiver.close();
                break;
            }
            request = receiver.recv() => request,
        };
        let Some(first) = first else {
            break;
        };
        finalize_one_batch(&finalizer, &mut receiver, first, config).await;
    }
    let deadline = Instant::now() + config.shutdown_drain;
    while let Ok(Some(first)) = timeout_at(deadline, receiver.recv()).await {
        finalize_one_batch(&finalizer, &mut receiver, first, config).await;
    }
    while let Ok(request) = receiver.try_recv() {
        let _ = request.response.send(Err(StageFailure::temporary(
            ProcessingErrorCode::FinalizerUnavailable,
        )));
    }
}

async fn finalize_one_batch(
    finalizer: &Finalizer,
    receiver: &mut mpsc::Receiver<FinalizerRequest>,
    first: FinalizerRequest,
    config: FinalizerBatchConfig,
) {
    let started = Instant::now();
    let deadline = started + config.max_wait;
    let mut requests = Vec::with_capacity(config.max_events);
    requests.push(first);
    while requests.len() < config.max_events {
        match timeout_at(deadline, receiver.recv()).await {
            Ok(Some(request)) => requests.push(request),
            Ok(None) | Err(_) => break,
        }
    }
    let events = requests
        .iter()
        .map(|request| request.event.clone())
        .collect();
    let result = finalizer
        .finalize(events)
        .await
        .map(|_| ())
        .map_err(map_finalizer_error);
    let outcome = if result.is_ok() { "ok" } else { "failed" };
    let count = requests.len();
    for request in requests {
        let _ = request.response.send(result);
    }
    metrics::histogram!("faultkeep_processor_finalize_batch_events").record(count as f64);
    metrics::histogram!(
        "faultkeep_processor_finalize_batch_duration_seconds",
        "outcome" => outcome
    )
    .record(started.elapsed().as_secs_f64());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorOutcome {
    Processed,
    RetryScheduled,
    PermanentlyFailed,
    StaleOrCompleted,
    StateUnavailable,
}

pub struct Processor {
    projects: Arc<dyn ProcessingProjectStore>,
    states: Arc<dyn ProcessingStateStore>,
    normalizer: Arc<dyn NormalizationStage>,
    symbolicator: Arc<dyn SymbolicationStage>,
    grouper: Arc<dyn GroupingStage>,
    issues: Arc<dyn IssuePreparationStage>,
    finalizer: Arc<dyn EventFinalizationStage>,
    clock: Arc<dyn Clock>,
    permits: Arc<Semaphore>,
    cancellation: CancellationToken,
    config: ProcessorConfig,
    project_inflight: Mutex<HashMap<ProjectId, Arc<ProjectFlight>>>,
}

type ProjectLoadFuture = Shared<
    std::pin::Pin<
        Box<
            dyn Future<
                    Output = Result<
                        faultkeep_domain::processing::ProcessingProject,
                        ProcessingProjectError,
                    >,
                > + Send,
        >,
    >,
>;

struct ProjectFlight {
    future: ProjectLoadFuture,
}

impl Processor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        projects: Arc<dyn ProcessingProjectStore>,
        states: Arc<dyn ProcessingStateStore>,
        normalizer: Arc<dyn NormalizationStage>,
        symbolicator: Arc<dyn SymbolicationStage>,
        grouper: Arc<dyn GroupingStage>,
        issues: Arc<dyn IssuePreparationStage>,
        finalizer: Arc<dyn EventFinalizationStage>,
        clock: Arc<dyn Clock>,
        config: ProcessorConfig,
    ) -> Result<Self, ProcessorConfigError> {
        let config = config.validate()?;
        Ok(Self {
            projects,
            states,
            normalizer,
            symbolicator,
            grouper,
            issues,
            finalizer,
            clock,
            permits: Arc::new(Semaphore::new(config.max_concurrency)),
            cancellation: CancellationToken::new(),
            config,
            project_inflight: Mutex::new(HashMap::new()),
        })
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub async fn process(&self, pending: PendingEvent) -> ProcessorOutcome {
        let started = Instant::now();
        let total_deadline = started + self.config.total_timeout;
        let permit = tokio::select! {
            () = self.cancellation.cancelled() => {
                return self.persist_failure(
                    &pending,
                    StageFailure::temporary(ProcessingErrorCode::Cancelled),
                ).await;
            }
            result = timeout_at(total_deadline, self.permits.clone().acquire_owned()) => {
                match result {
                    Ok(Ok(permit)) => permit,
                    Ok(Err(_)) => {
                        return self.persist_failure(
                            &pending,
                            StageFailure::temporary(ProcessingErrorCode::Cancelled),
                        ).await;
                    }
                    Err(_) => {
                        return self.persist_failure(
                            &pending,
                            StageFailure::temporary(ProcessingErrorCode::TotalDeadline),
                        ).await;
                    }
                }
            }
        };
        metrics::gauge!("faultkeep_processor_active").increment(1.0);
        let result = self.execute(&pending, total_deadline).await;
        drop(permit);
        metrics::gauge!("faultkeep_processor_active").decrement(1.0);
        let outcome = match result {
            Ok(()) => ProcessorOutcome::Processed,
            Err(failure) => self.persist_failure(&pending, failure).await,
        };
        metrics::histogram!(
            "faultkeep_processor_duration_seconds",
            "outcome" => outcome_name(outcome)
        )
        .record(started.elapsed().as_secs_f64());
        metrics::counter!(
            "faultkeep_processor_events_total",
            "outcome" => outcome_name(outcome)
        )
        .increment(1);
        outcome
    }

    async fn execute(
        &self,
        pending: &PendingEvent,
        total_deadline: Instant,
    ) -> Result<(), StageFailure> {
        let project = run_stage(
            "project",
            self.config.stage_timeout,
            total_deadline,
            &self.cancellation,
            self.load_project_coalesced(pending.event.project_id),
        )
        .await?
        .map_err(map_project_error)?;
        if project.project_id != pending.event.project_id {
            return Err(StageFailure::permanent(
                ProcessingErrorCode::ProjectInvalidData,
            ));
        }
        if project.state != ProjectAcceptanceState::Active {
            return Err(StageFailure::permanent(ProcessingErrorCode::ProjectFenced));
        }
        if !project.error_events_enabled {
            return Err(StageFailure::permanent(
                ProcessingErrorCode::ErrorCapabilityDisabled,
            ));
        }
        let normalized = run_stage(
            "normalizer",
            self.config.stage_timeout,
            total_deadline,
            &self.cancellation,
            self.normalizer.normalize(&pending.event),
        )
        .await??;
        let symbolication = run_stage(
            "symbolication",
            self.config.stage_timeout,
            total_deadline,
            &self.cancellation,
            self.symbolicator.symbolicate(
                &normalized,
                project.debug_file_revision,
                &self.cancellation,
            ),
        )
        .await??;
        let grouping = run_stage(
            "grouper",
            self.config.stage_timeout,
            total_deadline,
            &self.cancellation,
            self.grouper
                .group(&normalized, &symbolication, project.grouping_revision),
        )
        .await??;
        let issue = run_stage(
            "issue_service",
            self.config.stage_timeout,
            total_deadline,
            &self.cancellation,
            self.issues.prepare(&normalized, &grouping),
        )
        .await??;
        run_stage(
            "finalizer",
            self.config.stage_timeout,
            total_deadline,
            &self.cancellation,
            self.finalizer
                .finalize(&normalized, &symbolication, &grouping, issue),
        )
        .await??;
        Ok(())
    }

    async fn load_project_coalesced(
        &self,
        project_id: ProjectId,
    ) -> Result<faultkeep_domain::processing::ProcessingProject, ProcessingProjectError> {
        let flight = {
            let mut flights = self
                .project_inflight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            flights
                .entry(project_id)
                .or_insert_with(|| {
                    let projects = self.projects.clone();
                    let future: std::pin::Pin<
                        Box<
                            dyn Future<
                                    Output = Result<
                                        faultkeep_domain::processing::ProcessingProject,
                                        ProcessingProjectError,
                                    >,
                                > + Send,
                        >,
                    > = Box::pin(async move { projects.load_processing_project(project_id).await });
                    Arc::new(ProjectFlight {
                        future: future.shared(),
                    })
                })
                .clone()
        };
        let result = flight.future.clone().await;
        let mut flights = self
            .project_inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if flights
            .get(&project_id)
            .is_some_and(|current| Arc::ptr_eq(current, &flight))
        {
            flights.remove(&project_id);
        }
        result
    }

    async fn persist_failure(
        &self,
        pending: &PendingEvent,
        mut failure: StageFailure,
    ) -> ProcessorOutcome {
        let Some(new_attempts) = pending.attempts.checked_add(1) else {
            failure = StageFailure::permanent(ProcessingErrorCode::RetryExhausted);
            return self.write_failure(pending, failure, u32::MAX, None).await;
        };
        let exhausted = new_attempts >= self.config.max_attempts;
        if exhausted && failure.class == StageFailureClass::Temporary {
            metrics::counter!("faultkeep_processor_retry_exhausted_total").increment(1);
            failure.class = StageFailureClass::Permanent;
        }
        let retry_at = if failure.class == StageFailureClass::Temporary {
            match retry_at(self.clock.now(), new_attempts, self.config) {
                Some(at) => Some(at),
                None => {
                    failure = StageFailure::permanent(ProcessingErrorCode::RetryExhausted);
                    None
                }
            }
        } else {
            None
        };
        self.write_failure(pending, failure, new_attempts, retry_at)
            .await
    }

    async fn write_failure(
        &self,
        pending: &PendingEvent,
        failure: StageFailure,
        new_attempts: u32,
        retry_at: Option<Timestamp>,
    ) -> ProcessorOutcome {
        let disposition = retry_at.map_or(
            ProcessingFailureDisposition::PermanentlyFailed,
            ProcessingFailureDisposition::RetryAt,
        );
        let update = ProcessingFailure {
            key: pending.key(),
            expected_attempts: pending.attempts,
            new_attempts,
            code: failure.code,
            disposition,
        };
        metrics::counter!(
            "faultkeep_processor_failures_total",
            "error_code" => failure.code.as_str(),
            "disposition" => if retry_at.is_some() { "retry" } else { "failed" }
        )
        .increment(1);
        match timeout(
            self.config.state_timeout,
            self.states.record_processing_failure(update),
        )
        .await
        {
            Ok(Ok(ProcessingStateChange::Updated)) if retry_at.is_some() => {
                ProcessorOutcome::RetryScheduled
            }
            Ok(Ok(ProcessingStateChange::Updated)) => ProcessorOutcome::PermanentlyFailed,
            Ok(Ok(ProcessingStateChange::StaleOrCompleted)) => ProcessorOutcome::StaleOrCompleted,
            Ok(Err(ProcessingStateError::InvalidData | ProcessingStateError::Unavailable))
            | Err(_) => ProcessorOutcome::StateUnavailable,
        }
    }
}

impl faultkeep_ports::WorkHandler for Processor {
    fn handle(&self, event: PendingEvent) -> PortFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.process(event).await;
        })
    }
}

async fn run_stage<T, F>(
    stage: &'static str,
    stage_timeout: Duration,
    total_deadline: Instant,
    cancellation: &CancellationToken,
    future: F,
) -> Result<T, StageFailure>
where
    F: Future<Output = T>,
{
    let started = Instant::now();
    let stage_deadline = (started + stage_timeout).min(total_deadline);
    let result = tokio::select! {
        () = cancellation.cancelled() => {
            Err(StageFailure::temporary(ProcessingErrorCode::Cancelled))
        }
        result = timeout_at(stage_deadline, future) => {
            result.map_err(|_| {
                if stage_deadline == total_deadline {
                    StageFailure::temporary(ProcessingErrorCode::TotalDeadline)
                } else {
                    StageFailure::temporary(ProcessingErrorCode::StageDeadline)
                }
            })
        }
    };
    metrics::histogram!(
        "faultkeep_processor_stage_duration_seconds",
        "stage" => stage,
        "outcome" => if result.is_ok() { "ok" } else { "failed" }
    )
    .record(started.elapsed().as_secs_f64());
    result
}

fn retry_at(now: Timestamp, attempts: u32, config: ProcessorConfig) -> Option<Timestamp> {
    let exponent = attempts.saturating_sub(1).min(31);
    let multiplier = 1_u32.checked_shl(exponent).unwrap_or(u32::MAX);
    let delay = config
        .retry_base
        .checked_mul(multiplier)
        .unwrap_or(config.retry_max)
        .min(config.retry_max);
    let millis = i64::try_from(delay.as_millis()).ok()?;
    Timestamp::from_unix_millis(now.unix_millis().checked_add(millis)?).ok()
}

const fn map_project_error(error: ProcessingProjectError) -> StageFailure {
    match error {
        ProcessingProjectError::NotFound => {
            StageFailure::permanent(ProcessingErrorCode::ProjectNotFound)
        }
        ProcessingProjectError::InvalidData => {
            StageFailure::permanent(ProcessingErrorCode::ProjectInvalidData)
        }
        ProcessingProjectError::Unavailable => {
            StageFailure::temporary(ProcessingErrorCode::ProjectUnavailable)
        }
    }
}

const fn map_normalization_error(error: NormalizationError) -> StageFailure {
    StageFailure::permanent(match error {
        NormalizationError::InvalidJson => ProcessingErrorCode::NormalizationInvalidJson,
        NormalizationError::InvalidRoot => ProcessingErrorCode::NormalizationInvalidRoot,
        NormalizationError::TooComplex => ProcessingErrorCode::NormalizationTooComplex,
        NormalizationError::IdentityFieldTooLarge => {
            ProcessingErrorCode::NormalizationIdentityTooLarge
        }
    })
}

const fn map_grouping_error(error: GroupingError) -> StageFailure {
    StageFailure::permanent(match error {
        GroupingError::UnsupportedRevision => ProcessingErrorCode::GroupingUnsupportedRevision,
        GroupingError::InputLimitExceeded => ProcessingErrorCode::GroupingInputLimit,
    })
}

const fn map_issue_error(error: IssueServiceError) -> StageFailure {
    StageFailure::permanent(match error {
        IssueServiceError::InvalidGroupingIdentity => ProcessingErrorCode::IssueInvalidIdentity,
        IssueServiceError::InvalidSummary => ProcessingErrorCode::IssueInvalidSummary,
        IssueServiceError::IdentityCollision
        | IssueServiceError::NotFound
        | IssueServiceError::InvalidData
        | IssueServiceError::Unavailable => ProcessingErrorCode::IssueInvalidIdentity,
    })
}

const fn map_finalizer_error(error: FinalizerError) -> StageFailure {
    match error {
        FinalizerError::Unavailable => {
            StageFailure::temporary(ProcessingErrorCode::FinalizerUnavailable)
        }
        FinalizerError::IdentityCollision => {
            StageFailure::permanent(ProcessingErrorCode::FinalizerIdentityCollision)
        }
        FinalizerError::InvalidConfig
        | FinalizerError::InvalidBatch
        | FinalizerError::DuplicateEvent
        | FinalizerError::InvalidIdentity
        | FinalizerError::OutputTooLarge => {
            StageFailure::permanent(ProcessingErrorCode::FinalizerInvalidData)
        }
    }
}

const fn outcome_name(outcome: ProcessorOutcome) -> &'static str {
    match outcome {
        ProcessorOutcome::Processed => "processed",
        ProcessorOutcome::RetryScheduled => "retry",
        ProcessorOutcome::PermanentlyFailed => "failed",
        ProcessorOutcome::StaleOrCompleted => "stale",
        ProcessorOutcome::StateUnavailable => "state_unavailable",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use faultkeep_domain::{
        EventId, ProjectId, ScrubbedEventPayload,
        event::{EventLevel, EventPlatform, NormalizedEventBody},
        grouping::group,
        processing::{ProcessingProject, ProcessingStateChange},
        symbolication::{SymbolicationDisposition, SymbolicationKind, SymbolicationStatus},
    };
    use faultkeep_ports::{ProcessingProjectError, ProcessingStateError};

    use super::*;

    struct FixedClock(Timestamp);

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    struct FakeProjects {
        result: Result<ProcessingProject, ProcessingProjectError>,
    }

    impl ProcessingProjectStore for FakeProjects {
        fn load_processing_project(
            &self,
            _project_id: ProjectId,
        ) -> PortFuture<'_, Result<ProcessingProject, ProcessingProjectError>> {
            let result = self.result;
            Box::pin(async move { result })
        }
    }

    struct FakeStates {
        result: ProcessingStateChange,
        updates: Mutex<Vec<ProcessingFailure>>,
    }

    impl FakeStates {
        fn new(result: ProcessingStateChange) -> Self {
            Self {
                result,
                updates: Mutex::new(Vec::new()),
            }
        }
    }

    impl ProcessingStateStore for FakeStates {
        fn record_processing_failure(
            &self,
            failure: ProcessingFailure,
        ) -> PortFuture<'_, Result<ProcessingStateChange, ProcessingStateError>> {
            Box::pin(async move {
                self.updates.lock().unwrap().push(failure);
                Ok(self.result)
            })
        }
    }

    struct ScriptedStages {
        failure: Option<(&'static str, StageFailure)>,
        delay: Option<(&'static str, Duration)>,
        order: Mutex<Vec<&'static str>>,
        active: AtomicUsize,
        maximum_active: AtomicUsize,
    }

    impl ScriptedStages {
        fn new(failure: Option<(&'static str, StageFailure)>) -> Self {
            Self {
                failure,
                delay: None,
                order: Mutex::new(Vec::new()),
                active: AtomicUsize::new(0),
                maximum_active: AtomicUsize::new(0),
            }
        }

        fn delayed(stage: &'static str, duration: Duration) -> Self {
            Self {
                delay: Some((stage, duration)),
                ..Self::new(None)
            }
        }

        async fn enter(&self, stage: &'static str) -> Result<StageGuard<'_>, StageFailure> {
            self.order.lock().unwrap().push(stage);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum_active.fetch_max(active, Ordering::SeqCst);
            if self.delay.is_some_and(|(name, _)| name == stage) {
                tokio::time::sleep(self.delay.unwrap().1).await;
            }
            if let Some((_, failure)) = self.failure.filter(|(name, _)| *name == stage) {
                self.active.fetch_sub(1, Ordering::SeqCst);
                return Err(failure);
            }
            Ok(StageGuard(self))
        }
    }

    struct StageGuard<'a>(&'a ScriptedStages);

    impl Drop for StageGuard<'_> {
        fn drop(&mut self) {
            self.0.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl NormalizationStage for ScriptedStages {
        fn normalize<'a>(
            &'a self,
            event: &'a AcceptedEvent,
        ) -> PortFuture<'a, Result<NormalizedEvent, StageFailure>> {
            Box::pin(async move {
                let _guard = self.enter("normalizer").await?;
                Normalizer::new(Default::default())
                    .unwrap()
                    .normalize(event)
                    .map_err(map_normalization_error)
            })
        }
    }

    impl SymbolicationStage for ScriptedStages {
        fn symbolicate<'a>(
            &'a self,
            event: &'a NormalizedEvent,
            _debug_file_revision: u64,
            _cancellation: &'a CancellationToken,
        ) -> PortFuture<'a, Result<SymbolicationResult, StageFailure>> {
            Box::pin(async move {
                let _guard = self.enter("symbolication").await?;
                Ok(BaselineSymbolicationService::symbolicate(event))
            })
        }
    }

    impl GroupingStage for ScriptedStages {
        fn group<'a>(
            &'a self,
            event: &'a NormalizedEvent,
            symbolication: &'a SymbolicationResult,
            revision: u64,
        ) -> PortFuture<'a, Result<GroupingResult, StageFailure>> {
            Box::pin(async move {
                let _guard = self.enter("grouper").await?;
                group(event.project_id, revision, &event.body, Some(symbolication))
                    .map_err(map_grouping_error)
            })
        }
    }

    impl IssuePreparationStage for ScriptedStages {
        fn prepare<'a>(
            &'a self,
            event: &'a NormalizedEvent,
            grouping: &'a GroupingResult,
        ) -> PortFuture<'a, Result<IssueOccurrence, StageFailure>> {
            Box::pin(async move {
                let _guard = self.enter("issue_service").await?;
                prepare_issue_occurrence(event, grouping).map_err(map_issue_error)
            })
        }
    }

    impl EventFinalizationStage for ScriptedStages {
        fn finalize<'a>(
            &'a self,
            _event: &'a NormalizedEvent,
            _symbolication: &'a SymbolicationResult,
            _grouping: &'a GroupingResult,
            _issue: IssueOccurrence,
        ) -> PortFuture<'a, Result<(), StageFailure>> {
            Box::pin(async move {
                let _guard = self.enter("finalizer").await?;
                Ok(())
            })
        }
    }

    fn accepted(seed: u8) -> AcceptedEvent {
        AcceptedEvent {
            project_id: ProjectId::new(7).unwrap(),
            event_id: EventId::from_bytes([seed; 16]),
            received_at: Timestamp::from_unix_millis(1_000).unwrap(),
            policy_revision: 1,
            payload: ScrubbedEventPayload::new(
                format!(
                    r#"{{"event_id":"{}","message":"boom","platform":"rust"}}"#,
                    format!("{seed:02x}").repeat(16)
                )
                .into_bytes(),
            ),
        }
    }

    fn project() -> ProcessingProject {
        ProcessingProject {
            project_id: ProjectId::new(7).unwrap(),
            state: ProjectAcceptanceState::Active,
            error_events_enabled: true,
            grouping_revision: 1,
            debug_file_revision: 0,
        }
    }

    fn config() -> ProcessorConfig {
        ProcessorConfig {
            max_concurrency: 2,
            max_attempts: 3,
            retry_base: Duration::from_millis(100),
            retry_max: Duration::from_secs(1),
            stage_timeout: Duration::from_millis(100),
            total_timeout: Duration::from_secs(1),
            state_timeout: Duration::from_millis(100),
        }
    }

    fn processor(
        project_result: Result<ProcessingProject, ProcessingProjectError>,
        states: Arc<FakeStates>,
        stages: Arc<ScriptedStages>,
        config: ProcessorConfig,
    ) -> Processor {
        Processor::new(
            Arc::new(FakeProjects {
                result: project_result,
            }),
            states,
            stages.clone(),
            stages.clone(),
            stages.clone(),
            stages.clone(),
            stages,
            Arc::new(FixedClock(Timestamp::from_unix_millis(10_000).unwrap())),
            config,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn complete_stage_state_matrix_maps_temporary_and_permanent_failures() {
        let cases = [
            (
                "project unavailable",
                Err(ProcessingProjectError::Unavailable),
                None,
                ProcessorOutcome::RetryScheduled,
                ProcessingErrorCode::ProjectUnavailable,
            ),
            (
                "normalizer permanent",
                Ok(project()),
                Some((
                    "normalizer",
                    StageFailure::permanent(ProcessingErrorCode::NormalizationInvalidJson),
                )),
                ProcessorOutcome::PermanentlyFailed,
                ProcessingErrorCode::NormalizationInvalidJson,
            ),
            (
                "symbolicator retry",
                Ok(project()),
                Some((
                    "symbolication",
                    StageFailure::temporary(ProcessingErrorCode::SymbolicationRetryable),
                )),
                ProcessorOutcome::RetryScheduled,
                ProcessingErrorCode::SymbolicationRetryable,
            ),
            (
                "grouper permanent",
                Ok(project()),
                Some((
                    "grouper",
                    StageFailure::permanent(ProcessingErrorCode::GroupingInputLimit),
                )),
                ProcessorOutcome::PermanentlyFailed,
                ProcessingErrorCode::GroupingInputLimit,
            ),
            (
                "issue permanent",
                Ok(project()),
                Some((
                    "issue_service",
                    StageFailure::permanent(ProcessingErrorCode::IssueInvalidSummary),
                )),
                ProcessorOutcome::PermanentlyFailed,
                ProcessingErrorCode::IssueInvalidSummary,
            ),
            (
                "finalizer retry",
                Ok(project()),
                Some((
                    "finalizer",
                    StageFailure::temporary(ProcessingErrorCode::FinalizerUnavailable),
                )),
                ProcessorOutcome::RetryScheduled,
                ProcessingErrorCode::FinalizerUnavailable,
            ),
        ];
        for (name, project_result, failure, expected, code) in cases {
            let states = Arc::new(FakeStates::new(ProcessingStateChange::Updated));
            let stages = Arc::new(ScriptedStages::new(failure));
            let outcome = processor(project_result, states.clone(), stages, config())
                .process(PendingEvent::fresh(accepted(1)))
                .await;
            assert_eq!(outcome, expected, "{name}");
            assert_eq!(states.updates.lock().unwrap()[0].code, code, "{name}");
        }
    }

    #[tokio::test]
    async fn success_is_strictly_ordered_and_fences_skip_all_stages() {
        let states = Arc::new(FakeStates::new(ProcessingStateChange::Updated));
        let stages = Arc::new(ScriptedStages::new(None));
        let outcome = processor(Ok(project()), states.clone(), stages.clone(), config())
            .process(PendingEvent::fresh(accepted(2)))
            .await;
        assert_eq!(outcome, ProcessorOutcome::Processed);
        assert!(states.updates.lock().unwrap().is_empty());
        assert_eq!(
            *stages.order.lock().unwrap(),
            [
                "normalizer",
                "symbolication",
                "grouper",
                "issue_service",
                "finalizer"
            ]
        );

        for (state, enabled, code) in [
            (
                ProjectAcceptanceState::Disabled,
                true,
                ProcessingErrorCode::ProjectFenced,
            ),
            (
                ProjectAcceptanceState::PendingDelete,
                true,
                ProcessingErrorCode::ProjectFenced,
            ),
            (
                ProjectAcceptanceState::Active,
                false,
                ProcessingErrorCode::ErrorCapabilityDisabled,
            ),
        ] {
            let states = Arc::new(FakeStates::new(ProcessingStateChange::Updated));
            let stages = Arc::new(ScriptedStages::new(None));
            let outcome = processor(
                Ok(ProcessingProject {
                    state,
                    error_events_enabled: enabled,
                    ..project()
                }),
                states.clone(),
                stages.clone(),
                config(),
            )
            .process(PendingEvent::fresh(accepted(3)))
            .await;
            assert_eq!(outcome, ProcessorOutcome::PermanentlyFailed);
            assert_eq!(states.updates.lock().unwrap()[0].code, code);
            assert!(stages.order.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn retry_backoff_exhaustion_and_stale_completion_are_deterministic() {
        let failure = Some((
            "symbolication",
            StageFailure::temporary(ProcessingErrorCode::SymbolicationRetryable),
        ));
        let states = Arc::new(FakeStates::new(ProcessingStateChange::Updated));
        let outcome = processor(
            Ok(project()),
            states.clone(),
            Arc::new(ScriptedStages::new(failure)),
            config(),
        )
        .process(PendingEvent::fresh(accepted(4)))
        .await;
        assert_eq!(outcome, ProcessorOutcome::RetryScheduled);
        assert_eq!(
            states.updates.lock().unwrap()[0].disposition,
            ProcessingFailureDisposition::RetryAt(Timestamp::from_unix_millis(10_100).unwrap())
        );

        let states = Arc::new(FakeStates::new(ProcessingStateChange::Updated));
        let outcome = processor(
            Ok(project()),
            states.clone(),
            Arc::new(ScriptedStages::new(failure)),
            config(),
        )
        .process(PendingEvent {
            event: accepted(5),
            attempts: 2,
        })
        .await;
        assert_eq!(outcome, ProcessorOutcome::PermanentlyFailed);
        assert_eq!(
            states.updates.lock().unwrap()[0].disposition,
            ProcessingFailureDisposition::PermanentlyFailed
        );

        let states = Arc::new(FakeStates::new(ProcessingStateChange::StaleOrCompleted));
        let outcome = processor(
            Ok(project()),
            states,
            Arc::new(ScriptedStages::new(failure)),
            config(),
        )
        .process(PendingEvent::fresh(accepted(6)))
        .await;
        assert_eq!(outcome, ProcessorOutcome::StaleOrCompleted);
    }

    #[tokio::test]
    async fn deadlines_cancellation_and_processor_concurrency_are_bounded() {
        let states = Arc::new(FakeStates::new(ProcessingStateChange::Updated));
        let stages = Arc::new(ScriptedStages::delayed(
            "symbolication",
            Duration::from_millis(50),
        ));
        let mut short = config();
        short.stage_timeout = Duration::from_millis(5);
        let outcome = processor(Ok(project()), states.clone(), stages, short)
            .process(PendingEvent::fresh(accepted(7)))
            .await;
        assert_eq!(outcome, ProcessorOutcome::RetryScheduled);
        assert_eq!(
            states.updates.lock().unwrap()[0].code,
            ProcessingErrorCode::StageDeadline
        );

        let states = Arc::new(FakeStates::new(ProcessingStateChange::Updated));
        let stages = Arc::new(ScriptedStages::new(None));
        let cancelled_processor = processor(Ok(project()), states.clone(), stages, config());
        cancelled_processor.cancel();
        assert_eq!(
            cancelled_processor
                .process(PendingEvent::fresh(accepted(8)))
                .await,
            ProcessorOutcome::RetryScheduled
        );
        assert_eq!(
            states.updates.lock().unwrap()[0].code,
            ProcessingErrorCode::Cancelled
        );

        let states = Arc::new(FakeStates::new(ProcessingStateChange::Updated));
        let stages = Arc::new(ScriptedStages::delayed(
            "finalizer",
            Duration::from_millis(20),
        ));
        let processor = Arc::new(processor(Ok(project()), states, stages.clone(), config()));
        let mut tasks = Vec::new();
        for seed in 20..28 {
            let processor = processor.clone();
            tasks.push(tokio::spawn(async move {
                processor.process(PendingEvent::fresh(accepted(seed))).await
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), ProcessorOutcome::Processed);
        }
        assert!(stages.maximum_active.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn default_config_and_error_registry_are_bounded() {
        assert!(ProcessorConfig::default().validate().is_ok());
        assert!(
            ProcessorConfig {
                max_concurrency: 0,
                ..ProcessorConfig::default()
            }
            .validate()
            .is_err()
        );
        assert_eq!(ProcessingErrorCode::FinalizerUnavailable.stored(), 52);
        let _ = NormalizedEventBody {
            occurred_at: Timestamp::from_unix_millis(1).unwrap(),
            platform: EventPlatform::Rust,
            level: EventLevel::Error,
            logger: None,
            message: None,
            transaction: None,
            release: None,
            dist: None,
            environment: None,
            fingerprint: Vec::new(),
            exceptions: Vec::new(),
            stacktrace: Vec::new(),
            tags: Vec::new(),
            request: None,
            user: None,
            contexts: Default::default(),
            breadcrumbs: Vec::new(),
            unknown: Default::default(),
        };
        let _ = SymbolicationResult {
            kind: SymbolicationKind::NotRequired,
            status: SymbolicationStatus::NotRequired,
            disposition: SymbolicationDisposition::Continue,
            raw: Vec::new(),
            derived: Vec::new(),
            missing_debug_ids: Vec::new(),
            diagnostics: Vec::new(),
        };
    }
}
