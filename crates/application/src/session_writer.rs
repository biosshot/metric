//! Dedicated bounded Session lifecycle micro-batching.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use metric_domain::sessions::SessionUpdate;
use metric_ports::{
    Clock, DurableOutcome, PortFuture, SessionSink, SessionStore, SignalStoreError,
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant as TokioInstant, sleep_until, timeout},
};

use crate::shutdown::ShutdownSignal;

const SESSION_UPDATE_BYTES: usize = 192;

#[derive(Debug, Clone, Copy)]
pub struct SessionWriterConfig {
    pub channel_capacity: usize,
    pub max_wait: Duration,
    pub max_updates: usize,
    pub operation_timeout: Duration,
    pub shutdown_drain: Duration,
}

impl Default for SessionWriterConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 512,
            max_wait: Duration::from_millis(20),
            max_updates: 250,
            operation_timeout: Duration::from_secs(10),
            shutdown_drain: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SessionWriterStartError {
    #[error("SessionWriter configuration is invalid")]
    InvalidConfiguration,
}

struct Command {
    updates: Vec<SessionUpdate>,
    response: oneshot::Sender<Result<Vec<DurableOutcome>, SignalStoreError>>,
}

pub struct SessionWriter {
    sender: mpsc::Sender<Command>,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
    max_updates: usize,
}

impl SessionWriter {
    pub fn start(
        store: Arc<dyn SessionStore>,
        config: SessionWriterConfig,
        shutdown: ShutdownSignal,
    ) -> Result<(Arc<Self>, SessionWriterTask), SessionWriterStartError> {
        if config.channel_capacity == 0
            || config.max_updates == 0
            || config.max_wait.is_zero()
            || config.operation_timeout.is_zero()
            || config.shutdown_drain.is_zero()
        {
            return Err(SessionWriterStartError::InvalidConfiguration);
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
        Ok((writer, SessionWriterTask { join }))
    }
}

impl SessionSink for SessionWriter {
    fn persist_sessions(
        &self,
        updates: Vec<SessionUpdate>,
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

pub struct SessionWriterTask {
    join: JoinHandle<()>,
}

pub struct SessionMaintenanceTask {
    join: JoinHandle<()>,
}

impl SessionMaintenanceTask {
    pub fn abort_handle(&self) -> tokio::task::AbortHandle {
        self.join.abort_handle()
    }

    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

#[must_use]
pub fn start_session_maintenance(
    store: Arc<dyn SessionStore>,
    clock: Arc<dyn Clock>,
    maximum_active_age: Duration,
    interval: Duration,
    shutdown: ShutdownSignal,
) -> SessionMaintenanceTask {
    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(interval) => {
                    let _ = store
                        .terminalize_stale_sessions(clock.now(), maximum_active_age)
                        .await;
                }
            }
        }
    });
    SessionMaintenanceTask { join }
}

impl SessionWriterTask {
    pub fn abort_handle(&self) -> tokio::task::AbortHandle {
        self.join.abort_handle()
    }

    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

async fn run(
    store: Arc<dyn SessionStore>,
    mut receiver: mpsc::Receiver<Command>,
    config: SessionWriterConfig,
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
        let mut updates = commands[0].updates.len();
        let deadline = TokioInstant::now() + config.max_wait;
        while updates < config.max_updates {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => {
                    accepting.store(false, Ordering::Release);
                    break;
                }
                () = sleep_until(deadline) => break,
                command = receiver.recv() => match command {
                    Some(command) if updates + command.updates.len() <= config.max_updates => {
                        updates += command.updates.len();
                        commands.push(command);
                    }
                    Some(command) => {
                        flush(&store, commands, config.operation_timeout).await;
                        commands = vec![command];
                        break;
                    }
                    None => break,
                }
            }
        }
        flush(&store, commands, config.operation_timeout).await;
        if shutdown.is_cancelled() {
            drain(&store, &mut receiver, config).await;
            return;
        }
    }
}

async fn drain(
    store: &Arc<dyn SessionStore>,
    receiver: &mut mpsc::Receiver<Command>,
    config: SessionWriterConfig,
) {
    let started = Instant::now();
    while started.elapsed() < config.shutdown_drain {
        let Ok(first) = receiver.try_recv() else {
            return;
        };
        let mut commands = vec![first];
        let mut count = commands[0].updates.len();
        while count < config.max_updates {
            let Ok(command) = receiver.try_recv() else {
                break;
            };
            if count + command.updates.len() > config.max_updates {
                let _ = command.response.send(Err(SignalStoreError::Unavailable));
                break;
            }
            count += command.updates.len();
            commands.push(command);
        }
        flush(store, commands, config.operation_timeout).await;
    }
    while let Ok(command) = receiver.try_recv() {
        let _ = command.response.send(Err(SignalStoreError::Unavailable));
    }
}

