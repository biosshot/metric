//! Bounded in-process scheduling over the durable pending Event backlog.

use std::{
    collections::HashSet,
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use faultkeep_domain::{AcceptedEvent, EventKey};
use faultkeep_ports::{AcceptedEventHandoff, Clock, EventBacklog, EventBacklogError, WorkHandler};
use futures_util::FutureExt;
use thiserror::Error;
use tokio::{
    sync::mpsc,
    task::{JoinError, JoinSet},
    time::{MissedTickBehavior, interval, timeout, timeout_at},
};

use crate::shutdown::ShutdownSignal;

#[derive(Debug, Clone, Copy)]
pub struct DispatcherConfig {
    pub queue_capacity: usize,
    pub worker_concurrency: usize,
    pub low_watermark: usize,
    pub refill_target: usize,
    pub refill_batch_size: usize,
    pub poll_interval: Duration,
    pub metrics_interval: Duration,
    pub source_timeout: Duration,
    pub shutdown_drain: Duration,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 4_096,
            worker_concurrency: 32,
            low_watermark: 1_024,
            refill_target: 3_072,
            refill_batch_size: 512,
            poll_interval: Duration::from_millis(100),
            metrics_interval: Duration::from_secs(5),
            source_timeout: Duration::from_secs(5),
            shutdown_drain: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DispatcherStartError {
    #[error("Dispatcher configuration is invalid")]
    InvalidConfiguration,
    #[error("initial pending Event refill is temporarily unavailable")]
    BacklogUnavailable,
    #[error("initial pending Event refill returned invalid data")]
    BacklogInvalidData,
}

pub struct Dispatcher {
    sender: mpsc::Sender<AcceptedEvent>,
    keys: Arc<Mutex<HashSet<EventKey>>>,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
}

impl Dispatcher {
    pub async fn start(
        source: Arc<dyn EventBacklog>,
        handler: Arc<dyn WorkHandler>,
        clock: Arc<dyn Clock>,
        config: DispatcherConfig,
        shutdown: ShutdownSignal,
    ) -> Result<(Arc<Self>, DispatcherTask), DispatcherStartError> {
        validate_config(config)?;
        let (sender, receiver) = mpsc::channel(config.queue_capacity);
        let dispatcher = Arc::new(Self {
            sender,
            keys: Arc::new(Mutex::new(HashSet::with_capacity(
                config
                    .queue_capacity
                    .saturating_add(config.worker_concurrency),
            ))),
            accepting: Arc::new(AtomicBool::new(true)),
            shutdown: shutdown.clone(),
        });
        refill_once(&dispatcher, &source, &clock, config, true).await?;
        let join = tokio::spawn(run_dispatcher(
            Arc::clone(&dispatcher),
            source,
            handler,
            clock,
            receiver,
            config,
            shutdown,
        ));
        Ok((dispatcher, DispatcherTask { join }))
    }

    #[must_use]
    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire) && !self.shutdown.is_cancelled()
    }

    #[must_use]
    pub fn queued_depth(&self) -> usize {
        self.sender
            .max_capacity()
            .saturating_sub(self.sender.capacity())
    }

    #[must_use]
    pub fn scheduled_keys(&self) -> usize {
        lock(&self.keys).len()
    }

    fn offer_with_source(
        &self,
        event: AcceptedEvent,
        source: &'static str,
    ) -> Result<(), AcceptedEvent> {
        if !self.is_accepting() {
            record_admission(source, "closed");
            return Err(event);
        }
        let key = EventKey::new(event.project_id, event.event_id);
        {
            let mut keys = lock(&self.keys);
            if !keys.insert(key) {
                record_admission(source, "duplicate");
                return Ok(());
            }
        }
        match self.sender.try_send(event) {
            Ok(()) => {
                record_admission(source, "queued");
                self.record_depth();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(event)) => {
                lock(&self.keys).remove(&key);
                record_admission(source, "full");
                Err(event)
            }
            Err(mpsc::error::TrySendError::Closed(event)) => {
                lock(&self.keys).remove(&key);
                record_admission(source, "closed");
                Err(event)
            }
        }
    }

    fn record_depth(&self) {
        metrics::gauge!("faultkeep_dispatcher_queue_depth").set(self.queued_depth() as f64);
        metrics::gauge!("faultkeep_dispatcher_scheduled_keys").set(self.scheduled_keys() as f64);
    }
}

