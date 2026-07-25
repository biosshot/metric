//! Bounded durable Structured Log micro-batching.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use faultkeep_domain::signals::LogRecord;
use faultkeep_ports::{DurableOutcome, LogSink, PortFuture, SignalStore, SignalStoreError};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant as TokioInstant, sleep_until, timeout, timeout_at},
};

use crate::shutdown::ShutdownSignal;

const LOG_RECORD_OVERHEAD_BYTES: usize = 192;

#[derive(Debug, Clone, Copy)]
pub struct LogWriterConfig {
    pub channel_capacity: usize,
    pub max_wait: Duration,
    pub max_documents: usize,
    pub max_bytes: usize,
    pub operation_timeout: Duration,
    pub shutdown_drain: Duration,
}

impl Default for LogWriterConfig {
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
pub enum LogWriterStartError {
    #[error("LogWriter configuration is invalid")]
    InvalidConfiguration,
}

struct Command {
    record: LogRecord,
    estimated_bytes: usize,
    response: oneshot::Sender<Result<DurableOutcome, SignalStoreError>>,
}

struct Batch {
    records: Vec<LogRecord>,
    responses: Vec<oneshot::Sender<Result<DurableOutcome, SignalStoreError>>>,
    estimated_bytes: usize,
    opened_at: Instant,
}

impl Batch {
    fn new(capacity: usize, command: Command) -> Self {
        let mut records = Vec::with_capacity(capacity);
        let mut responses = Vec::with_capacity(capacity);
        records.push(command.record);
        responses.push(command.response);
        Self {
            records,
            responses,
            estimated_bytes: command.estimated_bytes,
            opened_at: Instant::now(),
        }
    }

    fn can_push(&self, command: &Command, config: LogWriterConfig) -> bool {
        self.records.len() < config.max_documents
            && self.estimated_bytes.saturating_add(command.estimated_bytes) <= config.max_bytes
    }

    fn push(&mut self, command: Command) {
        self.estimated_bytes = self.estimated_bytes.saturating_add(command.estimated_bytes);
        self.records.push(command.record);
        self.responses.push(command.response);
    }
}

pub struct LogWriter {
    sender: mpsc::Sender<Command>,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
    max_batch_bytes: usize,
}

impl LogWriter {
    pub fn start(
        store: Arc<dyn SignalStore>,
        config: LogWriterConfig,
        shutdown: ShutdownSignal,
    ) -> Result<(Arc<Self>, LogWriterTask), LogWriterStartError> {
        validate_config(config)?;
        let (sender, receiver) = mpsc::channel(config.channel_capacity);
        let accepting = Arc::new(AtomicBool::new(true));
        let writer = Arc::new(Self {
            sender,
            accepting: Arc::clone(&accepting),
            shutdown: shutdown.clone(),
            max_batch_bytes: config.max_bytes,
        });
        let join = tokio::spawn(run_writer(store, receiver, config, accepting, shutdown));
        Ok((writer, LogWriterTask { join }))
    }

    #[must_use]
    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire) && !self.shutdown.is_cancelled()
    }
}

