//! Bounded one-process scheduling for module-owned maintenance operations.

use std::{
    collections::BTreeMap,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use futures_util::FutureExt;
use metric_domain::Timestamp;
use metric_ports::{
    Clock, MaintenanceCursor, MaintenanceDisposition, MaintenanceRequest, MaintenanceStore,
    MaintenanceStoreError, MaintenanceTask,
};
use thiserror::Error;
use tokio::time::{MissedTickBehavior, interval, timeout};

use crate::shutdown::ShutdownSignal;

const TASKS: [MaintenanceTask; 8] = [
    MaintenanceTask::RetryBacklog,
    MaintenanceTask::EventRetention,
    MaintenanceTask::HourlyRetention,
    MaintenanceTask::CounterReconciliation,
    MaintenanceTask::UploadExpiry,
    MaintenanceTask::BlobOrphanRegistration,
    MaintenanceTask::MonitorTimeouts,
    MaintenanceTask::MonitorMissed,
];

#[derive(Debug, Clone, Copy)]
pub struct SchedulerConfig {
    pub poll_interval: Duration,
    pub maintenance_interval: Duration,
    pub reconciliation_interval: Duration,
    pub backlog_interval: Duration,
    pub task_timeout: Duration,
    pub retry_base: Duration,
    pub retry_max: Duration,
    pub batch_size: usize,
    pub event_retention: Duration,
    pub hourly_retention: Duration,
    pub archive_events: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            maintenance_interval: Duration::from_secs(60),
            reconciliation_interval: Duration::from_secs(5 * 60),
            backlog_interval: Duration::from_secs(5),
            task_timeout: Duration::from_secs(10),
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
            batch_size: 500,
            event_retention: Duration::from_secs(30 * 24 * 60 * 60),
            hourly_retention: Duration::from_secs(400 * 24 * 60 * 60),
            archive_events: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SchedulerStartError {
    #[error("Scheduler configuration is invalid")]
    InvalidConfiguration,
}

#[derive(Debug, Clone)]
struct TaskState {
    next_due: Timestamp,
    failures: u32,
    cursor: Option<MaintenanceCursor>,
    running: bool,
}

pub struct Scheduler {
    store: Arc<dyn MaintenanceStore>,
    clock: Arc<dyn Clock>,
    config: SchedulerConfig,
    states: Mutex<BTreeMap<&'static str, TaskState>>,
}

impl Scheduler {
    pub fn new(
        store: Arc<dyn MaintenanceStore>,
        clock: Arc<dyn Clock>,
        config: SchedulerConfig,
    ) -> Result<Arc<Self>, SchedulerStartError> {
        validate_config(config)?;
        let now = clock.now();
        let states = TASKS
            .into_iter()
            .map(|task| {
                (
                    task.name(),
                    TaskState {
                        next_due: now,
                        failures: 0,
                        cursor: None,
                        running: false,
                    },
                )
            })
            .collect();
        Ok(Arc::new(Self {
            store,
            clock,
            config,
            states: Mutex::new(states),
        }))
    }

    pub async fn start(
        store: Arc<dyn MaintenanceStore>,
        clock: Arc<dyn Clock>,
        config: SchedulerConfig,
        shutdown: ShutdownSignal,
    ) -> Result<(Arc<Self>, SchedulerTask), SchedulerStartError> {
        let scheduler = Self::new(store, clock, config)?;
        scheduler.run_due_once().await;
        let join = tokio::spawn(run_scheduler(Arc::clone(&scheduler), shutdown));
        Ok((scheduler, SchedulerTask { join }))
    }

    /// Runs each currently due task at most once. This is public so deterministic
    /// fake-clock tests and administrative diagnostics do not need real sleeps.
    pub async fn run_due_once(&self) {
        let now = self.clock.now();
        let due = {
            let mut states = lock(&self.states);
            TASKS
                .into_iter()
                .filter_map(|task| {
                    let state = states
                        .get_mut(task.name())
                        .expect("every static Scheduler task has state");
                    if state.running || state.next_due > now {
                        if state.running && state.next_due <= now {
                            metrics::counter!(
                                "metric_scheduler_runs_total",
                                "task" => task.name(),
                                "outcome" => "lease_busy"
                            )
                            .increment(1);
                        }
                        return None;
                    }
                    state.running = true;
                    Some((task, state.next_due, state.cursor.clone()))
                })
                .collect::<Vec<_>>()
        };

        for (task, scheduled_at, cursor) in due {
            self.run_task(task, scheduled_at, cursor).await;
        }
    }

    fn interval_for(&self, task: MaintenanceTask) -> Duration {
        match task {
            MaintenanceTask::RetryBacklog
            | MaintenanceTask::MonitorTimeouts
            | MaintenanceTask::MonitorMissed => self.config.backlog_interval,
            MaintenanceTask::CounterReconciliation => self.config.reconciliation_interval,
            MaintenanceTask::EventRetention
            | MaintenanceTask::HourlyRetention
            | MaintenanceTask::UploadExpiry
            | MaintenanceTask::BlobOrphanRegistration => self.config.maintenance_interval,
        }
    }

    async fn run_task(
        &self,
        task: MaintenanceTask,
        scheduled_at: Timestamp,
        cursor: Option<MaintenanceCursor>,
    ) {
        let started_at = self.clock.now();
        let lag = started_at
            .unix_millis()
            .saturating_sub(scheduled_at.unix_millis())
            .max(0) as f64
            / 1_000.0;
        metrics::histogram!(
            "metric_scheduler_task_lag_seconds",
            "task" => task.name()
        )
        .record(lag);
        let request = MaintenanceRequest {
            task,
            now: started_at,
            cursor,
            batch_size: self.config.batch_size,
            event_retention: self.config.event_retention,
            hourly_retention: self.config.hourly_retention,
            archive_events: self.config.archive_events,
        };
        let wall_started = Instant::now();
        let result = AssertUnwindSafe(timeout(self.config.task_timeout, self.store.run(request)))
            .catch_unwind()
            .await;
        metrics::histogram!(
            "metric_scheduler_task_duration_seconds",
            "task" => task.name()
        )
        .record(wall_started.elapsed().as_secs_f64());

        let completed_at = self.clock.now();
        let mut states = lock(&self.states);
        let state = states
            .get_mut(task.name())
            .expect("every static Scheduler task has state");
        state.running = false;
        match result {
            Ok(Ok(Ok(report))) => {
                let disposition = match report.disposition {
                    MaintenanceDisposition::Completed => "ok",
                    MaintenanceDisposition::Disabled => "disabled",
                };
                metrics::counter!(
                    "metric_scheduler_runs_total",
                    "task" => task.name(),
                    "outcome" => disposition
                )
                .increment(1);
                metrics::histogram!(
                    "metric_scheduler_items_scanned",
                    "task" => task.name()
                )
                .record(report.scanned as f64);
                metrics::histogram!(
                    "metric_scheduler_items_changed",
                    "task" => task.name()
                )
                .record(report.changed as f64);
                state.failures = 0;
                state.cursor = report.next_cursor;
                let delay = if state.cursor.is_some() {
                    self.config.poll_interval
                } else {
                    self.interval_for(task)
                };
                state.next_due = add_duration(completed_at, delay);
            }
            Ok(Ok(Err(error))) => {
                let outcome = match error {
                    MaintenanceStoreError::InvalidData => "invalid_data",
                    MaintenanceStoreError::Unavailable => "unavailable",
                };
                record_failure(self.config, task, state, completed_at, outcome);
            }
            Ok(Err(_elapsed)) => {
                record_failure(self.config, task, state, completed_at, "timeout");
            }
            Err(_panic) => {
                record_failure(self.config, task, state, completed_at, "panic");
            }
        }
    }
}

pub struct SchedulerTask {
    join: tokio::task::JoinHandle<()>,
}

impl SchedulerTask {
    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

async fn run_scheduler(scheduler: Arc<Scheduler>, shutdown: ShutdownSignal) {
    let mut tick = interval(scheduler.config.poll_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tick.tick().await;
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            _ = tick.tick() => scheduler.run_due_once().await,
        }
    }
}

fn record_failure(
    config: SchedulerConfig,
    task: MaintenanceTask,
    state: &mut TaskState,
    now: Timestamp,
    outcome: &'static str,
) {
    state.failures = state.failures.saturating_add(1);
    let multiplier = 1_u32
        .checked_shl(state.failures.saturating_sub(1).min(31))
        .unwrap_or(u32::MAX);
    let delay = config
        .retry_base
        .checked_mul(multiplier)
        .unwrap_or(config.retry_max)
        .min(config.retry_max);
    state.next_due = add_duration(now, delay);
    metrics::counter!(
        "metric_scheduler_runs_total",
        "task" => task.name(),
        "outcome" => outcome
    )
    .increment(1);
    metrics::histogram!(
        "metric_scheduler_retry_delay_seconds",
        "task" => task.name()
    )
    .record(delay.as_secs_f64());
}

fn validate_config(config: SchedulerConfig) -> Result<(), SchedulerStartError> {
    let valid = !config.poll_interval.is_zero()
        && !config.maintenance_interval.is_zero()
        && !config.reconciliation_interval.is_zero()
        && !config.backlog_interval.is_zero()
        && !config.task_timeout.is_zero()
        && !config.retry_base.is_zero()
        && config.retry_base <= config.retry_max
        && (1..=10_000).contains(&config.batch_size)
        && !config.event_retention.is_zero()
        && !config.hourly_retention.is_zero();
    valid
        .then_some(())
        .ok_or(SchedulerStartError::InvalidConfiguration)
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
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{
            Mutex,
            atomic::{AtomicI64, AtomicUsize, Ordering},
        },
    };

    use metric_ports::{MaintenanceDisposition, MaintenanceResult, PortFuture};
    use tokio::sync::Notify;

    use super::*;

    struct TestClock(AtomicI64);

    impl TestClock {
        fn set(&self, millis: i64) {
            self.0.store(millis, Ordering::Release);
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_unix_millis(self.0.load(Ordering::Acquire)).unwrap()
        }
    }

    #[derive(Default)]
    struct FakeStore {
        calls: Mutex<Vec<MaintenanceTask>>,
        scripted: Mutex<
            BTreeMap<&'static str, VecDeque<Result<MaintenanceResult, MaintenanceStoreError>>>,
        >,
        active: AtomicUsize,
        maximum_active: AtomicUsize,
        block: Notify,
        blocking: AtomicUsize,
        panics: Mutex<BTreeMap<&'static str, usize>>,
    }

    impl FakeStore {
        fn completed() -> MaintenanceResult {
            MaintenanceResult {
                scanned: 1,
                changed: 1,
                next_cursor: None,
                disposition: MaintenanceDisposition::Completed,
            }
        }

        fn calls(&self, task: MaintenanceTask) -> usize {
            lock(&self.calls)
                .iter()
                .filter(|candidate| **candidate == task)
                .count()
        }
    }

    impl MaintenanceStore for FakeStore {
        fn run(
            &self,
            request: MaintenanceRequest,
        ) -> PortFuture<'_, Result<MaintenanceResult, MaintenanceStoreError>> {
            Box::pin(async move {
                lock(&self.calls).push(request.task);
                let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
                self.maximum_active.fetch_max(active, Ordering::AcqRel);
                if self.blocking.load(Ordering::Acquire) > 0 {
                    self.block.notified().await;
                }
                self.active.fetch_sub(1, Ordering::AcqRel);
                let should_panic =
                    lock(&self.panics)
                        .get_mut(request.task.name())
                        .is_some_and(|remaining| {
                            if *remaining == 0 {
                                false
                            } else {
                                *remaining -= 1;
                                true
                            }
                        });
                assert!(!should_panic, "scripted maintenance panic");
                lock(&self.scripted)
                    .get_mut(request.task.name())
                    .and_then(VecDeque::pop_front)
                    .unwrap_or_else(|| Ok(Self::completed()))
            })
        }
    }

    fn config() -> SchedulerConfig {
        SchedulerConfig {
            poll_interval: Duration::from_millis(10),
            maintenance_interval: Duration::from_millis(100),
            reconciliation_interval: Duration::from_millis(200),
            backlog_interval: Duration::from_millis(50),
            task_timeout: Duration::from_secs(1),
            retry_base: Duration::from_millis(20),
            retry_max: Duration::from_millis(80),
            batch_size: 10,
            event_retention: Duration::from_secs(60),
            hourly_retention: Duration::from_secs(120),
            archive_events: false,
        }
    }

    #[tokio::test]
    async fn fake_clock_controls_due_tasks_and_incomplete_passes() {
        let clock = Arc::new(TestClock(AtomicI64::new(1_000)));
        let store = Arc::new(FakeStore::default());
        lock(&store.scripted).insert(
            MaintenanceTask::EventRetention.name(),
            VecDeque::from([
                Ok(MaintenanceResult {
                    next_cursor: MaintenanceCursor::new(vec![1]),
                    ..FakeStore::completed()
                }),
                Ok(FakeStore::completed()),
            ]),
        );
        let scheduler = Scheduler::new(store.clone(), clock.clone(), config()).unwrap();

        scheduler.run_due_once().await;
        assert_eq!(store.calls(MaintenanceTask::EventRetention), 1);
        scheduler.run_due_once().await;
        assert_eq!(store.calls(MaintenanceTask::RetryBacklog), 1);

        clock.set(1_010);
        scheduler.run_due_once().await;
        assert_eq!(store.calls(MaintenanceTask::EventRetention), 2);
        clock.set(1_060);
        scheduler.run_due_once().await;
        assert_eq!(store.calls(MaintenanceTask::RetryBacklog), 2);
        assert_eq!(store.calls(MaintenanceTask::CounterReconciliation), 1);
    }

    #[tokio::test]
    async fn failure_is_isolated_retries_and_restart_is_idempotent() {
        let clock = Arc::new(TestClock(AtomicI64::new(2_000)));
        let store = Arc::new(FakeStore::default());
        lock(&store.panics).insert(MaintenanceTask::EventRetention.name(), 1);
        lock(&store.scripted).insert(
            MaintenanceTask::EventRetention.name(),
            VecDeque::from([Ok(FakeStore::completed()), Ok(FakeStore::completed())]),
        );
        let scheduler = Scheduler::new(store.clone(), clock.clone(), config()).unwrap();
        scheduler.run_due_once().await;
        assert_eq!(store.calls(MaintenanceTask::HourlyRetention), 1);
        assert_eq!(store.calls(MaintenanceTask::EventRetention), 1);

        clock.set(2_019);
        scheduler.run_due_once().await;
        assert_eq!(store.calls(MaintenanceTask::EventRetention), 1);
        clock.set(2_020);
        scheduler.run_due_once().await;
        assert_eq!(store.calls(MaintenanceTask::EventRetention), 2);

        drop(scheduler);
        let restarted = Scheduler::new(store.clone(), clock, config()).unwrap();
        restarted.run_due_once().await;
        assert_eq!(store.calls(MaintenanceTask::EventRetention), 3);
    }

    #[tokio::test]
    async fn local_lease_prevents_overlapping_ticks() {
        let clock = Arc::new(TestClock(AtomicI64::new(3_000)));
        let store = Arc::new(FakeStore::default());
        store.blocking.store(1, Ordering::Release);
        let scheduler = Scheduler::new(store.clone(), clock, config()).unwrap();
        let first = {
            let scheduler = Arc::clone(&scheduler);
            tokio::spawn(async move { scheduler.run_due_once().await })
        };
        tokio::task::yield_now().await;
        scheduler.run_due_once().await;
        assert_eq!(store.calls(MaintenanceTask::RetryBacklog), 1);
        store.blocking.store(0, Ordering::Release);
        store.block.notify_waiters();
        first.await.unwrap();
    }

    #[test]
    fn rejects_unbounded_configuration() {
        assert!(matches!(
            Scheduler::new(
                Arc::new(FakeStore::default()),
                Arc::new(TestClock(AtomicI64::new(0))),
                SchedulerConfig {
                    batch_size: 0,
                    ..config()
                }
            ),
            Err(SchedulerStartError::InvalidConfiguration)
        ));
    }
}