impl AcceptedEventHandoff for Dispatcher {
    fn offer(&self, event: AcceptedEvent) -> Result<(), AcceptedEvent> {
        self.offer_with_source(event, "fresh")
    }
}

pub struct DispatcherTask {
    join: tokio::task::JoinHandle<()>,
}

impl DispatcherTask {
    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

fn validate_config(config: DispatcherConfig) -> Result<(), DispatcherStartError> {
    let valid = (1..=100_000).contains(&config.queue_capacity)
        && (1..=4_096).contains(&config.worker_concurrency)
        && config.worker_concurrency <= config.queue_capacity
        && config.low_watermark < config.refill_target
        && config.refill_target <= config.queue_capacity
        && (1..=config.refill_target.min(32_768)).contains(&config.refill_batch_size)
        && !config.poll_interval.is_zero()
        && !config.metrics_interval.is_zero()
        && !config.source_timeout.is_zero()
        && !config.shutdown_drain.is_zero();
    if valid {
        Ok(())
    } else {
        Err(DispatcherStartError::InvalidConfiguration)
    }
}

async fn run_dispatcher(
    dispatcher: Arc<Dispatcher>,
    source: Arc<dyn EventBacklog>,
    handler: Arc<dyn WorkHandler>,
    clock: Arc<dyn Clock>,
    mut receiver: mpsc::Receiver<AcceptedEvent>,
    config: DispatcherConfig,
    shutdown: ShutdownSignal,
) {
    let mut running = JoinSet::new();
    let mut refill_tick = interval(config.poll_interval);
    refill_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    refill_tick.tick().await;
    let mut metrics_tick = interval(config.metrics_interval);
    metrics_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    metrics_tick.tick().await;

    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            completed = running.join_next(), if !running.is_empty() => {
                complete_work(&dispatcher, completed);
            }
            event = receiver.recv(), if running.len() < config.worker_concurrency => {
                match event {
                    Some(event) => spawn_work(&mut running, Arc::clone(&handler), event),
                    None => break,
                }
                dispatcher.record_depth();
            }
            _ = refill_tick.tick() => {
                if dispatcher.queued_depth() <= config.low_watermark {
                    let _ = refill_once(&dispatcher, &source, &clock, config, false).await;
                }
            }
            _ = metrics_tick.tick() => {
                observe_backlog(&source, &clock, config.source_timeout).await;
            }
        }
    }

    dispatcher.accepting.store(false, Ordering::Release);
    receiver.close();
    drain_dispatcher(&dispatcher, &handler, &mut receiver, &mut running, config).await;
}

fn spawn_work(
    running: &mut JoinSet<(EventKey, bool)>,
    handler: Arc<dyn WorkHandler>,
    event: AcceptedEvent,
) {
    let key = EventKey::new(event.project_id, event.event_id);
    running.spawn(async move {
        let completed = AssertUnwindSafe(handler.handle(event))
            .catch_unwind()
            .await
            .is_ok();
        (key, completed)
    });
}

fn complete_work(dispatcher: &Dispatcher, completed: Option<Result<(EventKey, bool), JoinError>>) {
    match completed {
        Some(Ok((key, durable_completion))) => {
            lock(&dispatcher.keys).remove(&key);
            let outcome = if durable_completion {
                "completed"
            } else {
                "handler_failed"
            };
            metrics::counter!("faultkeep_dispatcher_work_total", "outcome" => outcome).increment(1);
        }
        Some(Err(_)) => {
            metrics::counter!("faultkeep_dispatcher_work_total", "outcome" => "cancelled")
                .increment(1);
        }
        None => {}
    }
    dispatcher.record_depth();
}

