use super::*;
use crate::{
    EventCodecConfig, IssueCodecConfig, PostgresEventStore, PostgresFinalizationStore,
    archive::tests::{claim, commit, complete, now, seed_all, setup},
    test_support::{CommitReplyProxy, finalized},
};
use metric_domain::{
    AcceptedEvent, EventId, ProjectId, ScrubbedEventPayload,
    archive::ArchiveKind,
    finalization::{FinalizationPolicy, FinalizeBatch},
};
use metric_ports::{ArchiveStore, EventStore, FinalizationStore};
use std::sync::Arc;

const DAY: i64 = 86_400_000;

#[tokio::test]
#[ignore = "requires real PostgreSQL; bounded retention diagnostic, not a soak test"]
async fn populated_retention_cycle_allows_queries_and_restarts() {
    let (fixture, database, _) = setup().await;
    let rows: i64 = std::env::var("METRIC_RETENTION_PROBE_ROWS")
        .ok()
        .map(|value| value.parse().unwrap())
        .unwrap_or(10_000);
    assert!((1_000..=1_000_000).contains(&rows));
    let time = now();
    for first in (1..=rows).step_by(10_000) {
        sqlx::query("INSERT INTO metric.error_events(project_id,event_id,received_at,policy_revision,body,processing_state) SELECT 1,pg_catalog.int8send(n)||pg_catalog.int8send(n),$1,1,'{}'::bytea,1 FROM generate_series($2::bigint,$3::bigint) n")
            .bind(time.unix_millis() - 2 * DAY).bind(first).bind((first + 9_999).min(rows))
            .execute(&database.pool).await.unwrap();
    }
    let mut store = PostgresMaintenanceStore::new(database.clone());
    let mut req = request(MaintenanceTask::EventRetention, time, 100);
    req.archive_events = false;
    let started = Instant::now();
    let mut batches = 0;
    let mut maximum_probe_ms = 0;
    loop {
        let probe = async {
            let start = Instant::now();
            // A real concurrent write, to an unrelated row, rather than SELECT 1.
            sqlx::query("UPDATE metric.projects SET policy_revision=policy_revision+1 WHERE id=2")
                .execute(&database.pool)
                .await
                .unwrap();
            start.elapsed().as_millis()
        };
        let (result, probe_ms) = tokio::join!(store.run(req.clone()), probe);
        let result = result.unwrap();
        assert!(result.scanned <= 100 && result.changed <= 100);
        maximum_probe_ms = maximum_probe_ms.max(probe_ms);
        batches += 1;
        req.cursor = result.next_cursor;
        if batches == 10 {
            // Dropping the adapter and its cursor repeats the scan safely.
            store = PostgresMaintenanceStore::new(database.clone());
            req.cursor = None;
            continue;
        }
        if req.cursor.is_none() {
            break;
        }
        assert!(batches < 25_000);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric.error_events")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        0
    );
    println!(
        "retention_probe rows={rows} batch=100 calls={batches} elapsed_ms={} max_concurrent_write_ms={maximum_probe_ms}; polling sleeps omitted",
        started.elapsed().as_millis()
    );
    database.pool.close().await;
    fixture.cleanup().await;
}