async fn flush(
    store: &Arc<dyn SessionStore>,
    mut commands: Vec<Command>,
    operation_timeout: Duration,
) {
    let counts = commands
        .iter()
        .map(|command| command.updates.len())
        .collect::<Vec<_>>();
    let updates = commands
        .iter_mut()
        .flat_map(|command| std::mem::take(&mut command.updates))
        .collect::<Vec<_>>();
    let update_count = updates.len();
    metrics::histogram!("metric_session_writer_batch_updates").record(update_count as f64);
    metrics::histogram!("metric_session_writer_batch_bytes")
        .record((update_count * SESSION_UPDATE_BYTES) as f64);
    let result = timeout(operation_timeout, store.persist_sessions(updates)).await;
    match result {
        Ok(Ok(outcomes)) if outcomes.len() == update_count => {
            let mut offset = 0;
            for (command, count) in commands.into_iter().zip(counts) {
                let next = offset + count;
                let _ = command.response.send(Ok(outcomes[offset..next].to_vec()));
                offset = next;
            }
        }
        Ok(Err(error)) => reject(commands, error),
        Err(_) | Ok(Ok(_)) => reject(commands, SignalStoreError::Unavailable),
    }
}

fn reject(commands: Vec<Command>, error: SignalStoreError) {
    for command in commands {
        let _ = command.response.send(Err(error));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use metric_domain::{
        ProjectId, Timestamp,
        finalization::{EnvironmentId, ReleaseId},
        sessions::{SessionId, SessionRecord, SessionState},
    };

    use super::*;

    struct FakeStore(Mutex<Vec<usize>>);

    impl SessionStore for FakeStore {
        fn persist_sessions(
            &self,
            updates: Vec<SessionUpdate>,
        ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(updates.len());
                Ok(vec![DurableOutcome::Accepted; updates.len()])
            })
        }

        fn load_session(
            &self,
            _project_id: ProjectId,
            _session_id: SessionId,
        ) -> PortFuture<'_, Result<SessionRecord, SignalStoreError>> {
            Box::pin(async { Err(SignalStoreError::NotFound) })
        }

        fn terminalize_stale_sessions(
            &self,
            _now: Timestamp,
            _maximum_active_age: Duration,
        ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
            Box::pin(async { Ok(0) })
        }

        fn release_health(
            &self,
            _project_id: ProjectId,
            _release_id: ReleaseId,
            _from: Timestamp,
            _until: Timestamp,
        ) -> PortFuture<
            '_,
            Result<Vec<metric_domain::sessions::ReleaseHealthBucket>, SignalStoreError>,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn rebuild_session_stats(
            &self,
            _project_id: ProjectId,
            _from: Timestamp,
            _until: Timestamp,
        ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
            Box::pin(async { Ok(0) })
        }
    }

    fn update(index: u8) -> SessionUpdate {
        let project = ProjectId::new(42).unwrap();
        SessionUpdate {
            id: SessionId::derive(project, [index; 16]),
            project_id: project,
            release_id: ReleaseId::from_bytes([3; 16]),
            environment_id: EnvironmentId::from_bytes([4; 16]),
            started_at: Timestamp::from_unix_millis(1_000).unwrap(),
            updated_at: Timestamp::from_unix_millis(1_100).unwrap(),
            state: SessionState::Ok,
            sequence: Some(1),
            duration_ms: None,
            user_digest: None,
        }
    }

    #[tokio::test]
    async fn concurrent_session_updates_use_their_own_micro_batch() {
        let root = crate::shutdown::ShutdownRoot::new();
        let store = Arc::new(FakeStore(Mutex::new(Vec::new())));
        let port: Arc<dyn SessionStore> = store.clone();
        let (writer, task) =
            SessionWriter::start(port, SessionWriterConfig::default(), root.signal()).unwrap();
        let mut joins = Vec::new();
        for index in 1..=32 {
            let writer = Arc::clone(&writer);
            joins.push(tokio::spawn(async move {
                writer.persist_sessions(vec![update(index)]).await
            }));
        }
        for join in joins {
            assert!(join.await.unwrap().is_ok());
        }
        assert_eq!(store.0.lock().unwrap().iter().sum::<usize>(), 32);
        root.begin();
        task.wait().await;
    }
}
