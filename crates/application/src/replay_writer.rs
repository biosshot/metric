//! Isolated byte-bounded Session Replay BlobStore and metadata writer.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use metric_domain::{
    Timestamp,
    blob::{BlobChecksum, BlobKey, BlobKind, BlobNamespace},
    replays::{ReplaySegment, ReplaySegmentCommit, ReplaySubmission},
};
use metric_ports::{
    BlobScanRequest, BlobStore, BlobStoreError, Clock, DurableOutcome, PortFuture, ReplaySink,
    ReplayStore, SignalStoreError,
};
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    task::JoinHandle,
    time::{MissedTickBehavior, interval, timeout},
};

use crate::shutdown::ShutdownSignal;

#[derive(Debug, Clone, Copy)]
pub struct ReplayWriterConfig {
    pub channel_capacity: usize,
    pub max_queued_bytes: u32,
    pub max_segment_bytes: usize,
    pub operation_timeout: Duration,
    pub shutdown_drain: Duration,
    pub orphan_grace: Duration,
    pub cleanup_interval: Duration,
    pub cleanup_batch_size: usize,
}

impl Default for ReplayWriterConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 32,
            max_queued_bytes: 32 * 1024 * 1024,
            max_segment_bytes: 5 * 1024 * 1024,
            operation_timeout: Duration::from_secs(15),
            shutdown_drain: Duration::from_secs(15),
            orphan_grace: Duration::from_secs(60 * 60),
            cleanup_interval: Duration::from_secs(5 * 60),
            cleanup_batch_size: 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReplayWriterStartError {
    #[error("ReplayWriter configuration is invalid")]
    InvalidConfiguration,
}

struct Command {
    submission: ReplaySubmission,
    response: oneshot::Sender<Result<DurableOutcome, SignalStoreError>>,
    _bytes: OwnedSemaphorePermit,
}

pub struct ReplayWriter {
    sender: mpsc::Sender<Command>,
    bytes: Arc<Semaphore>,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
    max_segment_bytes: usize,
}

impl ReplayWriter {
    pub fn start(
        store: Arc<dyn ReplayStore>,
        blobs: Arc<dyn BlobStore>,
        clock: Arc<dyn Clock>,
        config: ReplayWriterConfig,
        shutdown: ShutdownSignal,
    ) -> Result<(Arc<Self>, ReplayWriterTask), ReplayWriterStartError> {
        if config.channel_capacity == 0
            || config.max_queued_bytes == 0
            || config.max_segment_bytes == 0
            || config.operation_timeout.is_zero()
            || config.shutdown_drain.is_zero()
            || config.orphan_grace.is_zero()
            || config.cleanup_interval.is_zero()
            || config.cleanup_batch_size == 0
            || config.cleanup_batch_size > 10_000
        {
            return Err(ReplayWriterStartError::InvalidConfiguration);
        }
        let (sender, receiver) = mpsc::channel(config.channel_capacity);
        let accepting = Arc::new(AtomicBool::new(true));
        let writer = Arc::new(Self {
            sender,
            bytes: Arc::new(Semaphore::new(config.max_queued_bytes as usize)),
            accepting: Arc::clone(&accepting),
            shutdown: shutdown.clone(),
            max_segment_bytes: config.max_segment_bytes,
        });
        let join = tokio::spawn(run_writer(
            store, blobs, clock, receiver, config, accepting, shutdown,
        ));
        Ok((writer, ReplayWriterTask { join }))
    }
}

impl ReplaySink for ReplayWriter {
    fn persist_replay(
        &self,
        submission: ReplaySubmission,
    ) -> PortFuture<'_, Result<DurableOutcome, SignalStoreError>> {
        Box::pin(async move {
            if submission.recording.is_empty()
                || submission.recording.len() > self.max_segment_bytes
                || !self.accepting.load(Ordering::Acquire)
                || self.shutdown.is_cancelled()
            {
                return Err(SignalStoreError::Capacity);
            }
            let bytes = u32::try_from(submission.recording.len())
                .map_err(|_| SignalStoreError::Capacity)?;
            let byte_permit = Arc::clone(&self.bytes)
                .try_acquire_many_owned(bytes)
                .map_err(|_| SignalStoreError::Capacity)?;
            let queue_permit = self
                .sender
                .try_reserve()
                .map_err(|_| SignalStoreError::Capacity)?;
            let (response, receiver) = oneshot::channel();
            queue_permit.send(Command {
                submission,
                response,
                _bytes: byte_permit,
            });
            receiver.await.unwrap_or(Err(SignalStoreError::Unavailable))
        })
    }
}

pub struct ReplayWriterTask {
    join: JoinHandle<()>,
}

