//! Bounded durable Event micro-batching.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use metric_domain::AcceptedEvent;
use metric_ports::{
    AcceptedEventHandoff, DurableOutcome, EventSink, EventSinkError, EventStore, EventStoreError,
    EventWriteStatus, PortFuture, PreparedEvent,
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant as TokioInstant, sleep_until, timeout, timeout_at},
};

use crate::shutdown::ShutdownSignal;

#[derive(Debug, Clone, Copy)]
pub struct MongoWriterConfig {
    pub channel_capacity: usize,
    pub max_wait: Duration,
    pub max_documents: usize,
    pub max_bytes: usize,
    pub operation_timeout: Duration,
    pub shutdown_drain: Duration,
}

impl Default for MongoWriterConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 512,
            max_wait: Duration::from_millis(20),
            max_documents: 250,
            max_bytes: 8 * 1024 * 1024,
            operation_timeout: Duration::from_secs(10),
            shutdown_drain: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MongoWriterStartError {
    #[error("MongoWriter configuration is invalid")]
    InvalidConfiguration,
}

struct Command<P> {
    prepared: P,
    response: oneshot::Sender<Result<DurableOutcome, EventSinkError>>,
}

struct Batch<P> {
    events: Vec<P>,
    responses: Vec<oneshot::Sender<Result<DurableOutcome, EventSinkError>>>,
    encoded_bytes: usize,
    opened_at: Instant,
}

impl<P: PreparedEvent> Batch<P> {
    fn new(capacity: usize, command: Command<P>) -> Self {
        let encoded_bytes = command.prepared.encoded_len();
        Self {
            events: vec![command.prepared],
            responses: vec![command.response],
            encoded_bytes,
            opened_at: Instant::now(),
        }
        .with_capacity_hint(capacity)
    }

    fn with_capacity_hint(mut self, capacity: usize) -> Self {
        self.events.reserve(capacity.saturating_sub(1));
        self.responses.reserve(capacity.saturating_sub(1));
        self
    }

    fn can_push(&self, command: &Command<P>, config: MongoWriterConfig) -> bool {
        self.events.len() < config.max_documents
            && (self.events.is_empty()
                || self
                    .encoded_bytes
                    .saturating_add(command.prepared.encoded_len())
                    <= config.max_bytes)
    }

    fn push(&mut self, command: Command<P>) {
        self.encoded_bytes = self
            .encoded_bytes
            .saturating_add(command.prepared.encoded_len());
        self.events.push(command.prepared);
        self.responses.push(command.response);
    }
}

pub struct MongoWriter<S: EventStore> {
    store: Arc<S>,
    sender: mpsc::Sender<Command<S::Prepared>>,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
}

impl<S: EventStore> MongoWriter<S> {
    pub fn start(
        store: Arc<S>,
        handoff: Arc<dyn AcceptedEventHandoff>,
        config: MongoWriterConfig,
        shutdown: ShutdownSignal,
    ) -> Result<(Arc<Self>, MongoWriterTask), MongoWriterStartError> {
        validate_config(config)?;
        let (sender, receiver) = mpsc::channel(config.channel_capacity);
        let accepting = Arc::new(AtomicBool::new(true));
        let writer = Arc::new(Self {
            store: Arc::clone(&store),
            sender,
            accepting: Arc::clone(&accepting),
            shutdown: shutdown.clone(),
        });
        let join = tokio::spawn(run_writer(
            store, receiver, handoff, config, accepting, shutdown,
        ));
        Ok((writer, MongoWriterTask { join }))
    }

    #[must_use]
    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire) && !self.shutdown.is_cancelled()
    }
}

impl<S: EventStore> EventSink for MongoWriter<S> {
    fn persist(
        &self,
        event: AcceptedEvent,
    ) -> PortFuture<'_, Result<DurableOutcome, EventSinkError>> {
        Box::pin(async move {
            if !self.is_accepting() {
                record_outcome("admission_rejected");
                return Err(EventSinkError::Unavailable);
            }
            let prepared = match self.store.prepare(event) {
                Ok(prepared) => prepared,
                Err(_) => {
                    record_outcome("prepare_rejected");
                    return Err(EventSinkError::Unavailable);
                }
            };
            let (response, received) = oneshot::channel();
            if self
                .sender
                .send(Command { prepared, response })
                .await
                .is_err()
            {
                record_outcome("admission_rejected");
                return Err(EventSinkError::Unavailable);
            }
            match received.await {
                Ok(result) => result,
                Err(_) => {
                    record_outcome("actor_closed");
                    Err(EventSinkError::Unavailable)
                }
            }
        })
    }
}

