//! Real-adapter Metrics archive lifecycle, independent of storage representation.
use metric_domain::{
    ProjectId, Timestamp,
    archive::{ArchiveBatch, ArchiveBatchState, ArchiveKind, ArchiveRecords, ArchiveSourceId},
    blob::{BlobChecksum, BlobKind, BlobObject},
    metrics::{MetricAggregate, MetricDelta, MetricDeltaBatch, MetricKind, MetricSeries},
};
use metric_ports::{
    ArchiveClaimRequest, ArchiveCompleteRequest, ArchiveSourceCommitRequest, ArchiveStore,
    ArchiveStoreError, MetricStore,
};
use std::collections::BTreeMap;

/// Caller supplies archive retention with a one-day hot period and an isolated
/// database. This tests metadata integrity; server E2E verifies real Blob bytes.
pub async fn metric_archive_lifecycle(
    metrics: &dyn MetricStore,
    archives: &dyn ArchiveStore,
    project: ProjectId,
    now: Timestamp,
    expire_at: Timestamp,
) -> ArchiveBatch {
    let delta = MetricDelta {
        series: MetricSeries {
            project_id: project,
            name: "synthetic.archive.counter".into(),
            kind: MetricKind::Counter,
            unit: "none".into(),
            tags: BTreeMap::new(),
        },
        bucket_start: Timestamp::from_unix_millis(now.unix_millis() - 2 * 86_400_000).unwrap(),
        bucket_width_seconds: 60,
        received_at: now,
        trace_id: None,
        aggregate: MetricAggregate::from_measurement(MetricKind::Counter, 2.0),
    };
    let id = delta.bucket_id();
    let mut batch = MetricDeltaBatch::default();
    batch.push(delta);
    metrics.persist_metrics(batch).await.unwrap();
    let request = ArchiveClaimRequest {
        kind: ArchiveKind::Metric,
        now,
        maximum_events: 10,
        target_uncompressed_bytes: 1024 * 1024,
    };
    let claimed = archives.claim(request).await.unwrap().unwrap();
    assert_eq!(claimed.kind, ArchiveKind::Metric);
    assert_eq!(claimed.project_id, project);
    assert_eq!(claimed.state, ArchiveBatchState::Writing);
    assert_eq!(claimed.source_ids, vec![ArchiveSourceId::Metric(id)]);
    let ArchiveRecords::Metrics(records) = &claimed.records else {
        panic!("wrong archive records");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, id);
    assert_eq!(records[0].project_id, project);
    assert!(
        serde_json::from_slice::<serde_json::Value>(&records[0].canonical_payload)
            .unwrap()
            .is_object()
    );
    assert!(
        archives
            .object_referenced(&claimed.object_key)
            .await
            .unwrap()
    );
    let commit = ArchiveSourceCommitRequest {
        kind: ArchiveKind::Metric,
        segment_id: claimed.segment_id,
        source_ids: claimed.source_ids.clone(),
        expire_at,
    };
    assert_eq!(
        archives.commit_sources(commit.clone()).await,
        Err(ArchiveStoreError::InvalidData)
    );
    assert_eq!(
        archives.claim(request).await.unwrap(),
        Some(claimed.clone())
    );
    let complete = ArchiveCompleteRequest {
        segment_id: claimed.segment_id,
        object: BlobObject {
            key: claimed.object_key.clone(),
            kind: BlobKind::MetricArchive,
            size: 123,
            checksum: BlobChecksum::from_bytes([9; 32]),
            created_at: now,
        },
        completed_at: now,
    };
    archives.complete(complete.clone()).await.unwrap();
    archives.complete(complete.clone()).await.unwrap();
    let mut invalid = complete;
    invalid.object.checksum = BlobChecksum::from_bytes([8; 32]);
    assert_eq!(
        archives.complete(invalid).await,
        Err(ArchiveStoreError::Conflict)
    );
    let resumed = archives.claim(request).await.unwrap().unwrap();
    assert_eq!(resumed.state, ArchiveBatchState::Complete);
    assert!(resumed.records.is_empty());
    assert_eq!(resumed.source_ids, claimed.source_ids);
    assert_eq!(resumed.object_key, claimed.object_key);
    let mut invalid = commit.clone();
    invalid.source_ids = vec![ArchiveSourceId::Metric([255; 16])];
    assert_eq!(
        archives.commit_sources(invalid).await,
        Err(ArchiveStoreError::Conflict)
    );
    assert_eq!(archives.commit_sources(commit.clone()).await.unwrap(), 1);
    assert_eq!(archives.commit_sources(commit).await.unwrap(), 0);
    assert!(archives.claim(request).await.unwrap().is_none());
    assert!(
        archives
            .object_referenced(&claimed.object_key)
            .await
            .unwrap()
    );
    claimed
}