impl ReplayWriterTask {
    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

async fn run_writer(
    store: Arc<dyn ReplayStore>,
    blobs: Arc<dyn BlobStore>,
    clock: Arc<dyn Clock>,
    mut receiver: mpsc::Receiver<Command>,
    config: ReplayWriterConfig,
    accepting: Arc<AtomicBool>,
    shutdown: ShutdownSignal,
) {
    let mut cleanup = interval(config.cleanup_interval);
    cleanup.set_missed_tick_behavior(MissedTickBehavior::Skip);
    cleanup.tick().await;
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                accepting.store(false, Ordering::Release);
                drain(&store, &blobs, &mut receiver, config).await;
                return;
            }
            command = receiver.recv() => match command {
                Some(command) => flush(&store, &blobs, command, config.operation_timeout).await,
                None => return,
            },
            _ = cleanup.tick() => {
                let _ = cleanup_orphans(
                    &store,
                    &blobs,
                    clock.now(),
                    config.orphan_grace,
                    config.cleanup_batch_size,
                ).await;
            }
        }
    }
}

async fn drain(
    store: &Arc<dyn ReplayStore>,
    blobs: &Arc<dyn BlobStore>,
    receiver: &mut mpsc::Receiver<Command>,
    config: ReplayWriterConfig,
) {
    let deadline = tokio::time::Instant::now() + config.shutdown_drain;
    while let Ok(command) = receiver.try_recv() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            let _ = command.response.send(Err(SignalStoreError::Unavailable));
            reject_remaining(receiver);
            return;
        }
        flush(
            store,
            blobs,
            command,
            remaining.min(config.operation_timeout),
        )
        .await;
    }
}

fn reject_remaining(receiver: &mut mpsc::Receiver<Command>) {
    while let Ok(command) = receiver.try_recv() {
        let _ = command.response.send(Err(SignalStoreError::Unavailable));
    }
}

async fn flush(
    store: &Arc<dyn ReplayStore>,
    blobs: &Arc<dyn BlobStore>,
    command: Command,
    operation_timeout: Duration,
) {
    let result = timeout(
        operation_timeout,
        persist_one(store, blobs, command.submission),
    )
    .await
    .unwrap_or(Err(SignalStoreError::Unavailable));
    let _ = command.response.send(result);
}

async fn persist_one(
    store: &Arc<dyn ReplayStore>,
    blobs: &Arc<dyn BlobStore>,
    submission: ReplaySubmission,
) -> Result<DurableOutcome, SignalStoreError> {
    let key = BlobKey::replay_recording(
        submission.metadata.project_id,
        submission.metadata.replay_id,
        submission.metadata.segment_id,
    );
    if let Ok(existing) = store
        .load_replay(
            submission.metadata.project_id,
            submission.metadata.replay_id,
        )
        .await
        && let Some(segment) = existing
            .segments
            .iter()
            .find(|segment| segment.segment_id == submission.metadata.segment_id)
    {
        let checksum = BlobChecksum::from_bytes(*blake3::hash(&submission.recording).as_bytes());
        return if segment.object.size == submission.recording.len() as u64
            && segment.object.checksum == checksum
        {
            Ok(DurableOutcome::Duplicate)
        } else {
            Err(SignalStoreError::Conflict)
        };
    }
    let mut write = blobs
        .begin(BlobKind::ReplayRecording, submission.metadata.received_at)
        .await
        .map_err(map_blob)?;
    write
        .write_chunk(submission.recording)
        .await
        .map_err(map_blob)?;
    let object = write.commit(key).await.map_err(map_blob)?;
    store
        .persist_replay_segment(ReplaySegmentCommit {
            metadata: submission.metadata,
            segment: ReplaySegment {
                segment_id: object
                    .key
                    .replay_relation()
                    .map_err(|_| SignalStoreError::InvalidData)?
                    .2,
                object,
                decompressed_bytes: submission.decompressed_bytes,
                event_count: submission.event_count,
            },
        })
        .await
}

async fn cleanup_orphans(
    store: &Arc<dyn ReplayStore>,
    blobs: &Arc<dyn BlobStore>,
    now: Timestamp,
    orphan_grace: Duration,
    limit: usize,
) -> Result<(), SignalStoreError> {
    let grace = i64::try_from(orphan_grace.as_millis()).unwrap_or(i64::MAX);
    let cutoff = Timestamp::from_unix_millis(now.unix_millis().saturating_sub(grace))
        .map_err(|_| SignalStoreError::InvalidData)?;
    let page = blobs
        .scan(BlobScanRequest {
            namespace: BlobNamespace::ReplayRecordings,
            older_than: cutoff,
            cursor: None,
            limit,
        })
        .await
        .map_err(map_blob)?;
    for object in page.objects {
        if !store.references_replay_blob(&object.key).await? {
            blobs.delete(&object.key).await.map_err(map_blob)?;
        }
    }
    Ok(())
}