pub struct MongoWriterTask {
    join: JoinHandle<()>,
}

impl MongoWriterTask {
    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

fn validate_config(config: MongoWriterConfig) -> Result<(), MongoWriterStartError> {
    let valid = config.channel_capacity > 0
        && (100..=500).contains(&config.max_documents)
        && config.max_bytes > 0
        && !config.max_wait.is_zero()
        && !config.operation_timeout.is_zero()
        && !config.shutdown_drain.is_zero();
    if valid {
        Ok(())
    } else {
        Err(MongoWriterStartError::InvalidConfiguration)
    }
}

async fn run_writer<S: EventStore>(
    store: Arc<S>,
    mut receiver: mpsc::Receiver<Command<S::Prepared>>,
    handoff: Arc<dyn AcceptedEventHandoff>,
    config: MongoWriterConfig,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
) {
    let mut carry = None;
    loop {
        let first = if let Some(command) = carry.take() {
            command
        } else {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => {
                    accepting.store(false, Ordering::Release);
                    drain_writer(&store, &mut receiver, &handoff, config).await;
                    return;
                }
                command = receiver.recv() => match command {
                    Some(command) => command,
                    None => {
                        accepting.store(false, Ordering::Release);
                        return;
                    }
                }
            }
        };
        let mut batch = Batch::new(config.max_documents, first);
        let deadline = TokioInstant::now() + config.max_wait;
        let mut shutdown_requested = false;
        while batch.events.len() < config.max_documents {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => {
                    accepting.store(false, Ordering::Release);
                    shutdown_requested = true;
                    break;
                }
                () = sleep_until(deadline) => break,
                command = receiver.recv() => match command {
                    Some(command) if batch.can_push(&command, config) => batch.push(command),
                    Some(command) => {
                        carry = Some(command);
                        break;
                    }
                    None => break,
                }
            }
        }
        flush_batch(&store, &handoff, config.operation_timeout, batch).await;
        if shutdown_requested {
            if let Some(command) = carry.take() {
                reject_command(command, EventSinkError::Unavailable);
            }
            drain_writer(&store, &mut receiver, &handoff, config).await;
            return;
        }
    }
}

async fn drain_writer<S: EventStore>(
    store: &Arc<S>,
    receiver: &mut mpsc::Receiver<Command<S::Prepared>>,
    handoff: &Arc<dyn AcceptedEventHandoff>,
    config: MongoWriterConfig,
) {
    let deadline = TokioInstant::now() + config.shutdown_drain;
    let mut carry = None;
    loop {
        let first = match carry.take() {
            Some(command) => command,
            None => match receiver.try_recv() {
                Ok(command) => command,
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    return;
                }
            },
        };
        let mut batch = Batch::new(config.max_documents, first);
        while batch.events.len() < config.max_documents {
            match receiver.try_recv() {
                Ok(command) if batch.can_push(&command, config) => batch.push(command),
                Ok(command) => {
                    carry = Some(command);
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    break;
                }
            }
        }
        if timeout_at(
            deadline,
            flush_batch(store, handoff, config.operation_timeout, batch),
        )
        .await
        .is_err()
        {
            while let Ok(command) = receiver.try_recv() {
                reject_command(command, EventSinkError::Unavailable);
            }
            return;
        }
    }
}

