use super::*;
use crate::{
    IssueCodecConfig, PostgresEventStore, PostgresFinalizationStore, PostgresSessionStore,
    PostgresSignalStore, SessionRetention, SignalRetention,
    test_support::{CommitReplyProxy, finalized},
};
use crate::{
    MetricRetention, MigrationPolicy, PostgresMetricStore, PostgresSchemaManager,
    test_support::{Fixture, seed_projects},
};
use metric_domain::{
    AcceptedEvent, EventId, ScrubbedEventPayload,
    blob::{BlobChecksum, BlobObject},
    finalization::{EnvironmentId, FinalizationPolicy, FinalizeBatch, ReleaseId},
    metrics::{MetricAggregate, MetricDelta, MetricDeltaBatch, MetricKind, MetricSeries},
    sessions::{SessionState, SessionUpdate},
    signals::{
        LogRecord, LogSeverity, SignalBody, SpanId, SpanOperationClass, SpanRecord, TraceId,
    },
};
use metric_ports::{
    BlobStore, Clock, EventStore, FinalizationStore, MetricStore, SessionStore, SignalStore,
};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{sync::Arc, time::Duration};

fn metric(project: i32, time: Timestamp, value: f64) -> MetricDelta {
    MetricDelta {
        series: MetricSeries {
            project_id: ProjectId::new(project).unwrap(),
            name: "'; DROP TABLE metric.projects; -- Юникод".into(),
            kind: MetricKind::Counter,
            unit: "none".into(),
            tags: BTreeMap::new(),
        },
        bucket_start: at(time.unix_millis() - 2 * DAY).unwrap(),
        bucket_width_seconds: 60,
        received_at: time,
        trace_id: None,
        aggregate: MetricAggregate::from_measurement(MetricKind::Counter, value),
    }
}
fn metrics(database: PostgresDatabase) -> PostgresMetricStore {
    PostgresMetricStore::new(
        database,
        MetricRetention {
            days: 1,
            max_series_per_project: 16,
            archive: true,
        },
    )
    .unwrap()
}
async fn add_metric(store: &PostgresMetricStore, value: MetricDelta) {
    let mut batch = MetricDeltaBatch::default();
    batch.push(value);
    store.persist_metrics(batch).await.unwrap();
}
pub(crate) fn claim(kind: ArchiveKind, time: Timestamp) -> ArchiveClaimRequest {
    ArchiveClaimRequest {
        kind,
        now: time,
        maximum_events: 10,
        target_uncompressed_bytes: 1024 * 1024,
    }
}
pub(crate) fn complete(batch: &ArchiveBatch, time: Timestamp) -> ArchiveCompleteRequest {
    ArchiveCompleteRequest {
        segment_id: batch.segment_id,
        object: BlobObject {
            key: batch.object_key.clone(),
            kind: BlobKind::archive(batch.kind),
            size: 123,
            checksum: BlobChecksum::from_bytes([9; 32]),
            created_at: time,
        },
        completed_at: time,
    }
}
pub(crate) fn commit(batch: &ArchiveBatch, time: Timestamp) -> ArchiveSourceCommitRequest {
    ArchiveSourceCommitRequest {
        kind: batch.kind,
        segment_id: batch.segment_id,
        source_ids: batch.source_ids.clone(),
        expire_at: time,
    }
}