const fn map_blob(error: BlobStoreError) -> SignalStoreError {
    match error {
        BlobStoreError::TooLarge | BlobStoreError::Capacity => SignalStoreError::Capacity,
        BlobStoreError::NotFound => SignalStoreError::NotFound,
        BlobStoreError::Corrupt | BlobStoreError::Invalid => SignalStoreError::InvalidData,
        BlobStoreError::Unavailable => SignalStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use metric_blob::{LocalBlobConfig, LocalBlobStore};
    use metric_domain::{
        EventId, ProjectId,
        replays::{
            ReplayMetadata, ReplayPage, ReplayRecord, ReplaySegmentCommit, ReplaySubmission,
        },
    };
    use metric_ports::{BlobStore, ReplayQuery};

    use super::*;
    use crate::shutdown::ShutdownRoot;

    #[derive(Default)]
    struct FakeReplayStore(Mutex<Vec<ReplayRecord>>);

    impl ReplayStore for FakeReplayStore {
        fn persist_replay_segment(
            &self,
            commit: ReplaySegmentCommit,
        ) -> PortFuture<'_, Result<DurableOutcome, SignalStoreError>> {
            Box::pin(async move {
                let mut records = self.0.lock().expect("fake Replay store lock poisoned");
                let position = records
                    .iter()
                    .position(|record| {
                        record.project_id == commit.metadata.project_id
                            && record.replay_id == commit.metadata.replay_id
                    })
                    .unwrap_or_else(|| {
                        records.push(ReplayRecord {
                            project_id: commit.metadata.project_id,
                            replay_id: commit.metadata.replay_id,
                            started_at: commit.metadata.started_at,
                            ended_at: commit.metadata.ended_at,
                            received_at: commit.metadata.received_at,
                            environment: commit.metadata.environment.clone(),
                            release: commit.metadata.release.clone(),
                            url: commit.metadata.url.clone(),
                            error_ids: commit.metadata.error_ids.clone(),
                            trace_ids: commit.metadata.trace_ids.clone(),
                            segments: Vec::new(),
                            expires_at: None,
                        });
                        records.len() - 1
                    });
                let record = &mut records[position];
                if record
                    .segments
                    .iter()
                    .any(|segment| segment.segment_id == commit.segment.segment_id)
                {
                    return Ok(DurableOutcome::Duplicate);
                }
                record.started_at = record.started_at.min(commit.metadata.started_at);
                record.ended_at = record.ended_at.max(commit.metadata.ended_at);
                record.segments.push(commit.segment);
                record.segments.sort_by_key(|segment| segment.segment_id);
                Ok(DurableOutcome::Accepted)
            })
        }

        fn list_replays(
            &self,
            project_id: ProjectId,
            query: ReplayQuery,
        ) -> PortFuture<'_, Result<ReplayPage, SignalStoreError>> {
            Box::pin(async move {
                let records = self.0.lock().expect("fake Replay store lock poisoned");
                let items = records
                    .iter()
                    .filter(|record| record.project_id == project_id)
                    .take(query.limit)
                    .cloned()
                    .collect();
                Ok(ReplayPage { items, next: None })
            })
        }

        fn load_replay(
            &self,
            project_id: ProjectId,
            replay_id: EventId,
        ) -> PortFuture<'_, Result<ReplayRecord, SignalStoreError>> {
            Box::pin(async move {
                self.0
                    .lock()
                    .expect("fake Replay store lock poisoned")
                    .iter()
                    .find(|record| record.project_id == project_id && record.replay_id == replay_id)
                    .cloned()
                    .ok_or(SignalStoreError::NotFound)
            })
        }

        fn references_replay_blob(
            &self,
            key: &BlobKey,
        ) -> PortFuture<'_, Result<bool, SignalStoreError>> {
            let key = key.clone();
            Box::pin(async move {
                Ok(self
                    .0
                    .lock()
                    .expect("fake Replay store lock poisoned")
                    .iter()
                    .flat_map(|record| &record.segments)
                    .any(|segment| segment.object.key == key))
            })
        }
    }

    #[derive(Clone, Copy)]
    struct FixedClock(Timestamp);

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    #[tokio::test]
    async fn partial_replay_survives_writer_restart_and_keeps_segment_order() {
        let directory =
            std::env::temp_dir().join(format!("metric-replay-writer-{}", uuid::Uuid::new_v4()));
        let blobs = Arc::new(
            LocalBlobStore::new(
                &directory,
                LocalBlobConfig {
                    capacity_bytes: 1024 * 1024,
                    reserve_bytes: 0,
                    max_object_bytes: 64 * 1024,
                },
            )
            .await
            .unwrap(),
        );
        let store = Arc::new(FakeReplayStore::default());
        let clock = Arc::new(FixedClock(timestamp(10_000)));

        let first_root = ShutdownRoot::new();
        let (first, first_task) = ReplayWriter::start(
            store.clone(),
            blobs.clone(),
            clock.clone(),
            ReplayWriterConfig::default(),
            first_root.signal(),
        )
        .unwrap();
        assert_eq!(
            first.persist_replay(submission(0)).await.unwrap(),
            DurableOutcome::Accepted
        );
        first_root.begin();
        first_task.wait().await;

        let second_root = ShutdownRoot::new();
        let (second, second_task) = ReplayWriter::start(
            store.clone(),
            blobs,
            clock,
            ReplayWriterConfig::default(),
            second_root.signal(),
        )
        .unwrap();
        assert_eq!(
            second.persist_replay(submission(2)).await.unwrap(),
            DurableOutcome::Accepted
        );
        let record = store
            .load_replay(ProjectId::new(42).unwrap(), EventId::from_bytes([3; 16]))
            .await
            .unwrap();
        assert_eq!(
            record
                .segments
                .iter()
                .map(|segment| segment.segment_id)
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        second_root.begin();
        second_task.wait().await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn orphan_from_interrupted_metadata_commit_is_removed_after_grace() {
        let directory =
            std::env::temp_dir().join(format!("metric-replay-orphan-{}", uuid::Uuid::new_v4()));
        let blobs = Arc::new(
            LocalBlobStore::new(
                &directory,
                LocalBlobConfig {
                    capacity_bytes: 1024 * 1024,
                    reserve_bytes: 0,
                    max_object_bytes: 64 * 1024,
                },
            )
            .await
            .unwrap(),
        );
        let key =
            BlobKey::replay_recording(ProjectId::new(42).unwrap(), EventId::from_bytes([3; 16]), 0);
        let mut writer = blobs
            .begin(BlobKind::ReplayRecording, timestamp(1_000))
            .await
            .unwrap();
        writer.write_chunk(Box::from(*b"orphan")).await.unwrap();
        writer.commit(key.clone()).await.unwrap();

        cleanup_orphans(
            &(Arc::new(FakeReplayStore::default()) as Arc<dyn ReplayStore>),
            &(blobs.clone() as Arc<dyn BlobStore>),
            timestamp(4_000_000_000_000),
            Duration::from_secs(1),
            10,
        )
        .await
        .unwrap();
        assert!(matches!(
            blobs.open(&key).await,
            Err(BlobStoreError::NotFound)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn replay_byte_budget_rejects_before_blob_or_metadata_admission() {
        let directory =
            std::env::temp_dir().join(format!("metric-replay-budget-{}", uuid::Uuid::new_v4()));
        let blobs = Arc::new(
            LocalBlobStore::new(
                &directory,
                LocalBlobConfig {
                    capacity_bytes: 1024 * 1024,
                    reserve_bytes: 0,
                    max_object_bytes: 64 * 1024,
                },
            )
            .await
            .unwrap(),
        );
        let store = Arc::new(FakeReplayStore::default());
        let root = ShutdownRoot::new();
        let (writer, task) = ReplayWriter::start(
            store.clone(),
            blobs.clone(),
            Arc::new(FixedClock(timestamp(10_000))),
            ReplayWriterConfig {
                max_queued_bytes: 4,
                ..ReplayWriterConfig::default()
            },
            root.signal(),
        )
        .unwrap();

        assert_eq!(
            writer.persist_replay(submission(0)).await,
            Err(SignalStoreError::Capacity)
        );
        assert!(
            store
                .0
                .lock()
                .expect("fake Replay store lock poisoned")
                .is_empty()
        );
        let page = blobs
            .scan(BlobScanRequest {
                namespace: BlobNamespace::ReplayRecordings,
                older_than: timestamp(20_000),
                cursor: None,
                limit: 10,
            })
            .await
            .unwrap();
        assert!(page.objects.is_empty());

        root.begin();
        task.wait().await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn submission(segment_id: u32) -> ReplaySubmission {
        ReplaySubmission {
            metadata: ReplayMetadata {
                project_id: ProjectId::new(42).unwrap(),
                replay_id: EventId::from_bytes([3; 16]),
                segment_id,
                started_at: timestamp(1_000),
                ended_at: timestamp(2_000 + i64::from(segment_id)),
                received_at: timestamp(3_000 + i64::from(segment_id)),
                environment: Some("test".into()),
                release: None,
                url: None,
                error_ids: Vec::new(),
                trace_ids: Vec::new(),
            },
            recording: format!("{{\"segment_id\":{segment_id}}}\n[]")
                .into_bytes()
                .into_boxed_slice(),
            decompressed_bytes: 2,
            event_count: 0,
        }
    }

    fn timestamp(value: i64) -> Timestamp {
        Timestamp::from_unix_millis(value).unwrap()
    }
}