async fn flush_batch<S: EventStore>(
    store: &Arc<S>,
    handoff: &Arc<dyn AcceptedEventHandoff>,
    operation_timeout: Duration,
    mut batch: Batch<S::Prepared>,
) {
    let documents = batch.events.len();
    metrics::histogram!("metric_mongo_writer_batch_documents").record(documents as f64);
    metrics::histogram!("metric_mongo_writer_batch_bytes").record(batch.encoded_bytes as f64);
    metrics::histogram!("metric_mongo_writer_batch_wait_seconds")
        .record(batch.opened_at.elapsed().as_secs_f64());
    let started = Instant::now();
    let result = timeout(operation_timeout, store.insert_batch(&batch.events)).await;
    metrics::histogram!("metric_mongo_writer_batch_latency_seconds")
        .record(started.elapsed().as_secs_f64());
    match result {
        Ok(Ok(statuses)) if statuses.len() == documents => {
            for ((prepared, response), status) in batch
                .events
                .drain(..)
                .zip(batch.responses.drain(..))
                .zip(statuses)
            {
                match status {
                    EventWriteStatus::Inserted => {
                        let handoff_outcome = if handoff.offer(prepared.into_event()).is_ok() {
                            "accepted"
                        } else {
                            "rejected"
                        };
                        metrics::counter!(
                            "metric_mongo_writer_handoff_total",
                            "outcome" => handoff_outcome
                        )
                        .increment(1);
                        let _ = response.send(Ok(DurableOutcome::Accepted));
                        record_outcome("inserted");
                    }
                    EventWriteStatus::Duplicate => {
                        let _ = response.send(Ok(DurableOutcome::Duplicate));
                        record_outcome("duplicate");
                    }
                    EventWriteStatus::Rejected => {
                        let _ = response.send(Err(EventSinkError::Unavailable));
                        record_outcome("rejected");
                    }
                }
            }
        }
        Ok(Err(EventStoreError::Unavailable)) => {
            reject_batch(batch, EventSinkError::Unavailable, "unavailable");
        }
        Ok(Err(EventStoreError::Ambiguous)) | Err(_) | Ok(Ok(_)) => {
            reject_batch(batch, EventSinkError::Ambiguous, "ambiguous");
        }
    }
}

fn reject_batch<P: PreparedEvent>(batch: Batch<P>, error: EventSinkError, outcome: &'static str) {
    for response in batch.responses {
        let _ = response.send(Err(error));
        record_outcome(outcome);
    }
}

fn reject_command<P>(command: Command<P>, error: EventSinkError) {
    let _ = command.response.send(Err(error));
}

