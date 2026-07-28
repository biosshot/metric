//! Dedicated bounded micro-batching for compact Application Metric deltas.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use metric_domain::metrics::MetricDeltaBatch;
use metric_ports::{DurableOutcome, MetricSink, MetricStore, PortFuture, SignalStoreError};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};

use crate::shutdown::ShutdownSignal;

#[derive(Debug, Clone, Copy)]
pub struct MetricWriterConfig {
    pub channel_capacity: usize,
    pub max_wait: Duration,
    pub max_deltas: usize,
    pub operation_timeout: Duration,
    pub shutdown_drain: Duration,
}

impl Default for MetricWriterConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 256,
            max_wait: Duration::from_millis(20),
            max_deltas: 2_000,
            operation_timeout: Duration::from_secs(10),
            shutdown_drain: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MetricWriterStartError {
    #[error("MetricWriter configuration is invalid")]
    InvalidConfiguration,
}

struct Command {
    batch: MetricDeltaBatch,
    response: oneshot::Sender<Result<DurableOutcome, SignalStoreError>>,
}

pub struct MetricWriter {
    sender: mpsc::Sender<Command>,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
    max_deltas: usize,
}

impl MetricWriter {
    pub fn start(
        store: Arc<dyn MetricStore>,
        config: MetricWriterConfig,
        shutdown: ShutdownSignal,
    ) -> Result<(Arc<Self>, MetricWriterTask), MetricWriterStartError> {
        if config.channel_capacity == 0
            || config.max_deltas == 0
            || config.max_wait.is_zero()
            || config.operation_timeout.is_zero()
            || config.shutdown_drain.is_zero()
        {
            return Err(MetricWriterStartError::InvalidConfiguration);
        }
        let (sender, receiver) = mpsc::channel(config.channel_capacity);
        let accepting = Arc::new(AtomicBool::new(true));
        let writer = Arc::new(Self {
            sender,
            accepting: Arc::clone(&accepting),
            shutdown: shutdown.clone(),
            max_deltas: config.max_deltas,
        });
        let join = tokio::spawn(run_writer(store, receiver, config, accepting, shutdown));
        Ok((writer, MetricWriterTask { join }))
    }
}

impl MetricSink for MetricWriter {
    fn persist_metrics(
        &self,
        batch: MetricDeltaBatch,
    ) -> PortFuture<'_, Result<DurableOutcome, SignalStoreError>> {
        Box::pin(async move {
            if batch.is_empty() {
                return Ok(DurableOutcome::Accepted);
            }
            if batch.len() > self.max_deltas {
                return Err(SignalStoreError::InvalidData);
            }
            if !self.accepting.load(Ordering::Acquire) || self.shutdown.is_cancelled() {
                return Err(SignalStoreError::Unavailable);
            }
            let permit = self
                .sender
                .try_reserve()
                .map_err(|_| SignalStoreError::Capacity)?;
            let (response, receiver) = oneshot::channel();
            permit.send(Command { batch, response });
            receiver.await.unwrap_or(Err(SignalStoreError::Unavailable))
        })
    }
}

pub struct MetricWriterTask {
    join: JoinHandle<()>,
}

impl MetricWriterTask {
    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

async fn run_writer(
    store: Arc<dyn MetricStore>,
    mut receiver: mpsc::Receiver<Command>,
    config: MetricWriterConfig,
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
                    drain(&store, &mut receiver, config).await;
                    return;
                }
                command = receiver.recv() => match command {
                    Some(command) => command,
                    None => return,
                }
            }
        };
        let mut batch = first.batch;
        let mut responses = vec![first.response];
        let deadline = Instant::now() + config.max_wait;
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => {
                    accepting.store(false, Ordering::Release);
                    break;
                }
                () = sleep_until(deadline) => break,
                command = receiver.recv() => match command {
                    Some(command) if batch.len().saturating_add(command.batch.len()) <= config.max_deltas => {
                        batch.merge(command.batch);
                        responses.push(command.response);
                    }
                    Some(command) => {
                        carry = Some(command);
                        break;
                    }
                    None => break,
                }
            }
        }
        flush(&store, config.operation_timeout, batch, responses).await;
        if shutdown.is_cancelled() {
            if let Some(command) = carry.take() {
                let _ = command.response.send(Err(SignalStoreError::Unavailable));
            }
            drain(&store, &mut receiver, config).await;
            return;
        }
    }
}

