//! Independent bounded micro-batch writer for Cron check-ins.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use metric_domain::monitors::MonitorUpdate;
use metric_ports::{DurableOutcome, MonitorSink, MonitorStore, PortFuture, SignalStoreError};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Duration, Instant, timeout},
};

use crate::shutdown::ShutdownSignal;

#[derive(Debug, Clone, Copy)]
pub struct MonitorWriterConfig {
    pub channel_capacity: usize,
    pub max_updates: usize,
    pub max_wait: Duration,
    pub operation_timeout: Duration,
    pub shutdown_drain: Duration,
}

impl Default for MonitorWriterConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 1_024,
            max_updates: 200,
            max_wait: Duration::from_millis(5),
            operation_timeout: Duration::from_secs(5),
            shutdown_drain: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MonitorWriterStartError {
    #[error("MonitorWriter configuration is invalid")]
    InvalidConfiguration,
}

struct Command {
    updates: Vec<MonitorUpdate>,
    response: oneshot::Sender<Result<Vec<DurableOutcome>, SignalStoreError>>,
}

pub struct MonitorWriter {
    sender: mpsc::Sender<Command>,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
    max_updates: usize,
}

impl MonitorWriter {
    pub fn start(
        store: Arc<dyn MonitorStore>,
        config: MonitorWriterConfig,
        shutdown: ShutdownSignal,
    ) -> Result<(Arc<Self>, MonitorWriterTask), MonitorWriterStartError> {
        if config.channel_capacity == 0
            || config.max_updates == 0
            || config.max_wait.is_zero()
            || config.operation_timeout.is_zero()
            || config.shutdown_drain.is_zero()
        {
            return Err(MonitorWriterStartError::InvalidConfiguration);
        }
        let (sender, receiver) = mpsc::channel(config.channel_capacity);
        let accepting = Arc::new(AtomicBool::new(true));
        let writer = Arc::new(Self {
            sender,
            accepting: Arc::clone(&accepting),
            shutdown: shutdown.clone(),
            max_updates: config.max_updates,
        });
        let join = tokio::spawn(run(store, receiver, config, accepting, shutdown));
        Ok((writer, MonitorWriterTask { join }))
    }
}

impl MonitorSink for MonitorWriter {
    fn persist_monitors(
        &self,
        updates: Vec<MonitorUpdate>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(async move {
            if updates.is_empty() {
                return Ok(Vec::new());
            }
            if updates.len() > self.max_updates {
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
            permit.send(Command { updates, response });
            receiver.await.unwrap_or(Err(SignalStoreError::Unavailable))
        })
    }
}

pub struct MonitorWriterTask {
    join: JoinHandle<()>,
}

impl MonitorWriterTask {
    pub fn abort_handle(&self) -> tokio::task::AbortHandle {
        self.join.abort_handle()
    }

    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

async fn run(
    store: Arc<dyn MonitorStore>,
    mut receiver: mpsc::Receiver<Command>,
    config: MonitorWriterConfig,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
) {
    loop {
        let first = tokio::select! {
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
        };
        let mut commands = vec![first];
        let mut count = commands[0].updates.len();
        let deadline = Instant::now() + config.max_wait;
        while count < config.max_updates {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match timeout(remaining, receiver.recv()).await {
                Ok(Some(command)) if count + command.updates.len() <= config.max_updates => {
                    count += command.updates.len();
                    commands.push(command);
                }
                Ok(Some(command)) => {
                    persist(&store, commands, config.operation_timeout).await;
                    commands = vec![command];
                    break;
                }
                _ => break,
            }
        }
        persist(&store, commands, config.operation_timeout).await;
    }
}

async fn persist(
    store: &Arc<dyn MonitorStore>,
    commands: Vec<Command>,
    operation_timeout: Duration,
) {
    let lengths = commands
        .iter()
        .map(|command| command.updates.len())
        .collect::<Vec<_>>();
    let updates = commands
        .iter()
        .flat_map(|command| command.updates.iter().cloned())
        .collect::<Vec<_>>();
    let result = timeout(operation_timeout, store.persist_monitors(updates))
        .await
        .unwrap_or(Err(SignalStoreError::Unavailable));
    let mut offset = 0;
    for (command, length) in commands.into_iter().zip(lengths) {
        let response = match &result {
            Ok(outcomes) if offset + length <= outcomes.len() => {
                Ok(outcomes[offset..offset + length].to_vec())
            }
            Ok(_) => Err(SignalStoreError::Unavailable),
            Err(error) => Err(*error),
        };
        offset += length;
        let _ = command.response.send(response);
    }
}

async fn drain(
    store: &Arc<dyn MonitorStore>,
    receiver: &mut mpsc::Receiver<Command>,
    config: MonitorWriterConfig,
) {
    receiver.close();
    let deadline = Instant::now() + config.shutdown_drain;
    while let Ok(command) = receiver.try_recv() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = command.response.send(Err(SignalStoreError::Unavailable));
            continue;
        }
        persist(
            store,
            vec![command],
            remaining.min(config.operation_timeout),
        )
        .await;
    }
}