fn record_outcome(outcome: &'static str) {
    metrics::counter!("metric_mongo_writer_events_total", "outcome" => outcome).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use metric_domain::{EventId, EventKey, ProjectId, ScrubbedEventPayload, Timestamp};
    use metric_ports::{EventPrepareError, EventStoreError};
    use tokio::sync::Notify;

    struct FakePrepared {
        event: AcceptedEvent,
        len: usize,
    }

    impl PreparedEvent for FakePrepared {
        fn key(&self) -> EventKey {
            EventKey::new(self.event.project_id, self.event.event_id)
        }

        fn encoded_len(&self) -> usize {
            self.len
        }

        fn into_event(self) -> AcceptedEvent {
            self.event
        }
    }

    struct FakeStore {
        batches: Mutex<Vec<Vec<EventKey>>>,
        delay: Duration,
        prepared_len: usize,
        started: Notify,
    }

    impl FakeStore {
        fn new(delay: Duration, prepared_len: usize) -> Self {
            Self {
                batches: Mutex::new(Vec::new()),
                delay,
                prepared_len,
                started: Notify::new(),
            }
        }
    }

    impl EventStore for FakeStore {
        type Prepared = FakePrepared;

        fn prepare(&self, event: AcceptedEvent) -> Result<Self::Prepared, EventPrepareError> {
            Ok(FakePrepared {
                event,
                len: self.prepared_len,
            })
        }

        fn insert_batch<'a>(
            &'a self,
            events: &'a [Self::Prepared],
        ) -> PortFuture<'a, Result<Vec<EventWriteStatus>, EventStoreError>> {
            Box::pin(async move {
                self.started.notify_one();
                if !self.delay.is_zero() {
                    tokio::time::sleep(self.delay).await;
                }
                self.batches
                    .lock()
                    .unwrap()
                    .push(events.iter().map(PreparedEvent::key).collect());
                Ok(vec![EventWriteStatus::Inserted; events.len()])
            })
        }
    }

    #[derive(Default)]
    struct CapturingHandoff(Mutex<Vec<EventKey>>);

    impl AcceptedEventHandoff for CapturingHandoff {
        fn offer(&self, event: AcceptedEvent) -> Result<(), AcceptedEvent> {
            self.0
                .lock()
                .unwrap()
                .push(EventKey::new(event.project_id, event.event_id));
            Ok(())
        }
    }

    struct RejectingHandoff;

    impl AcceptedEventHandoff for RejectingHandoff {
        fn offer(&self, event: AcceptedEvent) -> Result<(), AcceptedEvent> {
            Err(event)
        }
    }

    fn event(byte: u8) -> AcceptedEvent {
        AcceptedEvent {
            project_id: ProjectId::new(42).unwrap(),
            event_id: EventId::from_bytes([byte; 16]),
            received_at: Timestamp::from_unix_millis(1_000).unwrap(),
            policy_revision: 1,
            payload: ScrubbedEventPayload::new(br#"{"event_id":"fixture"}"#.as_slice()),
        }
    }

    fn config() -> MongoWriterConfig {
        MongoWriterConfig {
            channel_capacity: 256,
            max_wait: Duration::from_millis(20),
            max_documents: 100,
            max_bytes: 1024,
            operation_timeout: Duration::from_secs(1),
            shutdown_drain: Duration::from_secs(1),
        }
    }

    #[tokio::test]
    async fn timer_flushes_a_partial_batch() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::ZERO, 20));
        let handoff = Arc::new(CapturingHandoff::default());
        let (writer, task) =
            MongoWriter::start(Arc::clone(&store), handoff.clone(), config(), root.signal())
                .unwrap();
        assert_eq!(writer.persist(event(1)).await, Ok(DurableOutcome::Accepted));
        assert_eq!(store.batches.lock().unwrap()[0].len(), 1);
        assert_eq!(handoff.0.lock().unwrap().len(), 1);
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn full_dispatcher_handoff_does_not_undo_durable_acceptance() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::ZERO, 20));
        let (writer, task) = MongoWriter::start(
            Arc::clone(&store),
            Arc::new(RejectingHandoff),
            config(),
            root.signal(),
        )
        .unwrap();
        assert_eq!(writer.persist(event(9)).await, Ok(DurableOutcome::Accepted));
        assert_eq!(store.batches.lock().unwrap().len(), 1);
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn document_threshold_flushes_one_full_batch() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::ZERO, 8));
        let (writer, task) = MongoWriter::start(
            Arc::clone(&store),
            Arc::new(CapturingHandoff::default()),
            MongoWriterConfig {
                max_wait: Duration::from_secs(1),
                ..config()
            },
            root.signal(),
        )
        .unwrap();
        let mut requests = Vec::new();
        for byte in 0..100 {
            let writer = Arc::clone(&writer);
            requests.push(tokio::spawn(
                async move { writer.persist(event(byte)).await },
            ));
        }
        for request in requests {
            assert_eq!(request.await.unwrap(), Ok(DurableOutcome::Accepted));
        }
        assert_eq!(store.batches.lock().unwrap()[0].len(), 100);
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn encoded_byte_threshold_splits_batches() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::ZERO, 60));
        let (writer, task) = MongoWriter::start(
            Arc::clone(&store),
            Arc::new(CapturingHandoff::default()),
            MongoWriterConfig {
                max_bytes: 100,
                ..config()
            },
            root.signal(),
        )
        .unwrap();
        let first = {
            let writer = Arc::clone(&writer);
            tokio::spawn(async move { writer.persist(event(1)).await })
        };
        let second = writer.persist(event(2));
        assert_eq!(second.await, Ok(DurableOutcome::Accepted));
        assert_eq!(first.await.unwrap(), Ok(DurableOutcome::Accepted));
        assert_eq!(
            store
                .batches
                .lock()
                .unwrap()
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            [1, 1]
        );
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn cancelled_waiter_does_not_cancel_an_enqueued_write() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::from_millis(25), 20));
        let handoff = Arc::new(CapturingHandoff::default());
        let (writer, task) =
            MongoWriter::start(Arc::clone(&store), handoff.clone(), config(), root.signal())
                .unwrap();
        let request = {
            let writer = Arc::clone(&writer);
            tokio::spawn(async move { writer.persist(event(1)).await })
        };
        store.started.notified().await;
        request.abort();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(store.batches.lock().unwrap().len(), 1);
        assert_eq!(handoff.0.lock().unwrap().len(), 1);
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn shutdown_drains_queued_work_and_rejects_new_submissions() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::from_millis(10), 20));
        let (writer, task) = MongoWriter::start(
            store,
            Arc::new(CapturingHandoff::default()),
            config(),
            root.signal(),
        )
        .unwrap();
        let queued = {
            let writer = Arc::clone(&writer);
            tokio::spawn(async move { writer.persist(event(1)).await })
        };
        tokio::task::yield_now().await;
        root.begin();
        let queued = queued.await.unwrap();
        assert!(matches!(
            queued,
            Ok(DurableOutcome::Accepted) | Err(EventSinkError::Unavailable)
        ));
        assert_eq!(
            writer.persist(event(2)).await,
            Err(EventSinkError::Unavailable)
        );
        task.wait().await;
    }
}