impl LogSink for LogWriter {
    fn persist_logs(
        &self,
        records: Vec<LogRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(async move {
            if records.is_empty() {
                return Ok(Vec::new());
            }
            if !self.is_accepting() {
                record_outcome("admission_rejected");
                return Err(SignalStoreError::Unavailable);
            }
            let records = records
                .into_iter()
                .map(|record| {
                    let estimated_bytes = estimated_log_bytes(&record);
                    if estimated_bytes > self.max_batch_bytes {
                        Err(SignalStoreError::InvalidData)
                    } else {
                        Ok((record, estimated_bytes))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let permits = self.sender.try_reserve_many(records.len()).map_err(|_| {
                record_outcome("capacity_rejected");
                SignalStoreError::Capacity
            })?;
            let mut receivers = Vec::with_capacity(records.len());
            for ((record, estimated_bytes), permit) in records.into_iter().zip(permits) {
                let (response, receiver) = oneshot::channel();
                let command = Command {
                    estimated_bytes,
                    record,
                    response,
                };
                permit.send(command);
                receivers.push(receiver);
            }
            let mut outcomes = Vec::with_capacity(receivers.len());
            for receiver in receivers {
                match receiver.await {
                    Ok(Ok(outcome)) => outcomes.push(outcome),
                    Ok(Err(error)) => return Err(error),
                    Err(_) => {
                        record_outcome("actor_closed");
                        return Err(SignalStoreError::Unavailable);
                    }
                }
            }
            Ok(outcomes)
        })
    }
}

pub struct LogWriterTask {
    join: JoinHandle<()>,
}

impl LogWriterTask {
    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

fn validate_config(config: LogWriterConfig) -> Result<(), LogWriterStartError> {
    let valid = config.channel_capacity > 0
        && (1..=500).contains(&config.max_documents)
        && config.max_bytes >= LOG_RECORD_OVERHEAD_BYTES
        && !config.max_wait.is_zero()
        && !config.operation_timeout.is_zero()
        && !config.shutdown_drain.is_zero();
    if valid {
        Ok(())
    } else {
        Err(LogWriterStartError::InvalidConfiguration)
    }
}

fn estimated_log_bytes(record: &LogRecord) -> usize {
    [
        record.message.len(),
        record.environment.as_deref().map_or(0, str::len),
        record.release.as_deref().map_or(0, str::len),
        record.service.as_deref().map_or(0, str::len),
        record.body.as_bytes().len(),
        LOG_RECORD_OVERHEAD_BYTES,
    ]
    .into_iter()
    .fold(0, usize::saturating_add)
}

async fn run_writer(
    store: Arc<dyn SignalStore>,
    mut receiver: mpsc::Receiver<Command>,
    config: LogWriterConfig,
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
                    drain_writer(&store, &mut receiver, config).await;
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
        while batch.records.len() < config.max_documents {
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
        flush_batch(&store, config.operation_timeout, batch).await;
        if shutdown_requested {
            if let Some(command) = carry.take() {
                reject_command(command, SignalStoreError::Unavailable);
            }
            drain_writer(&store, &mut receiver, config).await;
            return;
        }
    }
}

async fn drain_writer(
    store: &Arc<dyn SignalStore>,
    receiver: &mut mpsc::Receiver<Command>,
    config: LogWriterConfig,
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
        while batch.records.len() < config.max_documents {
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
            flush_batch(store, config.operation_timeout, batch),
        )
        .await
        .is_err()
        {
            while let Ok(command) = receiver.try_recv() {
                reject_command(command, SignalStoreError::Unavailable);
            }
            return;
        }
    }
}

async fn flush_batch(store: &Arc<dyn SignalStore>, operation_timeout: Duration, mut batch: Batch) {
    let documents = batch.records.len();
    metrics::histogram!("faultkeep_log_writer_batch_documents").record(documents as f64);
    metrics::histogram!("faultkeep_log_writer_batch_bytes").record(batch.estimated_bytes as f64);
    metrics::histogram!("faultkeep_log_writer_batch_wait_seconds")
        .record(batch.opened_at.elapsed().as_secs_f64());
    let started = Instant::now();
    let records = std::mem::take(&mut batch.records);
    let result = timeout(operation_timeout, store.persist_logs(records)).await;
    metrics::histogram!("faultkeep_log_writer_batch_latency_seconds")
        .record(started.elapsed().as_secs_f64());
    match result {
        Ok(Ok(outcomes)) if outcomes.len() == documents => {
            for (response, outcome) in batch.responses.drain(..).zip(outcomes) {
                let metric = match outcome {
                    DurableOutcome::Accepted => "inserted",
                    DurableOutcome::Duplicate => "duplicate",
                };
                let _ = response.send(Ok(outcome));
                record_outcome(metric);
            }
        }
        Ok(Err(error)) => reject_batch(batch, error, "unavailable"),
        Err(_) | Ok(Ok(_)) => {
            reject_batch(batch, SignalStoreError::Unavailable, "ambiguous");
        }
    }
}

fn reject_batch(batch: Batch, error: SignalStoreError, outcome: &'static str) {
    for response in batch.responses {
        let _ = response.send(Err(error));
        record_outcome(outcome);
    }
}

fn reject_command(command: Command, error: SignalStoreError) {
    let _ = command.response.send(Err(error));
}

fn record_outcome(outcome: &'static str) {
    metrics::counter!("faultkeep_log_writer_records_total", "outcome" => outcome).increment(1);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use faultkeep_domain::{
        ProjectId, Timestamp,
        signals::{LogId, LogSeverity, SignalBody, SignalPage, TraceView},
    };
    use faultkeep_ports::{LogQuery, PerformanceQuery, SegmentQuery};
    use tokio::sync::Notify;

    use super::*;

    struct FakeStore {
        batches: Mutex<Vec<usize>>,
        delay: Duration,
        started: Notify,
    }

    impl FakeStore {
        fn new(delay: Duration) -> Self {
            Self {
                batches: Mutex::new(Vec::new()),
                delay,
                started: Notify::new(),
            }
        }
    }

    impl SignalStore for FakeStore {
        fn persist_logs(
            &self,
            records: Vec<LogRecord>,
        ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
            Box::pin(async move {
                self.started.notify_one();
                if !self.delay.is_zero() {
                    tokio::time::sleep(self.delay).await;
                }
                self.batches.lock().unwrap().push(records.len());
                Ok(vec![DurableOutcome::Accepted; records.len()])
            })
        }

        fn persist_spans(
            &self,
            _records: Vec<faultkeep_domain::signals::SpanRecord>,
        ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn list_logs(
            &self,
            _project_id: ProjectId,
            _query: LogQuery,
        ) -> PortFuture<'_, Result<SignalPage<LogRecord>, SignalStoreError>> {
            Box::pin(async { Err(SignalStoreError::NotFound) })
        }

        fn load_log(
            &self,
            _project_id: ProjectId,
            _log_id: LogId,
        ) -> PortFuture<'_, Result<LogRecord, SignalStoreError>> {
            Box::pin(async { Err(SignalStoreError::NotFound) })
        }

        fn list_segments(
            &self,
            _project_id: ProjectId,
            _query: SegmentQuery,
        ) -> PortFuture<
            '_,
            Result<SignalPage<faultkeep_domain::signals::SpanRecord>, SignalStoreError>,
        > {
            Box::pin(async { Err(SignalStoreError::NotFound) })
        }

        fn trace(
            &self,
            _project_ids: Vec<ProjectId>,
            _trace_id: faultkeep_domain::signals::TraceId,
            _maximum_spans: usize,
            _maximum_logs: usize,
        ) -> PortFuture<'_, Result<TraceView, SignalStoreError>> {
            Box::pin(async { Err(SignalStoreError::NotFound) })
        }

        fn performance(
            &self,
            _project_id: ProjectId,
            _query: PerformanceQuery,
        ) -> PortFuture<
            '_,
            Result<Vec<faultkeep_domain::signals::PerformanceBucket>, SignalStoreError>,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn rebuild_span_stats(
            &self,
            _project_id: ProjectId,
            _from: Timestamp,
            _until: Timestamp,
        ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
            Box::pin(async { Ok(0) })
        }
    }

    fn record(byte: u8, body_bytes: usize) -> LogRecord {
        let project_id = ProjectId::new(42).unwrap();
        let received_at = Timestamp::from_unix_millis(1_000).unwrap();
        LogRecord {
            id: LogId::deterministic(project_id, received_at, 1_000_000_000, &[byte]),
            project_id,
            received_at,
            occurred_at_ns: 1_000_000_000,
            severity: LogSeverity::Info,
            message: format!("log-{byte}").into(),
            trace_id: None,
            span_id: None,
            environment: None,
            release: None,
            service: None,
            body: SignalBody::new(vec![byte; body_bytes]),
        }
    }

    fn config() -> LogWriterConfig {
        LogWriterConfig {
            channel_capacity: 256,
            max_wait: Duration::from_millis(20),
            max_documents: 100,
            max_bytes: 64 * 1024,
            operation_timeout: Duration::from_secs(1),
            shutdown_drain: Duration::from_secs(1),
        }
    }

    #[tokio::test]
    async fn timer_flushes_a_partial_batch() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::ZERO));
        let store_port: Arc<dyn SignalStore> = store.clone();
        let (writer, task) = LogWriter::start(store_port, config(), root.signal()).unwrap();
        assert_eq!(
            writer.persist_logs(vec![record(1, 8)]).await,
            Ok(vec![DurableOutcome::Accepted])
        );
        assert_eq!(store.batches.lock().unwrap().as_slice(), &[1]);
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn concurrent_records_are_micro_batched() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::ZERO));
        let store_port: Arc<dyn SignalStore> = store.clone();
        let (writer, task) = LogWriter::start(store_port, config(), root.signal()).unwrap();
        let mut requests = Vec::new();
        for byte in 0..100 {
            let writer = Arc::clone(&writer);
            requests.push(tokio::spawn(async move {
                writer.persist_logs(vec![record(byte, 8)]).await
            }));
        }
        for request in requests {
            assert_eq!(request.await.unwrap(), Ok(vec![DurableOutcome::Accepted]));
        }
        assert_eq!(store.batches.lock().unwrap().as_slice(), &[100]);
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn byte_limit_splits_batches() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::ZERO));
        let store_port: Arc<dyn SignalStore> = store.clone();
        let (writer, task) = LogWriter::start(
            store_port,
            LogWriterConfig {
                max_bytes: LOG_RECORD_OVERHEAD_BYTES + 64,
                ..config()
            },
            root.signal(),
        )
        .unwrap();
        let first = {
            let writer = Arc::clone(&writer);
            tokio::spawn(async move { writer.persist_logs(vec![record(1, 32)]).await })
        };
        assert_eq!(
            writer.persist_logs(vec![record(2, 32)]).await,
            Ok(vec![DurableOutcome::Accepted])
        );
        assert_eq!(first.await.unwrap(), Ok(vec![DurableOutcome::Accepted]));
        assert_eq!(store.batches.lock().unwrap().as_slice(), &[1, 1]);
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn shutdown_drains_and_rejects_new_records() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store: Arc<dyn SignalStore> = Arc::new(FakeStore::new(Duration::from_millis(10)));
        let (writer, task) = LogWriter::start(store, config(), root.signal()).unwrap();
        let queued = {
            let writer = Arc::clone(&writer);
            tokio::spawn(async move { writer.persist_logs(vec![record(1, 8)]).await })
        };
        tokio::task::yield_now().await;
        root.begin();
        let queued = queued.await.unwrap();
        assert!(
            matches!(
                queued,
                Ok(ref outcomes) if outcomes == &[DurableOutcome::Accepted]
            ) || matches!(queued, Err(SignalStoreError::Unavailable))
        );
        assert_eq!(
            writer.persist_logs(vec![record(2, 8)]).await,
            Err(SignalStoreError::Unavailable)
        );
        task.wait().await;
    }

    #[tokio::test]
    async fn full_lane_rejects_without_growing_the_queue() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::from_millis(50)));
        let store_port: Arc<dyn SignalStore> = store.clone();
        let (writer, task) = LogWriter::start(
            store_port,
            LogWriterConfig {
                channel_capacity: 1,
                max_documents: 1,
                ..config()
            },
            root.signal(),
        )
        .unwrap();
        let active = {
            let writer = Arc::clone(&writer);
            tokio::spawn(async move { writer.persist_logs(vec![record(1, 8)]).await })
        };
        store.started.notified().await;
        let queued = {
            let writer = Arc::clone(&writer);
            tokio::spawn(async move { writer.persist_logs(vec![record(2, 8)]).await })
        };
        while writer.sender.capacity() != 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            writer.persist_logs(vec![record(3, 8)]).await,
            Err(SignalStoreError::Capacity)
        );
        assert_eq!(active.await.unwrap(), Ok(vec![DurableOutcome::Accepted]));
        assert_eq!(queued.await.unwrap(), Ok(vec![DurableOutcome::Accepted]));
        root.begin();
        task.wait().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "Phase 24 Log writer RPS baseline runs explicitly in release mode"]
    async fn performance_log_writer_rps_and_batch_occupancy() {
        const RECORDS: usize = 20_000;
        const CONCURRENCY: usize = 512;
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore::new(Duration::ZERO));
        let store_port: Arc<dyn SignalStore> = store.clone();
        let (writer, task) = LogWriter::start(
            store_port,
            LogWriterConfig {
                channel_capacity: 1_024,
                max_wait: Duration::from_millis(2),
                max_documents: 250,
                max_bytes: 8 * 1024 * 1024,
                operation_timeout: Duration::from_secs(1),
                shutdown_drain: Duration::from_secs(2),
            },
            root.signal(),
        )
        .unwrap();
        let started = Instant::now();
        for start in (0..RECORDS).step_by(CONCURRENCY) {
            let mut requests = Vec::with_capacity(CONCURRENCY);
            for index in start..(start + CONCURRENCY).min(RECORDS) {
                let writer = Arc::clone(&writer);
                requests.push(tokio::spawn(async move {
                    writer
                        .persist_logs(vec![record((index % 251) as u8, 128)])
                        .await
                }));
            }
            for request in requests {
                assert_eq!(request.await.unwrap(), Ok(vec![DurableOutcome::Accepted]));
            }
        }
        let elapsed = started.elapsed();
        root.begin();
        task.wait().await;
        let batches = store.batches.lock().unwrap();
        let durable = batches.iter().sum::<usize>();
        let average_occupancy = durable as f64 / batches.len() as f64;
        let rps = durable as f64 / elapsed.as_secs_f64();
        eprintln!(
            "LogWriter Phase 24: rps={rps:.0},records={durable},batches={},average_occupancy={average_occupancy:.2},concurrency={CONCURRENCY},elapsed_ms={}",
            batches.len(),
            elapsed.as_millis()
        );
        assert_eq!(durable, RECORDS);
        assert!(rps >= 20_000.0, "Log writer {rps:.0} RPS is below gate");
        assert!(
            average_occupancy >= 100.0,
            "average Log batch occupancy {average_occupancy:.2} is below gate"
        );
    }
}
