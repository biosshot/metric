//! Bounded reconciliation for published event-owned blobs without a MongoDB parent.

use std::{sync::Arc, time::Duration};

use metric_domain::{Timestamp, blob::BlobNamespace};
use metric_ports::{
    BlobReference, BlobReferenceStore, BlobScanRequest, BlobStore, BlobStoreError, Clock,
};
use thiserror::Error;

use crate::shutdown::ShutdownSignal;

#[derive(Debug, Clone, Copy)]
pub struct BlobCleanupConfig {
    pub orphan_grace: Duration,
    pub interval: Duration,
    pub batch_size: usize,
    pub max_pages_per_run: usize,
}

impl Default for BlobCleanupConfig {
    fn default() -> Self {
        Self {
            orphan_grace: Duration::from_secs(24 * 60 * 60),
            interval: Duration::from_secs(15 * 60),
            batch_size: 256,
            max_pages_per_run: 16,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlobCleanupReport {
    pub scanned: u64,
    pub referenced: u64,
    pub deleted: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BlobCleanupError {
    #[error("blob cleanup configuration is invalid")]
    InvalidConfiguration,
    #[error("blob cleanup storage operation failed")]
    Storage,
}

pub struct BlobCleanupService {
    blobs: Arc<dyn BlobStore>,
    references: Arc<dyn BlobReferenceStore>,
    clock: Arc<dyn Clock>,
    config: BlobCleanupConfig,
}

impl BlobCleanupService {
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        references: Arc<dyn BlobReferenceStore>,
        clock: Arc<dyn Clock>,
        config: BlobCleanupConfig,
    ) -> Result<Self, BlobCleanupError> {
        if config.orphan_grace.is_zero()
            || config.interval.is_zero()
            || config.batch_size == 0
            || config.batch_size > 10_000
            || config.max_pages_per_run == 0
            || config.max_pages_per_run > 1024
        {
            return Err(BlobCleanupError::InvalidConfiguration);
        }
        Ok(Self {
            blobs,
            references,
            clock,
            config,
        })
    }

    pub async fn run_once(&self) -> Result<BlobCleanupReport, BlobCleanupError> {
        let grace_millis = i64::try_from(self.config.orphan_grace.as_millis())
            .map_err(|_| BlobCleanupError::InvalidConfiguration)?;
        let cutoff = Timestamp::from_unix_millis(
            self.clock
                .now()
                .unix_millis()
                .checked_sub(grace_millis)
                .ok_or(BlobCleanupError::InvalidConfiguration)?,
        )
        .map_err(|_| BlobCleanupError::InvalidConfiguration)?;
        let mut cursor = None;
        let mut report = BlobCleanupReport::default();
        for _ in 0..self.config.max_pages_per_run {
            let page = self
                .blobs
                .scan(BlobScanRequest {
                    namespace: BlobNamespace::EventOwned,
                    older_than: cutoff,
                    cursor,
                    limit: self.config.batch_size,
                })
                .await
                .map_err(map_storage)?;
            for object in page.objects {
                report.scanned = report.scanned.saturating_add(1);
                let (project_id, event_id, object_id) = object
                    .key
                    .event_relation()
                    .map_err(|_| BlobCleanupError::Storage)?;
                if self
                    .references
                    .is_referenced(BlobReference {
                        project_id,
                        event_id,
                        object_id,
                    })
                    .await
                    .map_err(map_storage)?
                {
                    report.referenced = report.referenced.saturating_add(1);
                } else {
                    self.blobs.delete(&object.key).await.map_err(map_storage)?;
                    report.deleted = report.deleted.saturating_add(1);
                }
            }
            let Some(next) = page.next_cursor else {
                break;
            };
            cursor = Some(next);
        }
        Ok(report)
    }
}

pub struct BlobCleanupTask {
    handle: tokio::task::JoinHandle<()>,
}

impl BlobCleanupTask {
    pub async fn wait(self) {
        let _ = self.handle.await;
    }
}

pub fn start_blob_cleanup_worker(
    service: Arc<BlobCleanupService>,
    shutdown: ShutdownSignal,
) -> BlobCleanupTask {
    let interval = service.config.interval;
    BlobCleanupTask {
        handle: tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        if let Err(error) = service.run_once().await {
                            tracing::warn!(
                                operation = "blob.cleanup",
                                error = %error,
                                "blob cleanup run failed"
                            );
                        }
                    }
                }
            }
        }),
    }
}

fn map_storage(_: BlobStoreError) -> BlobCleanupError {
    BlobCleanupError::Storage
}

#[cfg(test)]
mod tests {
    use super::*;
    use metric_blob::{LocalBlobConfig, LocalBlobStore};
    use metric_domain::{
        EventId, ProjectId,
        blob::{BlobKind, BlobObjectId},
    };
    use metric_ports::{BlobReferenceStore, PortFuture};

    struct FixedClock(Timestamp);

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    struct References(BlobObjectId);

    impl BlobReferenceStore for References {
        fn is_referenced(
            &self,
            reference: BlobReference,
        ) -> PortFuture<'_, Result<bool, BlobStoreError>> {
            Box::pin(async move { Ok(reference.object_id == self.0) })
        }
    }

    #[tokio::test]
    async fn reconciliation_preserves_parent_reference_and_deletes_only_orphan() {
        let root = std::env::temp_dir().join(format!("metric-cleanup-{}", uuid::Uuid::new_v4()));
        let blobs = LocalBlobStore::new(
            &root,
            LocalBlobConfig {
                capacity_bytes: 1024,
                reserve_bytes: 128,
                max_object_bytes: 128,
            },
        )
        .await
        .unwrap();
        let project = ProjectId::new(7).unwrap();
        let event = EventId::from_bytes([1; 16]);
        let retained = BlobObjectId::from_bytes([2; 16]);
        let orphan = BlobObjectId::from_bytes([3; 16]);
        for object in [retained, orphan] {
            let mut writer = blobs
                .begin(
                    BlobKind::EventAttachment,
                    Timestamp::from_unix_millis(1).unwrap(),
                )
                .await
                .unwrap();
            writer.write_chunk(b"blob".as_slice().into()).await.unwrap();
            writer
                .commit(metric_domain::blob::BlobKey::event_owned(
                    project, event, object,
                ))
                .await
                .unwrap();
        }
        let service = BlobCleanupService::new(
            Arc::new(blobs.clone()),
            Arc::new(References(retained)),
            Arc::new(FixedClock(
                Timestamp::from_unix_millis(2_000_000_000_000).unwrap(),
            )),
            BlobCleanupConfig {
                orphan_grace: Duration::from_secs(1),
                interval: Duration::from_secs(60),
                batch_size: 1,
                max_pages_per_run: 4,
            },
        )
        .unwrap();
        assert_eq!(
            service.run_once().await.unwrap(),
            BlobCleanupReport {
                scanned: 2,
                referenced: 1,
                deleted: 1,
            }
        );
        assert!(
            blobs
                .open(&metric_domain::blob::BlobKey::event_owned(
                    project, event, retained
                ))
                .await
                .is_ok()
        );
        assert_eq!(
            blobs
                .open(&metric_domain::blob::BlobKey::event_owned(
                    project, event, orphan
                ))
                .await
                .err(),
            Some(BlobStoreError::NotFound)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