async fn drain(
    store: &Arc<dyn MetricStore>,
    receiver: &mut mpsc::Receiver<Command>,
    config: MetricWriterConfig,
) {
    let deadline = Instant::now() + config.shutdown_drain;
    while let Ok(first) = receiver.try_recv() {
        let batch = first.batch;
        let responses = vec![first.response];
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            reject_remaining(receiver);
            return;
        }
        flush(
            store,
            config.operation_timeout.min(remaining),
            batch,
            responses,
        )
        .await;
    }
}

async fn flush(
    store: &Arc<dyn MetricStore>,
    operation_timeout: Duration,
    batch: MetricDeltaBatch,
    responses: Vec<oneshot::Sender<Result<DurableOutcome, SignalStoreError>>>,
) {
    let result = timeout(operation_timeout, store.persist_metrics(batch))
        .await
        .unwrap_or(Err(SignalStoreError::Unavailable));
    for response in responses {
        let _ = response.send(result);
    }
}

fn reject_remaining(receiver: &mut mpsc::Receiver<Command>) {
    while let Ok(command) = receiver.try_recv() {
        let _ = command.response.send(Err(SignalStoreError::Unavailable));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use metric_domain::{
        ProjectId, Timestamp,
        metrics::{MetricAggregate, MetricDelta, MetricKind, MetricSeries},
    };

    use super::*;
    use crate::shutdown::ShutdownRoot;

    struct RecordingStore {
        batches: Mutex<Vec<MetricDeltaBatch>>,
        fail_once: AtomicBool,
    }

    impl MetricStore for RecordingStore {
        fn persist_metrics(
            &self,
            batch: MetricDeltaBatch,
        ) -> PortFuture<'_, Result<DurableOutcome, SignalStoreError>> {
            Box::pin(async move {
                if self.fail_once.swap(false, Ordering::AcqRel) {
                    return Err(SignalStoreError::Unavailable);
                }
                self.batches.lock().unwrap().push(batch);
                Ok(DurableOutcome::Accepted)
            })
        }
    }

    fn one_counter() -> MetricDeltaBatch {
        let mut batch = MetricDeltaBatch::default();
        batch.push(MetricDelta {
            series: MetricSeries {
                project_id: ProjectId::new(7).unwrap(),
                name: "requests".into(),
                kind: MetricKind::Counter,
                unit: "none".into(),
                tags: Default::default(),
            },
            bucket_start: Timestamp::from_unix_millis(60_000).unwrap(),
            bucket_width_seconds: 60,
            received_at: Timestamp::from_unix_millis(60_001).unwrap(),
            trace_id: None,
            aggregate: MetricAggregate::from_measurement(MetricKind::Counter, 1.0),
        });
        batch
    }

    #[tokio::test]
    async fn hot_series_collapses_across_concurrent_requests() {
        let root = ShutdownRoot::new();
        let store = Arc::new(RecordingStore {
            batches: Mutex::new(Vec::new()),
            fail_once: AtomicBool::new(false),
        });
        let (writer, task) = MetricWriter::start(
            Arc::clone(&store) as Arc<dyn MetricStore>,
            MetricWriterConfig {
                max_wait: Duration::from_millis(50),
                ..MetricWriterConfig::default()
            },
            root.signal(),
        )
        .unwrap();
        let requests = (0..100)
            .map(|_| {
                let writer = Arc::clone(&writer);
                tokio::spawn(async move { writer.persist_metrics(one_counter()).await })
            })
            .collect::<Vec<_>>();
        for request in requests {
            assert_eq!(request.await.unwrap(), Ok(DurableOutcome::Accepted));
        }
        {
            let batches = store.batches.lock().unwrap();
            assert_eq!(batches.len(), 1);
            assert_eq!(batches[0].len(), 1);
            let MetricAggregate::Counter { count, sum } =
                &batches[0].deltas.values().next().unwrap().aggregate
            else {
                panic!("counter");
            };
            assert_eq!((*count, *sum), (100, 100.0));
        }
        root.begin();
        task.wait().await;
    }

    #[tokio::test]
    async fn storage_failure_does_not_kill_metric_lane() {
        let root = ShutdownRoot::new();
        let store = Arc::new(RecordingStore {
            batches: Mutex::new(Vec::new()),
            fail_once: AtomicBool::new(true),
        });
        let (writer, task) = MetricWriter::start(
            Arc::clone(&store) as Arc<dyn MetricStore>,
            MetricWriterConfig::default(),
            root.signal(),
        )
        .unwrap();
        assert_eq!(
            writer.persist_metrics(one_counter()).await,
            Err(SignalStoreError::Unavailable)
        );
        assert_eq!(
            writer.persist_metrics(one_counter()).await,
            Ok(DurableOutcome::Accepted)
        );
        root.begin();
        task.wait().await;
    }
}