async fn refill_once(
    dispatcher: &Arc<Dispatcher>,
    source: &Arc<dyn EventBacklog>,
    clock: &Arc<dyn Clock>,
    config: DispatcherConfig,
    startup: bool,
) -> Result<(), DispatcherStartError> {
    let depth = dispatcher.queued_depth();
    if depth >= config.refill_target {
        return Ok(());
    }
    let wanted = config
        .refill_target
        .saturating_sub(depth)
        .min(config.refill_batch_size);
    let excluded = lock(&dispatcher.keys).iter().copied().collect::<Vec<_>>();
    let started = Instant::now();
    let loaded = timeout(
        config.source_timeout,
        source.load_due(clock.now(), wanted, &excluded),
    )
    .await;
    metrics::histogram!("faultkeep_dispatcher_refill_duration_seconds")
        .record(started.elapsed().as_secs_f64());
    let events = match loaded {
        Ok(Ok(events)) => events,
        Ok(Err(EventBacklogError::InvalidData)) => {
            record_refill("invalid", 0);
            return if startup {
                Err(DispatcherStartError::BacklogInvalidData)
            } else {
                Ok(())
            };
        }
        Ok(Err(EventBacklogError::Unavailable)) | Err(_) => {
            record_refill("unavailable", 0);
            return if startup {
                Err(DispatcherStartError::BacklogUnavailable)
            } else {
                Ok(())
            };
        }
    };
    let count = events.len();
    for event in events {
        let _ = dispatcher.offer_with_source(event, "refill");
    }
    record_refill("ok", count);
    Ok(())
}

async fn observe_backlog(
    source: &Arc<dyn EventBacklog>,
    clock: &Arc<dyn Clock>,
    source_timeout: Duration,
) {
    let Ok(Ok(observation)) = timeout(source_timeout, source.observe()).await else {
        metrics::counter!("faultkeep_dispatcher_observation_total", "outcome" => "unavailable")
            .increment(1);
        return;
    };
    metrics::gauge!("faultkeep_dispatcher_pending_estimate").set(observation.pending_count as f64);
    let age_seconds = observation.oldest_pending_at.map_or(0.0, |oldest| {
        clock
            .now()
            .unix_millis()
            .saturating_sub(oldest.unix_millis())
            .max(0) as f64
            / 1_000.0
    });
    metrics::gauge!("faultkeep_dispatcher_oldest_pending_age_seconds").set(age_seconds);
    metrics::counter!("faultkeep_dispatcher_observation_total", "outcome" => "ok").increment(1);
}

async fn drain_dispatcher(
    dispatcher: &Dispatcher,
    handler: &Arc<dyn WorkHandler>,
    receiver: &mut mpsc::Receiver<AcceptedEvent>,
    running: &mut JoinSet<(EventKey, bool)>,
    config: DispatcherConfig,
) {
    let deadline = tokio::time::Instant::now() + config.shutdown_drain;
    loop {
        while running.len() < config.worker_concurrency {
            match receiver.try_recv() {
                Ok(event) => spawn_work(running, Arc::clone(handler), event),
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    break;
                }
            }
        }
        if running.is_empty() && receiver.is_empty() {
            dispatcher.record_depth();
            return;
        }
        match timeout_at(deadline, running.join_next()).await {
            Ok(completed) => complete_work(dispatcher, completed),
            Err(_) => {
                running.abort_all();
                while running.join_next().await.is_some() {}
                metrics::counter!("faultkeep_dispatcher_shutdown_total", "outcome" => "deadline")
                    .increment(1);
                dispatcher.record_depth();
                return;
            }
        }
    }
}

fn record_admission(source: &'static str, outcome: &'static str) {
    metrics::counter!(
        "faultkeep_dispatcher_admission_total",
        "source" => source,
        "outcome" => outcome
    )
    .increment(1);
}