fn table(kind: ArchiveKind) -> &'static str {
    match kind {
        ArchiveKind::Event => "metric.error_events",
        ArchiveKind::Log => "metric.logs",
        ArchiveKind::Span => "metric.spans",
        ArchiveKind::Session => "metric.sessions",
        ArchiveKind::Metric => "metric.metric_buckets",
    }
}
async fn source_count(
    database: &PostgresDatabase,
    kind: ArchiveKind,
    project: i32,
    held: bool,
) -> i64 {
    let mut q = QueryBuilder::<Postgres>::new("SELECT count(*) FROM ");
    q.push(table(kind))
        .push(" WHERE project_id=")
        .push_bind(project);
    if held {
        q.push(" AND archive_due IS NOT NULL AND expire_at IS NULL");
    }
    q.build_query_scalar()
        .fetch_one(&database.pool)
        .await
        .unwrap()
}
fn at(ms: i64) -> Timestamp {
    Timestamp::from_unix_millis(ms).unwrap()
}
fn request(task: MaintenanceTask, time: Timestamp, batch_size: usize) -> MaintenanceRequest {
    MaintenanceRequest {
        task,
        now: time,
        batch_size,
        cursor: None,
        event_retention: Duration::from_secs(86400),
        hourly_retention: Duration::from_secs(86400),
        archive_events: true,
    }
}
async fn pass(
    store: &PostgresMaintenanceStore,
    mut request: MaintenanceRequest,
) -> Vec<MaintenanceResult> {
    let mut results = Vec::new();
    for _ in 0..100 {
        let result = store.run(request.clone()).await.unwrap();
        assert!(result.scanned <= request.batch_size);
        assert!(result.changed <= request.batch_size);
        request.cursor = result.next_cursor.clone();
        results.push(result);
        if request.cursor.is_none() {
            return results;
        }
    }
    panic!("bounded fixture pass did not finish");
}
async fn event(
    database: &PostgresDatabase,
    seed: u8,
    time: Timestamp,
    environment: Option<&str>,
) -> EventKey {
    let store = PostgresEventStore::new(database.clone(), EventCodecConfig::default()).unwrap();
    let id = EventId::from_bytes([seed; 16]);
    let key = EventKey::new(ProjectId::new(1).unwrap(), id);
    let accepted = AcceptedEvent {
        event_id: id,
        project_id: key.project_id(),
        received_at: time,
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            serde_json::to_vec(&serde_json::json!({ "event_id": id.to_string() })).unwrap(),
        ),
    };
    store
        .insert_batch(&[store.prepare(accepted).unwrap()])
        .await
        .unwrap();
    if let Some(environment) = environment {
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
                    id,
                    time.unix_millis(),
                    time.unix_millis(),
                    Some("backend@1"),
                    Some(environment),
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
    }
    key
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn native_expiry_removes_only_committed_hot_sources_after_their_delay() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    seed_all(&database, time).await;
    let store = PostgresMaintenanceStore::new(database.clone());
    pass(&store, request(MaintenanceTask::EventRetention, time, 2)).await;
    let expiry = at(time.unix_millis() + 60_000);
    let mut batches = Vec::new();
    for kind in ArchiveKind::ALL {
        let batch = archive.claim(claim(kind, time)).await.unwrap().unwrap();
        archive.complete(complete(&batch, time)).await.unwrap();
        assert_eq!(
            archive
                .commit_sources(commit(&batch, expiry))
                .await
                .unwrap(),
            1
        );
        batches.push(batch);
    }
    pass(&store, request(MaintenanceTask::EventRetention, time, 2)).await;
    for kind in ArchiveKind::ALL {
        let count = source_count(&database, kind, 1, false).await;
        assert_eq!(count, 1);
    }
    pass(&store, request(MaintenanceTask::EventRetention, expiry, 2)).await;
    for kind in ArchiveKind::ALL {
        let count = source_count(&database, kind, 1, false).await;
        assert_eq!(count, 0);
        if matches!(
            kind,
            ArchiveKind::Log | ArchiveKind::Span | ArchiveKind::Session
        ) {
            let count = source_count(&database, kind, 2, true).await;
            assert_eq!(count, 1);
        }
    }
    for batch in batches {
        assert!(archive.object_referenced(&batch.object_key).await.unwrap());
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM metric.archive_manifests WHERE source_committed"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap(),
        5
    );
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn retention_protects_pending_and_failed_archive_holds_and_repairs_hourly_policy() {
    let (fixture, database, _) = setup().await;
    let time = now();
    seed_all(&database, time).await;
    let pending = event(&database, 2, at(time.unix_millis() - 2 * DAY), None).await;
    sqlx::query("UPDATE metric.error_events SET processing_state=1,next_attempt_at=NULL,archive_due=NULL,expire_at=NULL WHERE event_id=$1")
        .bind(vec![1u8;16]).execute(&database.pool).await.unwrap();
    let store = PostgresMaintenanceStore::new(database.clone());
    pass(&store, request(MaintenanceTask::EventRetention, time, 1)).await;
    assert!(sqlx::query_scalar::<_, bool>("SELECT archive_due IS NOT NULL AND expire_at IS NULL FROM metric.error_events WHERE event_id=$1")
        .bind(vec![1u8;16]).fetch_one(&database.pool).await.unwrap());
    let mut req = request(MaintenanceTask::EventRetention, time, 1);
    req.archive_events = false;
    pass(&store, req).await;
    let keys: Vec<Vec<u8>> = sqlx::query_scalar("SELECT event_key FROM metric.error_events")
        .fetch_all(&database.pool)
        .await
        .unwrap();
    assert_eq!(keys, vec![pending.as_bytes().to_vec()]);
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT expire_at IS NULL AND archive_due IS NULL FROM metric.error_events"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap()
    );
    pass(&store, request(MaintenanceTask::HourlyRetention, time, 1)).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric.issue_stats_hourly")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        0
    );
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn counters_are_monotonic_and_catalog_counts_are_exact_above_page_size() {
    let (fixture, database, _) = setup().await;
    let time = now();
    seed_all(&database, time).await;
    for seed in 2..=5 {
        event(
            &database,
            seed,
            at(time.unix_millis() - 2 * DAY),
            Some(&format!("env-{seed}")),
        )
        .await;
    }
    sqlx::query("UPDATE metric.issues SET occurrence_count=1 WHERE project_id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE metric.project_catalog_usage SET environment_count=0 WHERE project_id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    let store = PostgresMaintenanceStore::new(database.clone());
    pass(
        &store,
        request(MaintenanceTask::CounterReconciliation, time, 1),
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT environment_count FROM metric.project_catalog_usage WHERE project_id=1"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap(),
        5
    );
    pass(
        &store,
        request(MaintenanceTask::CounterReconciliation, time, 10),
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT occurrence_count FROM metric.issues WHERE project_id=1"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap(),
        5
    );
    let mut req = request(MaintenanceTask::EventRetention, time, 2);
    req.archive_events = false;
    pass(&store, req).await;
    pass(
        &store,
        request(MaintenanceTask::CounterReconciliation, time, 10),
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT occurrence_count FROM metric.issues WHERE project_id=1"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap(),
        5
    );
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn concurrent_expiry_and_lost_commit_reply_keep_holds_and_retry_safely() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    seed_all(&database, time).await;
    let batch = archive
        .claim(claim(ArchiveKind::Log, time))
        .await
        .unwrap()
        .unwrap();
    archive.complete(complete(&batch, time)).await.unwrap();
    archive.commit_sources(commit(&batch, time)).await.unwrap();
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let wire = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let uncertain = PostgresMaintenanceStore::new(wire.clone());
    let mut req = request(MaintenanceTask::EventRetention, time, 1);
    req.cursor = Some(make_cursor(EVENT_EXPIRY + 1, &[]).unwrap());
    proxy.drop_next_commit_reply();
    assert_eq!(
        uncertain.run(req.clone()).await,
        Err(MaintenanceStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    let store = PostgresMaintenanceStore::new(database.clone());
    let (one, two) = tokio::join!(store.run(req.clone()), store.run(req));
    assert_eq!(one.unwrap().changed + two.unwrap().changed, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric.logs WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        0
    );
    assert!(sqlx::query_scalar::<_, bool>("SELECT archive_due IS NOT NULL AND expire_at IS NULL FROM metric.logs WHERE project_id=2")
        .fetch_one(&database.pool).await.unwrap());
    wire.pool.close().await;
    proxy.close().await;
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn invalid_limits_cursors_pending_corruption_and_overflow_leave_no_changes() {
    let (fixture, database, _) = setup().await;
    let time = now();
    let store = PostgresMaintenanceStore::new(database.clone());
    for size in [0, 10_001] {
        assert_eq!(
            store
                .run(request(MaintenanceTask::EventRetention, time, size))
                .await,
            Err(MaintenanceStoreError::InvalidData)
        );
    }
    for (task, bytes) in [
        (MaintenanceTask::EventRetention, vec![HOURLY]),
        (MaintenanceTask::CounterReconciliation, vec![ISSUES, 1]),
        (MaintenanceTask::HourlyRetention, vec![HOURLY, 1]),
        (MaintenanceTask::EventRetention, vec![EVENT_EXPIRY, 1]),
    ] {
        let mut req = request(task, time, 1);
        req.cursor = MaintenanceCursor::new(bytes);
        assert_eq!(
            store.run(req).await,
            Err(MaintenanceStoreError::InvalidData)
        );
    }
    let key = event(&database, 2, time, None).await;
    sqlx::query("UPDATE metric.error_events SET expire_at=$1 WHERE event_key=$2")
        .bind(time.unix_millis() - 1)
        .bind(key.as_bytes().as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .run(request(MaintenanceTask::EventRetention, time, 1))
            .await,
        Err(MaintenanceStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.error_events SET expire_at=NULL,processing_state=1,next_attempt_at=NULL,received_at=253402300799999 WHERE event_key=$1")
        .bind(key.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
    assert_eq!(
        store
            .run(request(MaintenanceTask::EventRetention, time, 1))
            .await,
        Err(MaintenanceStoreError::InvalidData)
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT expire_at IS NULL AND archive_due IS NULL FROM metric.error_events"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap()
    );
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn native_reference_cleanup_rolls_back_and_existing_scheduler_reuses_the_port() {
    let (fixture, database, archive) = setup().await;
    let time = now();
    seed_all(&database, time).await;
    let batch = archive
        .claim(claim(ArchiveKind::Event, time))
        .await
        .unwrap()
        .unwrap();
    archive.complete(complete(&batch, time)).await.unwrap();
    archive.commit_sources(commit(&batch, time)).await.unwrap();
    sqlx::query("INSERT INTO metric.blob_references(project_id,event_id,object_id,source_kind,blob_key) VALUES(1,$1,$2,0,$3)")
        .bind(vec![1u8;16]).bind(vec![77u8;16]).bind("synthetic/expired-event-reference").execute(&database.pool).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION metric.reject_reference_cleanup() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic rollback'; END $$; CREATE TRIGGER reject_reference_cleanup BEFORE DELETE ON metric.blob_references FOR EACH ROW EXECUTE FUNCTION metric.reject_reference_cleanup()")
        .execute(&database.pool).await.unwrap();
    let store = PostgresMaintenanceStore::new(database.clone());
    let mut req = request(MaintenanceTask::EventRetention, time, 1);
    req.cursor = Some(make_cursor(EVENT_EXPIRY, &[]).unwrap());
    assert_eq!(
        store.run(req.clone()).await,
        Err(MaintenanceStoreError::Unavailable)
    );
    assert_eq!(
        source_count(&database, ArchiveKind::Event, 1, false).await,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric.blob_references")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        1
    );
    sqlx::raw_sql("DROP TRIGGER reject_reference_cleanup ON metric.blob_references; DROP FUNCTION metric.reject_reference_cleanup()")
        .execute(&database.pool).await.unwrap();
    assert_eq!(store.run(req).await.unwrap().changed, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric.blob_references")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        0
    );
    struct Clock(Timestamp);
    impl metric_ports::Clock for Clock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }
    for task in [
        MaintenanceTask::RetryBacklog,
        MaintenanceTask::MonitorTimeouts,
        MaintenanceTask::MonitorMissed,
        MaintenanceTask::UploadExpiry,
        MaintenanceTask::BlobOrphanRegistration,
    ] {
        let result = store.run(request(task, time, 1)).await.unwrap();
        assert_eq!(
            result.disposition,
            if matches!(
                task,
                MaintenanceTask::UploadExpiry | MaintenanceTask::BlobOrphanRegistration
            ) {
                MaintenanceDisposition::Disabled
            } else {
                MaintenanceDisposition::Completed
            }
        );
    }
    let scheduler = metric_application::scheduler::Scheduler::new(
        Arc::new(store),
        Arc::new(Clock(time)),
        metric_application::scheduler::SchedulerConfig {
            batch_size: 1,
            event_retention: Duration::from_secs(86400),
            hourly_retention: Duration::from_secs(86400),
            archive_events: true,
            ..Default::default()
        },
    )
    .unwrap();
    scheduler.run_due_once().await;
    assert!(source_count(&database, ArchiveKind::Log, 2, true).await > 0);
    database.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn actual_expiry_batch_and_native_partial_index_are_bounded() {
    let (fixture, database, _) = setup().await;
    let time = now();
    seed_all(&database, time).await;
    sqlx::query("INSERT INTO metric.logs(project_id,log_id,received_at,occurred_at_ns,severity,message,message_folded,body,body_size,expire_at) SELECT 2,pg_catalog.int8send(n)||pg_catalog.int8send(n),l.received_at,l.occurred_at_ns,l.severity,l.message,l.message_folded,l.body,l.body_size,$1+n FROM metric.logs l CROSS JOIN generate_series(1::bigint,4096::bigint) n WHERE l.project_id=2")
        .bind(time.unix_millis()-60_000).execute(&database.pool).await.unwrap();
    sqlx::raw_sql("ANALYZE metric.logs")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan: serde_json::Value=sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) SELECT ctid FROM metric.logs WHERE expire_at<=$1 AND archive_due IS NULL ORDER BY expire_at,project_id,log_id LIMIT 2 FOR UPDATE SKIP LOCKED")
        .bind(time.unix_millis()).fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("log_expiry_due"), "{plan}");
    let store = PostgresMaintenanceStore::new(database.clone());
    let mut req = request(MaintenanceTask::EventRetention, time, 2);
    req.cursor = Some(make_cursor(EVENT_EXPIRY + 1, &[]).unwrap());
    let (one, two) = tokio::join!(store.run(req.clone()), store.run(req));
    assert_eq!(one.unwrap().changed, 2);
    assert_eq!(two.unwrap().changed, 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM metric.logs WHERE expire_at IS NOT NULL"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap(),
        4092
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM metric.logs WHERE archive_due IS NOT NULL"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap(),
        2
    );
    database.pool.close().await;
    fixture.cleanup().await;
}