pub(crate) async fn seed_all(database: &PostgresDatabase, time: Timestamp) {
    let base = at(time.unix_millis() - 2 * DAY).unwrap();
    let events = PostgresEventStore::new(database.clone(), EventCodecConfig::default()).unwrap();
    let event = AcceptedEvent {
        project_id: ProjectId::new(1).unwrap(),
        event_id: EventId::from_bytes([1; 16]),
        received_at: base,
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            serde_json::to_vec(
                &json!({"event_id":EventId::from_bytes([1;16]).to_string(),"message":"synthetic"}),
            )
            .unwrap(),
        ),
    };
    events
        .insert_batch(&[events.prepare(event).unwrap()])
        .await
        .unwrap();
    PostgresFinalizationStore::new(
        database.clone(),
        EventCodecConfig::default(),
        IssueCodecConfig::default(),
    )
    .unwrap()
    .finalize(
        FinalizeBatch {
            events: vec![finalized(
                1,
                7,
                EventId::from_bytes([1; 16]),
                base.unix_millis(),
                base.unix_millis(),
                Some("backend@1"),
                Some("prod"),
            )],
        },
        FinalizationPolicy {
            event_retention: Duration::from_secs(86400),
            hourly_retention: Duration::from_secs(864000),
            archive_events: true,
            max_implicit_releases_per_project_day: 100,
            max_implicit_environments_per_project: 100,
        },
    )
    .await
    .unwrap();
    let signals = PostgresSignalStore::new(
        database.clone(),
        SignalRetention {
            logs_days: 1,
            spans_days: 1,
            span_stats_hourly_days: 90,
            archive: true,
        },
    )
    .unwrap();
    let sessions = PostgresSessionStore::new(
        database.clone(),
        SessionRetention {
            sessions_days: 1,
            session_stats_hourly_days: 90,
            archive: true,
        },
    )
    .unwrap();
    for p in [1, 2] {
        let project = ProjectId::new(p).unwrap();
        signals
            .persist_logs(vec![LogRecord {
                id: LogId::from_bytes([2; 16]),
                project_id: project,
                received_at: base,
                occurred_at_ns: base.unix_millis() * 1_000_000,
                severity: LogSeverity::Info,
                message: "'; SELECT pg_sleep(999); --".into(),
                trace_id: None,
                span_id: None,
                environment: None,
                release: None,
                service: None,
                body: SignalBody::new(b"{ \"a\": [1, true] }".to_vec()),
            }])
            .await
            .unwrap();
        signals
            .persist_spans(vec![SpanRecord {
                id: SpanRecordId::from_bytes([3; 16]),
                project_id: project,
                received_at: base,
                started_at_ns: base.unix_millis() * 1_000_000,
                duration_ns: 1000,
                trace_id: TraceId::from_bytes([4; 16]),
                span_id: SpanId::from_bytes([5; 8]),
                parent_span_id: None,
                is_segment: true,
                operation_class: SpanOperationClass::HttpServer,
                operation: "http.server".into(),
                status: "ok".into(),
                name: "GET /archive".into(),
                environment: None,
                release: None,
                service: None,
                insight_flags: 0,
                body: SignalBody::new(b"{ \"kind\": \"span\" }".to_vec()),
            }])
            .await
            .unwrap();
        sessions
            .persist_sessions(vec![SessionUpdate {
                id: SessionId::from_bytes([6; 16]),
                project_id: project,
                release_id: ReleaseId::from_bytes([10; 16]),
                environment_id: EnvironmentId::from_bytes([20; 16]),
                started_at: at(base.unix_millis() - 1000).unwrap(),
                updated_at: base,
                state: SessionState::Exited,
                sequence: Some(u64::MAX),
                duration_ms: Some(500),
                user_digest: Some([7; 16]),
            }])
            .await
            .unwrap();
    }
    add_metric(&metrics(database.clone()), metric(1, time, 2.0)).await;
}
struct FixedClock(Timestamp);
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn byte_budget_allows_one_large_record_then_stops_before_the_next() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    let store = metrics(database.clone());
    for prefix in ["first", "second", "third"] {
        let mut value = metric(1, time, 2.0);
        value.series.name = format!("{prefix}{}", "x".repeat(2048)).into();
        add_metric(&store, value).await;
    }
    let mut request = claim(ArchiveKind::Metric, time);
    request.target_uncompressed_bytes = 1024;
    let batch = archive.claim(request).await.unwrap().unwrap();
    let ArchiveRecords::Metrics(records) = &batch.records else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    assert!(records[0].canonical_payload.len() > 1024);
    // Resume uses persisted identities rather than selecting a new budget-sized set.
    assert_eq!(
        archive
            .claim(claim(ArchiveKind::Metric, time))
            .await
            .unwrap()
            .unwrap(),
        batch
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric.metric_buckets WHERE archive_due IS NOT NULL AND expire_at IS NULL")
        .fetch_one(&database.pool).await.unwrap(), 3);
    archive.complete(complete(&batch, time)).await.unwrap();
    assert_eq!(
        archive.commit_sources(commit(&batch, time)).await.unwrap(),
        1
    );
    let next = archive
        .claim(claim(ArchiveKind::Metric, time))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.records.len(), 2);
    assert!(!next.source_ids.contains(&batch.source_ids[0]));
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn bounded_claims_preserve_day_project_and_failed_resume_holds() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    let store = metrics(database.clone());
    for (project, name, received) in [
        (1, "one", time),
        (1, "two", time),
        (1, "next-day", at(time.unix_millis() + DAY).unwrap()),
        (2, "other-project", time),
    ] {
        let mut value = metric(project, time, 2.0);
        value.series.name = name.into();
        value.received_at = received;
        add_metric(&store, value).await;
    }
    sqlx::query("UPDATE metric.metric_buckets SET archive_due=CASE WHEN project_id=2 THEN $1 ELSE received_at-$2 END")
        .bind(time.unix_millis()).bind(DAY).execute(&database.pool).await.unwrap();
    for (maximum, budget) in [
        (0, 1024),
        (MAX_RECORDS + 1, 1024),
        (1, 1023),
        (1, MAX_BYTES + 1),
    ] {
        let mut request = claim(ArchiveKind::Metric, time);
        request.maximum_events = maximum;
        request.target_uncompressed_bytes = budget;
        assert_eq!(
            archive.claim(request).await,
            Err(ArchiveStoreError::InvalidData)
        );
    }
    let mut request = claim(ArchiveKind::Metric, time);
    request.maximum_events = 1;
    let batch = archive.claim(request).await.unwrap().unwrap();
    assert_eq!(batch.project_id.get(), 1);
    assert_eq!(batch.records.len(), 1);
    // A persisted claim never quietly drops an unreadable source or releases its hold.
    let id = local_id(batch.source_ids[0]);
    sqlx::query("UPDATE metric.metric_buckets SET tags=$1 WHERE project_id=1 AND bucket_id=$2")
        .bind(b"not JSON".to_vec())
        .bind(&id)
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        archive.claim(claim(ArchiveKind::Metric, time)).await,
        Err(ArchiveStoreError::InvalidData)
    );
    let held: bool = sqlx::query_scalar("SELECT archive_due IS NOT NULL AND expire_at IS NULL FROM metric.metric_buckets WHERE project_id=1 AND bucket_id=$1")
        .bind(&id).fetch_one(&database.pool).await.unwrap();
    assert!(held);
    sqlx::query("UPDATE metric.metric_buckets SET tags=$1 WHERE project_id=1 AND bucket_id=$2")
        .bind(b"{}".to_vec())
        .bind(&id)
        .execute(&database.pool)
        .await
        .unwrap();
    let resumed = archive
        .claim(claim(ArchiveKind::Metric, time))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed, batch);
    archive.complete(complete(&batch, time)).await.unwrap();
    archive.commit_sources(commit(&batch, time)).await.unwrap();
    let mut request = claim(ArchiveKind::Metric, time);
    request.target_uncompressed_bytes = 1024;
    let next = archive.claim(request).await.unwrap().unwrap();
    assert_eq!(next.project_id.get(), 1);
    assert_eq!(next.records.len(), 1);
    assert!(next.received_to.unix_millis() < time.unix_millis().div_euclid(DAY) * DAY + DAY);
    // Schema guards reject malformed identity arrays before any publication.
    assert!(
        sqlx::query("UPDATE metric.archive_manifests SET source_ids=$1 WHERE segment_id=$2")
            .bind(vec![
                local_id(next.source_ids[0]),
                local_id(next.source_ids[0])
            ])
            .bind(next.segment_id.as_bytes().as_slice())
            .execute(&database.pool)
            .await
            .is_err()
    );
    let original_key = next.object_key.clone();
    sqlx::query("UPDATE metric.archive_manifests SET object_key=$1 WHERE segment_id=$2")
        .bind("archive/wrong-project/valid-text.parquet")
        .bind(next.segment_id.as_bytes().as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        archive.claim(claim(ArchiveKind::Metric, time)).await,
        Err(ArchiveStoreError::InvalidData)
    );
    assert_eq!(
        archive.commit_sources(commit(&next, time)).await,
        Err(ArchiveStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.archive_manifests SET object_key=$1 WHERE segment_id=$2")
        .bind(original_key.as_str())
        .bind(next.segment_id.as_bytes().as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    archive.complete(complete(&next, time)).await.unwrap();
    archive.commit_sources(commit(&next, time)).await.unwrap();
    let next_day = archive
        .claim(claim(ArchiveKind::Metric, time))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next_day.project_id.get(), 1);
    assert_eq!(next_day.records.len(), 1);
    assert_eq!(
        next_day.received_from,
        at(time.unix_millis() + DAY).unwrap()
    );
    archive.complete(complete(&next_day, time)).await.unwrap();
    archive
        .commit_sources(commit(&next_day, time))
        .await
        .unwrap();
    let other = archive
        .claim(claim(ArchiveKind::Metric, time))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.project_id.get(), 2);
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn pending_events_and_oversized_source_bodies_cannot_release_holds() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    seed_all(&database, time).await;
    let original: Vec<u8> =
        sqlx::query_scalar("SELECT body FROM metric.error_events WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE metric.error_events SET processing_state=0,next_attempt_at=$1 WHERE project_id=1",
    )
    .bind(time.unix_millis())
    .execute(&database.pool)
    .await
    .unwrap();
    assert!(
        archive
            .claim(claim(ArchiveKind::Event, time))
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE metric.error_events SET processing_state=1,next_attempt_at=NULL,body=$1 WHERE project_id=1")
        .bind(vec![0u8; 9 * 1024 * 1024]).execute(&database.pool).await.unwrap();
    assert_eq!(
        archive.claim(claim(ArchiveKind::Event, time)).await,
        Err(ArchiveStoreError::InvalidData)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric.archive_manifests")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        0
    );
    assert!(sqlx::query_scalar::<_, bool>("SELECT archive_due IS NOT NULL AND expire_at IS NULL FROM metric.error_events WHERE project_id=1")
        .fetch_one(&database.pool).await.unwrap());
    sqlx::query("UPDATE metric.error_events SET body=$1 WHERE project_id=1")
        .bind(original)
        .execute(&database.pool)
        .await
        .unwrap();
    let failed = archive
        .claim(claim(ArchiveKind::Event, time))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.records.len(), 1);
    let wrong_project_id = EventKey::new(ProjectId::new(2).unwrap(), EventId::from_bytes([1; 16]));
    assert!(
        sqlx::query("UPDATE metric.archive_manifests SET source_ids=$1 WHERE segment_id=$2")
            .bind(vec![wrong_project_id.as_bytes().to_vec()])
            .bind(failed.segment_id.as_bytes().as_slice())
            .execute(&database.pool)
            .await
            .is_err()
    );
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn archive_native_plans_use_due_day_and_manifest_indexes() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    seed_all(&database, time).await;
    // Representative synthetic held records, using the normal seed's valid codec.
    sqlx::query("INSERT INTO metric.logs(project_id,log_id,received_at,occurred_at_ns,severity,message,message_folded,body,body_size,archive_due) SELECT 2,pg_catalog.int8send(n)||pg_catalog.int8send(n),l.received_at+n*60000,l.occurred_at_ns,l.severity,l.message,l.message_folded,l.body,l.body_size,l.archive_due+n FROM metric.logs l CROSS JOIN generate_series(1::bigint,4096::bigint) n WHERE l.project_id=2")
        .execute(&database.pool).await.unwrap();
    sqlx::raw_sql("ANALYZE metric.logs; ANALYZE metric.projects")
        .execute(&database.pool)
        .await
        .unwrap();
    let due: serde_json::Value = sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) SELECT project_id,received_at FROM metric.logs WHERE archive_due<=$1 AND archive_segment IS NULL AND project_id IN (SELECT id FROM metric.projects WHERE state IN (0,1)) ORDER BY archive_due,project_id,log_id LIMIT 1")
        .bind(time.unix_millis()).fetch_one(&database.pool).await.unwrap();
    assert!(due.to_string().contains("log_archive_due"), "{due}");
    let start = (time.unix_millis() - 2 * DAY).div_euclid(DAY) * DAY;
    let day: serde_json::Value = sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) SELECT log_id FROM metric.logs WHERE project_id=1 AND archive_due IS NOT NULL AND archive_segment IS NULL AND received_at>=$1 AND received_at<$2 AND archive_due<=$3 ORDER BY received_at,log_id LIMIT 10")
        .bind(start).bind(start+DAY).bind(time.unix_millis()).fetch_one(&database.pool).await.unwrap();
    assert!(day.to_string().contains("log_archive_project_day"), "{day}");
    let batch = archive
        .claim(claim(ArchiveKind::Log, time))
        .await
        .unwrap()
        .unwrap();
    let manifest: serde_json::Value = sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) SELECT object_key FROM metric.archive_manifests WHERE segment_id=$1")
        .bind(batch.segment_id.as_bytes().as_slice()).fetch_one(&database.pool).await.unwrap();
    assert!(
        manifest.to_string().contains("archive_manifests_pkey"),
        "{manifest}"
    );
    println!("archive native plans: due={due}; day={day}; manifest={manifest}");
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn all_families_publish_real_parquet_and_preserve_other_project_ids() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    seed_all(&database, time).await;
    let mut expected = Vec::new();
    for k in ArchiveKind::ALL {
        let batch = archive.claim(claim(k, time)).await.unwrap().unwrap();
        assert_eq!(batch.project_id.get(), 1);
        assert_eq!(batch.records.len(), 1);
        let bytes = match &batch.records {
            ArchiveRecords::Events(v) => metric_application::archive::encode_parquet(v).unwrap(),
            ArchiveRecords::Logs(v)
            | ArchiveRecords::Spans(v)
            | ArchiveRecords::Sessions(v)
            | ArchiveRecords::Metrics(v) => {
                metric_application::archive::encode_signal_parquet(k, v).unwrap()
            }
        };
        expected.push((batch.object_key, bytes));
    }
    let parent = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("metric-pg-archive-tests");
    std::fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let root = parent.join(uuid::Uuid::new_v4().to_string());
    let blobs = Arc::new(
        metric_blob::LocalBlobStore::new(
            &root,
            metric_blob::LocalBlobConfig {
                capacity_bytes: 32 * 1024 * 1024,
                reserve_bytes: 1024 * 1024,
                max_object_bytes: 16 * 1024 * 1024,
            },
        )
        .await
        .unwrap(),
    );
    let service = metric_application::archive::ArchiveService::new(
        Arc::new(archive.clone()),
        blobs.clone(),
        Arc::new(FixedClock(time)),
        metric_application::archive::ArchiveConfig {
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
            write_chunk_bytes: 4096,
            hot_copy_delay: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .unwrap();
    let report = service.run_once().await.unwrap();
    assert_eq!(report.claimed_records, 5);
    assert_eq!(report.archived_records, 5);
    for (key, expected) in expected {
        let mut reader = blobs.open(&key).await.unwrap();
        let mut bytes = Vec::new();
        while let Some(chunk) = reader.read_chunk(4096).await.unwrap() {
            bytes.extend_from_slice(&chunk);
        }
        assert_eq!(bytes, expected);
        assert!(archive.object_referenced(&key).await.unwrap());
    }
    for k in ArchiveKind::ALL {
        let spec = source(k);
        let mut q = QueryBuilder::<Postgres>::new(
            "SELECT project_id,archive_due,expire_at,archive_segment FROM ",
        );
        q.push(spec.table).push(" ORDER BY project_id");
        let rows = q.build().fetch_all(&database.pool).await.unwrap();
        assert!(rows[0].get::<Option<i64>, _>("archive_due").is_none());
        assert_eq!(
            rows[0].get::<i64, _>("expire_at"),
            time.unix_millis() + 60_000
        );
        assert_eq!(rows[0].get::<Vec<u8>, _>("archive_segment").len(), 16);
        if rows.len() == 2 {
            assert_eq!(rows[1].get::<i32, _>("project_id"), 2);
            assert!(rows[1].get::<Option<i64>, _>("archive_due").is_some());
            assert!(rows[1].get::<Option<i64>, _>("expire_at").is_none());
        }
    }
    let resolved = root.canonicalize().unwrap();
    assert_eq!(resolved.parent(), Some(parent.as_path()));
    std::fs::remove_dir_all(resolved).unwrap();
    drop(service);
    drop(blobs);
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn concurrent_claims_and_commit_retries_have_one_native_transition() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    add_metric(&metrics(database.clone()), metric(1, time, 2.0)).await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let archive = archive.clone();
        tasks.push(tokio::spawn(async move {
            archive
                .claim(claim(ArchiveKind::Metric, time))
                .await
                .unwrap()
                .unwrap()
        }));
    }
    let batch = tasks.remove(0).await.unwrap();
    for task in tasks {
        assert_eq!(task.await.unwrap(), batch);
    }
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let archive = archive.clone();
        let request = complete(&batch, time);
        tasks.push(tokio::spawn(async move {
            archive.complete(request).await.unwrap()
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let archive = archive.clone();
        let request = commit(&batch, time);
        tasks.push(tokio::spawn(async move {
            archive.commit_sources(request).await.unwrap()
        }));
    }
    let mut total = 0;
    for task in tasks {
        total += task.await.unwrap();
    }
    assert_eq!(total, 1);
    assert!(
        archive
            .claim(claim(ArchiveKind::Metric, time))
            .await
            .unwrap()
            .is_none()
    );
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn completed_manifest_and_source_conflicts_do_not_assign_expiry() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    let value = metric(1, time, 2.0);
    add_metric(&metrics(database.clone()), value.clone()).await;
    let batch = archive
        .claim(claim(ArchiveKind::Metric, time))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        archive.commit_sources(commit(&batch, time)).await,
        Err(ArchiveStoreError::InvalidData)
    );
    let mut wrong = complete(&batch, time);
    wrong.object.kind = BlobKind::LogArchive;
    assert_eq!(
        archive.complete(wrong).await,
        Err(ArchiveStoreError::InvalidData)
    );
    archive.complete(complete(&batch, time)).await.unwrap();
    let mut wrong = commit(&batch, time);
    wrong.source_ids = vec![ArchiveSourceId::Metric([255; 16])];
    assert_eq!(
        archive.commit_sources(wrong).await,
        Err(ArchiveStoreError::Conflict)
    );
    sqlx::query("UPDATE metric.metric_buckets SET archive_segment=$1 WHERE project_id=1")
        .bind(vec![255u8; 16])
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        archive.commit_sources(commit(&batch, time)).await,
        Err(ArchiveStoreError::Conflict)
    );
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT source_committed FROM metric.archive_manifests")
            .fetch_one(&database.pool)
            .await
            .unwrap()
    );
    assert!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT expire_at FROM metric.metric_buckets")
            .fetch_one(&database.pool)
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE metric.metric_buckets SET archive_segment=NULL WHERE project_id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        archive.commit_sources(commit(&batch, time)).await.unwrap(),
        1
    );
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn accepted_late_metric_values_retain_documented_expiry_window() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    let store = metrics(database.clone());
    add_metric(&store, metric(1, time, 2.0)).await;
    let batch = archive
        .claim(claim(ArchiveKind::Metric, time))
        .await
        .unwrap()
        .unwrap();
    let ArchiveRecords::Metrics(v) = &batch.records else {
        panic!()
    };
    let payload: serde_json::Value = serde_json::from_slice(&v[0].canonical_payload).unwrap();
    assert_eq!(payload["sum"], 2.0);
    archive.complete(complete(&batch, time)).await.unwrap();
    add_metric(&store, metric(1, time, 3.0)).await;
    assert_eq!(
        archive.commit_sources(commit(&batch, time)).await.unwrap(),
        1
    );
    add_metric(&store, metric(1, time, 4.0)).await;
    let row = sqlx::query(
        "SELECT aggregate_sum,measurement_count,expire_at,archive_due FROM metric.metric_buckets",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<f64, _>("aggregate_sum"), 9.0);
    assert_eq!(row.get::<i64, _>("measurement_count"), 3);
    assert_eq!(row.get::<i64, _>("expire_at"), time.unix_millis());
    assert!(row.get::<Option<i64>, _>("archive_due").is_none());
    assert!(
        archive
            .claim(claim(ArchiveKind::Metric, time))
            .await
            .unwrap()
            .is_none()
    );
    // Actual bounded maintenance is the next pending capability, not emulated here.
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn ambiguous_commit_replies_resume_manifest_completion_and_source_commit() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    add_metric(&metrics(database.clone()), metric(1, time, 2.0)).await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let wire = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let uncertain = PostgresArchiveStore::new(wire.clone(), EventCodecConfig::default()).unwrap();
    proxy.drop_next_commit_reply();
    assert_eq!(
        uncertain.claim(claim(ArchiveKind::Metric, time)).await,
        Err(ArchiveStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    let batch = archive
        .claim(claim(ArchiveKind::Metric, time))
        .await
        .unwrap()
        .unwrap();
    proxy.drop_next_commit_reply();
    assert_eq!(
        uncertain.complete(complete(&batch, time)).await,
        Err(ArchiveStoreError::Unavailable)
    );
    archive.complete(complete(&batch, time)).await.unwrap();
    proxy.drop_next_commit_reply();
    assert_eq!(
        uncertain.commit_sources(commit(&batch, time)).await,
        Err(ArchiveStoreError::Unavailable)
    );
    assert_eq!(
        archive.commit_sources(commit(&batch, time)).await.unwrap(),
        0
    );
    wire.pool.close().await;
    proxy.close().await;
    database.pool.close().await;
    fixture.cleanup().await;
}

pub(crate) fn now() -> Timestamp {
    Timestamp::from_unix_millis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
    )
    .unwrap()
}
pub(crate) async fn setup() -> (Fixture, PostgresDatabase, PostgresArchiveStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let archive = PostgresArchiveStore::new(database.clone(), EventCodecConfig::default()).unwrap();
    (fixture, database, archive)
}
#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn metric_lifecycle_uses_shared_public_port_contract() {
    let (fixture, database, archive) = setup().await;
    let metrics = PostgresMetricStore::new(
        database.clone(),
        MetricRetention {
            days: 1,
            max_series_per_project: 16,
            archive: true,
        },
    )
    .unwrap();
    let time = now();
    metric_testkit::archive_conformance::metric_archive_lifecycle(
        &metrics,
        &archive,
        ProjectId::new(1).unwrap(),
        time,
        at(time.unix_millis() + DAY).unwrap(),
    )
    .await;
    let row=sqlx::query("SELECT archive_due,expire_at,archive_segment,aggregate_sum,measurement_count FROM metric.metric_buckets WHERE project_id=1").fetch_one(&database.pool).await.unwrap();
    assert!(row.get::<Option<i64>, _>("archive_due").is_none());
    assert_eq!(row.get::<i64, _>("expire_at"), time.unix_millis() + DAY);
    assert_eq!(row.get::<Vec<u8>, _>("archive_segment").len(), 16);
    assert_eq!(row.get::<f64, _>("aggregate_sum"), 2.0);
    database.pool.close().await;
    fixture.cleanup().await;
}