fn record_refill(outcome: &'static str, count: usize) {
    metrics::counter!("faultkeep_dispatcher_refill_total", "outcome" => outcome).increment(1);
    metrics::histogram!("faultkeep_dispatcher_refill_events").record(count as f64);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::atomic::AtomicUsize};

    use faultkeep_domain::{EventId, ProjectId, ScrubbedEventPayload, Timestamp};
    use faultkeep_ports::{BacklogObservation, PortFuture};
    use tokio::sync::Notify;

    use super::*;
    use crate::shutdown::ShutdownRoot;

    struct TestClock(Timestamp);

    impl Clock for TestClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    struct FakeBacklog {
        events: Mutex<BTreeMap<EventKey, (Timestamp, AcceptedEvent)>>,
        fail: AtomicBool,
    }

    impl FakeBacklog {
        fn new(events: impl IntoIterator<Item = (Timestamp, AcceptedEvent)>) -> Self {
            Self {
                events: Mutex::new(
                    events
                        .into_iter()
                        .map(|(due, event)| (key(&event), (due, event)))
                        .collect(),
                ),
                fail: AtomicBool::new(false),
            }
        }

        fn complete(&self, key: EventKey) {
            lock(&self.events).remove(&key);
        }
    }

    impl EventBacklog for FakeBacklog {
        fn load_due<'a>(
            &'a self,
            now: Timestamp,
            limit: usize,
            excluded: &'a [EventKey],
        ) -> PortFuture<'a, Result<Vec<AcceptedEvent>, EventBacklogError>> {
            Box::pin(async move {
                if self.fail.load(Ordering::Relaxed) {
                    return Err(EventBacklogError::Unavailable);
                }
                let mut events = lock(&self.events)
                    .values()
                    .filter(|(due, event)| *due <= now && !excluded.contains(&key(event)))
                    .cloned()
                    .collect::<Vec<_>>();
                events.sort_by_key(|(due, event)| (*due, event.received_at, key(event)));
                Ok(events
                    .into_iter()
                    .take(limit)
                    .map(|(_, event)| event)
                    .collect())
            })
        }

        fn observe(&self) -> PortFuture<'_, Result<BacklogObservation, EventBacklogError>> {
            Box::pin(async move {
                let events = lock(&self.events);
                Ok(BacklogObservation {
                    pending_count: events.len() as u64,
                    oldest_pending_at: events.values().map(|(_, event)| event.received_at).min(),
                })
            })
        }
    }

    struct FakeHandler {
        backlog: Arc<FakeBacklog>,
        handled: Mutex<Vec<EventKey>>,
        gate: Option<Arc<Notify>>,
        started: AtomicUsize,
    }

    impl WorkHandler for FakeHandler {
        fn handle(&self, event: AcceptedEvent) -> PortFuture<'_, ()> {
            Box::pin(async move {
                self.started.fetch_add(1, Ordering::Relaxed);
                if let Some(gate) = &self.gate {
                    gate.notified().await;
                }
                let key = key(&event);
                self.backlog.complete(key);
                lock(&self.handled).push(key);
            })
        }
    }

    fn event(byte: u8, received: i64) -> AcceptedEvent {
        AcceptedEvent {
            project_id: ProjectId::new(42).unwrap(),
            event_id: EventId::from_bytes([byte; 16]),
            received_at: Timestamp::from_unix_millis(received).unwrap(),
            policy_revision: 1,
            payload: ScrubbedEventPayload::new(
                format!(r#"{{"event_id":"{}"}}"#, format!("{byte:02x}").repeat(16)).into_bytes(),
            ),
        }
    }

    fn key(event: &AcceptedEvent) -> EventKey {
        EventKey::new(event.project_id, event.event_id)
    }

    fn config() -> DispatcherConfig {
        DispatcherConfig {
            queue_capacity: 8,
            worker_concurrency: 2,
            low_watermark: 2,
            refill_target: 6,
            refill_batch_size: 6,
            poll_interval: Duration::from_millis(5),
            metrics_interval: Duration::from_secs(1),
            source_timeout: Duration::from_secs(1),
            shutdown_drain: Duration::from_secs(1),
        }
    }

    async fn wait_until(mut predicate: impl FnMut() -> bool) {
        timeout(Duration::from_secs(2), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn startup_refill_orders_due_work_and_skips_future_retry() {
        let now = Timestamp::from_unix_millis(10_000).unwrap();
        let backlog = Arc::new(FakeBacklog::new([
            (Timestamp::from_unix_millis(9_000).unwrap(), event(2, 2_000)),
            (Timestamp::from_unix_millis(8_000).unwrap(), event(1, 1_000)),
            (
                Timestamp::from_unix_millis(11_000).unwrap(),
                event(3, 3_000),
            ),
        ]));
        let handler = Arc::new(FakeHandler {
            backlog: Arc::clone(&backlog),
            handled: Mutex::new(Vec::new()),
            gate: None,
            started: AtomicUsize::new(0),
        });
        let root = ShutdownRoot::new();
        let (_, task) = Dispatcher::start(
            backlog,
            handler.clone(),
            Arc::new(TestClock(now)),
            config(),
            root.signal(),
        )
        .await
        .unwrap();
        wait_until(|| lock(&handler.handled).len() == 2).await;
        assert_eq!(
            lock(&handler.handled).as_slice(),
            [key(&event(1, 1_000)), key(&event(2, 2_000))]
        );
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn duplicate_fresh_offer_is_not_run_concurrently() {
        let now = Timestamp::from_unix_millis(10_000).unwrap();
        let first = event(1, 1_000);
        let backlog = Arc::new(FakeBacklog::new([(now, first.clone())]));
        let gate = Arc::new(Notify::new());
        let handler = Arc::new(FakeHandler {
            backlog: Arc::clone(&backlog),
            handled: Mutex::new(Vec::new()),
            gate: Some(Arc::clone(&gate)),
            started: AtomicUsize::new(0),
        });
        let root = ShutdownRoot::new();
        let (dispatcher, task) = Dispatcher::start(
            backlog,
            handler.clone(),
            Arc::new(TestClock(now)),
            config(),
            root.signal(),
        )
        .await
        .unwrap();
        wait_until(|| handler.started.load(Ordering::Relaxed) == 1).await;
        assert!(dispatcher.offer(first).is_ok());
        assert_eq!(handler.started.load(Ordering::Relaxed), 1);
        gate.notify_one();
        wait_until(|| lock(&handler.handled).len() == 1).await;
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn full_queue_releases_fresh_payload_and_shutdown_rejects_new_work() {
        let now = Timestamp::from_unix_millis(10_000).unwrap();
        let backlog = Arc::new(FakeBacklog::new([]));
        let gate = Arc::new(Notify::new());
        let handler = Arc::new(FakeHandler {
            backlog: Arc::clone(&backlog),
            handled: Mutex::new(Vec::new()),
            gate: Some(Arc::clone(&gate)),
            started: AtomicUsize::new(0),
        });
        let root = ShutdownRoot::new();
        let mut bounded = config();
        bounded.queue_capacity = 2;
        bounded.worker_concurrency = 1;
        bounded.low_watermark = 0;
        bounded.refill_target = 2;
        bounded.refill_batch_size = 2;
        let (dispatcher, task) = Dispatcher::start(
            backlog,
            handler,
            Arc::new(TestClock(now)),
            bounded,
            root.signal(),
        )
        .await
        .unwrap();
        assert!(dispatcher.offer(event(1, 1)).is_ok());
        wait_until(|| dispatcher.queued_depth() == 0).await;
        assert!(dispatcher.offer(event(2, 2)).is_ok());
        assert!(dispatcher.offer(event(3, 3)).is_ok());
        let rejected = dispatcher.offer(event(4, 4)).unwrap_err();
        assert_eq!(rejected.event_id, EventId::from_bytes([4; 16]));
        root.begin();
        assert!(dispatcher.offer(event(5, 5)).is_err());
        gate.notify_waiters();
        task.wait().await;
    }

    #[tokio::test]
    async fn startup_failure_is_fail_closed() {
        let backlog = Arc::new(FakeBacklog::new([]));
        backlog.fail.store(true, Ordering::Relaxed);
        let root = ShutdownRoot::new();
        let result = Dispatcher::start(
            backlog.clone(),
            Arc::new(FakeHandler {
                backlog,
                handled: Mutex::new(Vec::new()),
                gate: None,
                started: AtomicUsize::new(0),
            }),
            Arc::new(TestClock(Timestamp::from_unix_millis(1).unwrap())),
            config(),
            root.signal(),
        )
        .await;
        assert!(matches!(
            result,
            Err(DispatcherStartError::BacklogUnavailable)
        ));
    }

    #[tokio::test]
    async fn restart_recovers_work_aborted_before_durable_completion() {
        let now = Timestamp::from_unix_millis(10_000).unwrap();
        let pending = event(7, 1_000);
        let backlog = Arc::new(FakeBacklog::new([(now, pending)]));
        let gate = Arc::new(Notify::new());
        let first_handler = Arc::new(FakeHandler {
            backlog: Arc::clone(&backlog),
            handled: Mutex::new(Vec::new()),
            gate: Some(gate),
            started: AtomicUsize::new(0),
        });
        let first_root = ShutdownRoot::new();
        let mut crash_config = config();
        crash_config.shutdown_drain = Duration::from_millis(10);
        let (_, first_task) = Dispatcher::start(
            backlog.clone(),
            first_handler.clone(),
            Arc::new(TestClock(now)),
            crash_config,
            first_root.signal(),
        )
        .await
        .unwrap();
        wait_until(|| first_handler.started.load(Ordering::Relaxed) == 1).await;
        first_root.begin();
        first_task.wait().await;
        assert_eq!(lock(&backlog.events).len(), 1);

        let second_handler = Arc::new(FakeHandler {
            backlog: Arc::clone(&backlog),
            handled: Mutex::new(Vec::new()),
            gate: None,
            started: AtomicUsize::new(0),
        });
        let second_root = ShutdownRoot::new();
        let (_, second_task) = Dispatcher::start(
            backlog,
            second_handler.clone(),
            Arc::new(TestClock(now)),
            config(),
            second_root.signal(),
        )
        .await
        .unwrap();
        wait_until(|| lock(&second_handler.handled).len() == 1).await;
        second_root.begin();
        second_task.wait().await;
    }

    #[tokio::test]
    async fn refill_outage_does_not_block_fresh_durable_handoff() {
        let now = Timestamp::from_unix_millis(10_000).unwrap();
        let backlog = Arc::new(FakeBacklog::new([]));
        let handler = Arc::new(FakeHandler {
            backlog: Arc::clone(&backlog),
            handled: Mutex::new(Vec::new()),
            gate: None,
            started: AtomicUsize::new(0),
        });
        let root = ShutdownRoot::new();
        let (dispatcher, task) = Dispatcher::start(
            backlog.clone(),
            handler.clone(),
            Arc::new(TestClock(now)),
            config(),
            root.signal(),
        )
        .await
        .unwrap();
        backlog.fail.store(true, Ordering::Relaxed);
        assert!(dispatcher.offer(event(8, 8_000)).is_ok());
        wait_until(|| lock(&handler.handled).len() == 1).await;
        root.begin();
        task.wait().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn large_backlog_drains_with_bounded_scheduled_set() {
        let now = Timestamp::from_unix_millis(100_000).unwrap();
        let backlog = Arc::new(FakeBacklog::new((0..5_000_u32).map(|index| {
            (
                Timestamp::from_unix_millis(i64::from(index)).unwrap(),
                AcceptedEvent {
                    event_id: EventId::from_bytes(u128::from(index).to_be_bytes()),
                    ..event((index % 250) as u8, i64::from(index))
                },
            )
        })));
        let handler = Arc::new(FakeHandler {
            backlog: Arc::clone(&backlog),
            handled: Mutex::new(Vec::new()),
            gate: None,
            started: AtomicUsize::new(0),
        });
        let root = ShutdownRoot::new();
        let bounded = DispatcherConfig {
            queue_capacity: 128,
            worker_concurrency: 16,
            low_watermark: 32,
            refill_target: 96,
            refill_batch_size: 96,
            poll_interval: Duration::from_millis(1),
            metrics_interval: Duration::from_secs(1),
            source_timeout: Duration::from_secs(1),
            shutdown_drain: Duration::from_secs(1),
        };
        let (dispatcher, task) = Dispatcher::start(
            backlog,
            handler.clone(),
            Arc::new(TestClock(now)),
            bounded,
            root.signal(),
        )
        .await
        .unwrap();
        let mut maximum_scheduled = 0;
        timeout(Duration::from_secs(10), async {
            while lock(&handler.handled).len() < 5_000 {
                maximum_scheduled = maximum_scheduled.max(dispatcher.scheduled_keys());
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(maximum_scheduled <= bounded.queue_capacity + bounded.worker_concurrency);
        root.begin();
        task.wait().await;
    }
}
