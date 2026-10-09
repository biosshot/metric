use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, seed_projects},
};
fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn log(p: i32, seed: u128) -> LogRecord {
    LogRecord {
        id: LogId::from_bytes(seed.to_be_bytes()),
        project_id: project(p),
        received_at: at(3000),
        occurred_at_ns: 1_000_000,
        severity: LogSeverity::Error,
        message: "Failure: SQL.*%_'\\ — СНЕГ\0tail".into(),
        trace_id: Some(TraceId::from_bytes([1; 16])),
        span_id: Some(SpanId::from_bytes([2; 8])),
        environment: Some("prod\0east".into()),
        release: Some("web@1".into()),
        service: Some("api".into()),
        body: SignalBody::new(b"{ \"a\": [1, true] }".to_vec()),
    }
}
fn span(p: i32, seed: u128) -> SpanRecord {
    SpanRecord {
        id: SpanRecordId::from_bytes(seed.to_be_bytes()),
        project_id: project(p),
        received_at: at(3000),
        started_at_ns: 1_000_000,
        duration_ns: seed as i64 * 1_000_000,
        trace_id: TraceId::from_bytes([1; 16]),
        span_id: SpanId::from_bytes((seed as u64).to_be_bytes()),
        parent_span_id: Some(SpanId::from_bytes([1; 8])),
        is_segment: true,
        operation_class: SpanOperationClass::HttpServer,
        operation: "http.server".into(),
        status: "ok".into(),
        name: "GET /checkout\0tail".into(),
        environment: Some("prod\0east".into()),
        release: Some("web@1".into()),
        service: Some("api".into()),
        insight_flags: u32::MAX,
        body: SignalBody::new(b"{ \"kind\": \"span\" }".to_vec()),
    }
}
fn log_query(limit: usize) -> LogQuery {
    LogQuery {
        from_ns: 0,
        until_ns: 10_000_000,
        severity: None,
        message: None,
        environment: None,
        release: None,
        service: None,
        trace_id: None,
        before: None,
        limit,
    }
}
fn segment_query(limit: usize) -> SegmentQuery {
    SegmentQuery {
        from_ns: 0,
        until_ns: 10_000_000,
        environment: None,
        release: None,
        service: None,
        before: None,
        limit,
    }
}
fn performance_query() -> PerformanceQuery {
    PerformanceQuery {
        from: at(0),
        until: at(3_600_000),
        environment: None,
        release: None,
        service: None,
        limit: 200,
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresSignalStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let store = PostgresSignalStore::new(database.clone(), SignalRetention::default()).unwrap();
    (fixture, database, store)
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn logs_spans_scoped_idempotency_filters_pages_and_lossless_payloads() {
    let (fixture, database, store) = setup().await;
    let logs: Vec<_> = (1..=5).map(|seed| log(1, seed)).collect();
    assert_eq!(
        store.persist_logs(logs.clone()).await.unwrap(),
        vec![DurableOutcome::Accepted; 5]
    );
    assert_eq!(
        store.load_log(project(1), logs[0].id).await.unwrap(),
        logs[0]
    );
    let mut retry = logs[0].clone();
    retry.received_at = at(5000);
    retry.body = SignalBody::new(b"{}".to_vec());
    retry.severity = LogSeverity::Fatal;
    assert_eq!(
        store.persist_logs(vec![retry]).await.unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    assert_eq!(
        store.load_log(project(1), logs[0].id).await.unwrap(),
        logs[0]
    );
    let mut bad = logs[0].clone();
    bad.message = "different".into();
    assert_eq!(
        store.persist_logs(vec![log(1, 6), bad]).await,
        Err(SignalStoreError::Conflict)
    );
    assert_eq!(
        store.load_log(project(1), log(1, 6).id).await,
        Err(SignalStoreError::NotFound)
    );
    assert_eq!(
        store.load_log(project(2), logs[0].id).await,
        Err(SignalStoreError::NotFound)
    );
    store.persist_logs(vec![log(2, 1)]).await.unwrap();
    let mut query = log_query(2);
    let first = store.list_logs(project(1), query.clone()).await.unwrap();
    assert_eq!(first.items, vec![logs[4].clone(), logs[3].clone()]);
    query.before = first.next;
    let second = store.list_logs(project(1), query.clone()).await.unwrap();
    assert_eq!(second.items, vec![logs[2].clone(), logs[1].clone()]);
    query.before = second.next;
    let third = store.list_logs(project(1), query).await.unwrap();
    assert_eq!(third.items, vec![logs[0].clone()]);
    assert!(third.next.is_none());
    for needle in ["sql.*%_'\\", "снег\0TAIL", "FAILURE", ".*"] {
        let mut query = log_query(200);
        query.message = Some(needle.into());
        assert_eq!(
            store
                .list_logs(project(1), query)
                .await
                .unwrap()
                .items
                .len(),
            5
        );
    }
    let mut query = log_query(200);
    query.message = Some(".*missing".into());
    assert!(
        store
            .list_logs(project(1), query)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let mut query = log_query(200);
    query.severity = Some(LogSeverity::Error);
    query.environment = logs[0].environment.clone();
    query.release = logs[0].release.clone();
    query.service = logs[0].service.clone();
    query.trace_id = logs[0].trace_id;
    assert_eq!(
        store
            .list_logs(project(1), query.clone())
            .await
            .unwrap()
            .items
            .len(),
        5
    );
    query.service = Some("other".into());
    assert!(
        store
            .list_logs(project(1), query)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let spans: Vec<_> = (1..=5).map(|seed| span(1, seed)).collect();
    assert_eq!(
        store.persist_spans(spans.clone()).await.unwrap(),
        vec![DurableOutcome::Accepted; 5]
    );
    assert_eq!(
        store.persist_spans(spans.clone()).await.unwrap(),
        vec![DurableOutcome::Duplicate; 5]
    );
    let page = store
        .list_segments(project(1), segment_query(2))
        .await
        .unwrap();
    assert_eq!(page.items, vec![spans[4].clone(), spans[3].clone()]);
    let mut query = segment_query(200);
    query.before = page.next;
    assert_eq!(
        store
            .list_segments(project(1), query)
            .await
            .unwrap()
            .items
            .len(),
        3
    );
    let view = store
        .trace(vec![project(1)], TraceId::from_bytes([1; 16]), 2, 2)
        .await
        .unwrap();
    assert_eq!(view.spans, spans[..2]);
    assert_eq!(view.logs, logs[..2]);
    assert!(view.partial);
    assert_eq!(view.omitted_spans, 1);
    assert_eq!(
        store
            .trace(vec![project(2)], TraceId::from_bytes([1; 16]), 10, 10)
            .await
            .unwrap()
            .spans
            .len(),
        0
    );
    assert_eq!(
        store
            .trace(vec![project(1)], TraceId::from_bytes([9; 16]), 10, 10)
            .await,
        Err(SignalStoreError::NotFound)
    );
    let bucket = store
        .performance(project(1), performance_query())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(bucket.count, 5);
    assert_eq!(bucket.failure_count, 0);
    assert_eq!(bucket.average_duration_ms, 3.0);
    assert_eq!(bucket.p50_ms, 3.0);
    assert_eq!(bucket.p99_ms, 5.0);
    let mut query = performance_query();
    query.environment = spans[0].environment.clone();
    query.service = spans[0].service.clone();
    query.release = spans[0].release.clone();
    assert_eq!(
        store.performance(project(1), query.clone()).await.unwrap(),
        vec![bucket]
    );
    query.release = Some("other".into());
    assert!(
        store
            .performance(project(1), query)
            .await
            .unwrap()
            .is_empty()
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn shared_full_hour_repair_and_failure_rollback_preserve_complete_statistics() {
    let (fixture, database, store) = setup().await;
    metric_testkit::signal_conformance::whole_hour_rebuild(&store, project(1))
        .await
        .unwrap();
    store
        .persist_spans(vec![span(1, 1), span(1, 2)])
        .await
        .unwrap();
    let before = store
        .performance(project(1), performance_query())
        .await
        .unwrap();
    sqlx::query("CREATE FUNCTION metric.reject_signal_stat() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected statistic failure'; END $$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_signal_stat BEFORE INSERT ON metric.span_stats_hourly FOR EACH ROW EXECUTE FUNCTION metric.reject_signal_stat()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store.rebuild_span_stats(project(1), at(1), at(10)).await,
        Err(SignalStoreError::Unavailable)
    );
    assert_eq!(
        store
            .performance(project(1), performance_query())
            .await
            .unwrap(),
        before
    );
    // Raw admission remains successful even if its derived aggregate is down.
    assert_eq!(
        store.persist_spans(vec![span(1, 3)]).await.unwrap(),
        vec![DurableOutcome::Accepted]
    );
    assert_eq!(
        store
            .performance(project(1), performance_query())
            .await
            .unwrap(),
        before
    );
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM metric.spans WHERE project_id=1 AND is_segment AND NOT stats_applied",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(pending, 1);
    sqlx::query("DROP TRIGGER reject_signal_stat ON metric.span_stats_hourly")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_spans(vec![span(1, 3)]).await.unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    let after = store
        .performance(project(1), performance_query())
        .await
        .unwrap();
    assert_eq!(after[0].count, 3);
    assert_eq!(after[0].average_duration_ms, 2.0);
    for _ in 0..2 {
        assert_eq!(
            store
                .rebuild_span_stats(project(1), at(1), at(10))
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            store
                .performance(project(1), performance_query())
                .await
                .unwrap(),
            after
        );
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_duplicates_and_repair_do_not_lose_or_double_span_counters() {
    let (fixture, database, store) = setup().await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let store = store.clone();
        tasks.spawn(async move { store.persist_spans(vec![span(1, 1)]).await.unwrap() });
    }
    let mut accepted = 0;
    while let Some(result) = tasks.join_next().await {
        accepted += usize::from(result.unwrap()[0] == DurableOutcome::Accepted);
    }
    assert_eq!(accepted, 1);
    let mut writes = tokio::task::JoinSet::new();
    for seed in 2..=65 {
        let store = store.clone();
        writes.spawn(async move { store.persist_spans(vec![span(1, seed)]).await.unwrap() });
    }
    let mut repair = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let store = store.clone();
        repair.spawn(async move {
            store
                .rebuild_span_stats(project(1), at(1), at(10))
                .await
                .unwrap()
        });
    }
    while let Some(result) = writes.join_next().await {
        assert_eq!(result.unwrap(), vec![DurableOutcome::Accepted]);
    }
    while let Some(result) = repair.join_next().await {
        let _ = result.unwrap();
    }
    let bucket = store
        .performance(project(1), performance_query())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(bucket.count, 65);
    assert_eq!(bucket.average_duration_ms, 33.0);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lost_commit_replies_and_second_sql_chunk_failure_are_retryable() {
    let (fixture, database, store) = setup().await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxied_database = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied =
        PostgresSignalStore::new(proxied_database.clone(), SignalRetention::default()).unwrap();
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied.persist_logs(vec![log(1, 1)]).await,
        Err(SignalStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    assert_eq!(
        proxied.persist_logs(vec![log(1, 1)]).await.unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied.persist_spans(vec![span(1, 1)]).await,
        Err(SignalStoreError::Unavailable)
    );
    assert_eq!(
        proxied.persist_spans(vec![span(1, 1)]).await.unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    assert_eq!(
        store
            .performance(project(1), performance_query())
            .await
            .unwrap()[0]
            .count,
        1
    );
    sqlx::query("CREATE FUNCTION metric.reject_log_chunk() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.log_id=decode(lpad(to_hex(1001),32,'0'),'hex') THEN RAISE EXCEPTION 'injected second chunk failure'; END IF; RETURN NEW; END $$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_log_chunk BEFORE INSERT ON metric.logs FOR EACH ROW EXECUTE FUNCTION metric.reject_log_chunk()").execute(&database.pool).await.unwrap();
    let values: Vec<_> = (2..=2001).map(|seed| log(1, seed)).collect();
    assert_eq!(
        store.persist_logs(values.clone()).await,
        Err(SignalStoreError::Unavailable)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.logs WHERE project_id=1")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    sqlx::query("DROP TRIGGER reject_log_chunk ON metric.logs")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_logs(values).await.unwrap(),
        vec![DurableOutcome::Accepted; 2000]
    );
    proxied_database.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn large_payload_reads_are_byte_bounded_and_archive_retention_is_separate() {
    let (fixture, database, store) = setup().await;
    let mut logs = vec![];
    for seed in 1..=12 {
        let mut value = log(1, seed);
        value.body = SignalBody::new(format!("\"{}\"", "x".repeat(900000)).into_bytes());
        logs.push(value);
    }
    store.persist_logs(logs).await.unwrap();
    let page = store.list_logs(project(1), log_query(200)).await.unwrap();
    assert!(!page.items.is_empty());
    assert!(page.items.len() < 12);
    assert!(page.next.is_some());
    assert!(page.items.iter().map(log_size).sum::<usize>() <= MAX_READ_BYTES as usize);
    let mut next = log_query(200);
    next.before = page.next;
    let second = store.list_logs(project(1), next).await.unwrap();
    assert_eq!(page.items.len() + second.items.len(), 12);
    assert!(second.next.is_none());
    let mut spans = vec![];
    for seed in 1..=12 {
        let mut value = span(1, seed);
        value.body = SignalBody::new(format!("\"{}\"", "y".repeat(900000)).into_bytes());
        spans.push(value);
    }
    store.persist_spans(spans).await.unwrap();
    let segments = store
        .list_segments(project(1), segment_query(200))
        .await
        .unwrap();
    assert!(segments.items.len() < 12);
    assert!(segments.next.is_some());
    assert!(segments.items.iter().map(span_size).sum::<usize>() <= MAX_READ_BYTES as usize);
    let view = store
        .trace(vec![project(1)], TraceId::from_bytes([1; 16]), 2000, 1000)
        .await
        .unwrap();
    assert!(view.partial);
    assert!(view.logs.len() < 12);
    assert!(view.spans.len() < 12);
    assert_eq!(view.omitted_spans, 1);
    let archive = PostgresSignalStore::new(
        database.clone(),
        SignalRetention {
            archive: true,
            ..SignalRetention::default()
        },
    )
    .unwrap();
    archive.persist_logs(vec![log(2, 1)]).await.unwrap();
    archive.persist_spans(vec![span(2, 1)]).await.unwrap();
    for query in [
        "SELECT expire_at,archive_due FROM metric.logs WHERE project_id=2",
        "SELECT expire_at,archive_due FROM metric.spans WHERE project_id=2",
    ] {
        let row = sqlx::query(query).fetch_one(&database.pool).await.unwrap();
        assert_eq!(row.try_get::<Option<i64>, _>("expire_at").unwrap(), None);
        assert_eq!(
            row.try_get::<Option<i64>, _>("archive_due").unwrap(),
            Some(3000 + 30 * 86_400_000)
        );
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn invalid_data_project_fences_and_corrupt_bodies_fail_closed() {
    let (fixture, database, store) = setup().await;
    for value in [
        vec![],
        b"invalid".to_vec(),
        format!("\"{}\"", "x".repeat(MAX_BODY)).into_bytes(),
    ] {
        let mut bad = log(1, 1);
        bad.body = SignalBody::new(value);
        assert_eq!(
            store.persist_logs(vec![bad]).await,
            Err(SignalStoreError::InvalidData)
        );
    }
    let mut bad = span(1, 1);
    bad.duration_ns = -1;
    assert_eq!(
        store.persist_spans(vec![bad]).await,
        Err(SignalStoreError::InvalidData)
    );
    let mut bad = span(1, 1);
    bad.name = "x".repeat(1025).into();
    assert_eq!(
        store.persist_spans(vec![bad]).await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(
        store.persist_logs(vec![log(1, 1); 10001]).await,
        Err(SignalStoreError::InvalidData)
    );
    let mut large = log(1, 1);
    large.body = SignalBody::new(format!("\"{}\"", "x".repeat(900000)).into_bytes());
    assert_eq!(
        store.persist_logs(vec![large; 19]).await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(
        store.persist_spans(vec![span(99, 1)]).await,
        Err(SignalStoreError::NotFound)
    );
    for limit in [0, 201] {
        assert_eq!(
            store.list_logs(project(1), log_query(limit)).await,
            Err(SignalStoreError::InvalidData)
        );
        assert_eq!(
            store.list_segments(project(1), segment_query(limit)).await,
            Err(SignalStoreError::InvalidData)
        );
    }
    sqlx::query("UPDATE metric.projects SET state=1 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    store.persist_logs(vec![log(1, 1)]).await.unwrap();
    store.persist_spans(vec![span(1, 1)]).await.unwrap();
    sqlx::query("UPDATE metric.projects SET state=2 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_logs(vec![log(1, 2)]).await,
        Err(SignalStoreError::NotFound)
    );
    assert_eq!(
        store.rebuild_span_stats(project(1), at(0), at(100)).await,
        Err(SignalStoreError::NotFound)
    );
    for body in [vec![1_u8, 0, 255], vec![1_u8, 2, 1]] {
        sqlx::query("UPDATE metric.logs SET body=$1 WHERE project_id=1")
            .bind(body)
            .execute(&database.pool)
            .await
            .unwrap();
        assert_eq!(
            store.load_log(project(1), log(1, 1).id).await,
            Err(SignalStoreError::InvalidData)
        );
    }
    sqlx::query("UPDATE metric.spans SET dimensions=$1 WHERE project_id=1")
        .bind(b"[]".as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.list_segments(project(1), segment_query(200)).await,
        Err(SignalStoreError::InvalidData)
    );
    for (from, until, expected) in [
        (-1, 1, (-3600000, 3600000)),
        (-3600000, 0, (-3600000, 0)),
        (0, 3600000, (0, 3600000)),
    ] {
        let actual = rebuild_bounds(at(from), at(until)).unwrap();
        assert_eq!((actual.0, actual.1), expected);
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn maximum_batches_sample_cap_and_native_scoped_index_plans() {
    let (fixture, database, store) = setup().await;
    let values: Vec<_> = (1..=10000).map(|seed| log(2, seed)).collect();
    assert_eq!(
        store.persist_logs(values).await.unwrap(),
        vec![DurableOutcome::Accepted; 10000]
    );
    store.persist_logs(vec![log(1, 1)]).await.unwrap();
    let values: Vec<_> = (1..=2100).map(|seed| span(1, seed)).collect();
    store.persist_spans(values).await.unwrap();
    let bucket = store
        .performance(project(1), performance_query())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(bucket.count, 2100);
    assert_eq!(bucket.p50_ms, 1077.0);
    let count: i64 = sqlx::query_scalar(
        "SELECT cardinality(samples)::bigint FROM metric.span_stats_hourly WHERE project_id=1",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(count, 2048);
    // Normal vacuum flushes the GIN pending list after a bulk load. Keep default
    // durability, planner, fastupdate and autovacuum settings intact.
    sqlx::query("VACUUM(ANALYZE) metric.logs")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE metric.spans")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT log_id FROM metric.logs WHERE project_id=1 AND occurred_at_ns>=0 AND occurred_at_ns<10000000 ORDER BY occurred_at_ns DESC,log_id DESC LIMIT 201").fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("log_project_time"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(1.0));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT log_id FROM metric.logs WHERE project_id=2 AND message_trigrams @> metric.signal_trigrams($1)").bind(b"unique-absent".as_slice()).fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("log_message_trigrams"), "{plan}");
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT record_id FROM metric.spans WHERE project_id=1 AND trace_id=$1 ORDER BY started_at_ns,span_id,record_id LIMIT 201").bind(vec![1_u8;16]).fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("span_project_trace_time"));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn existing_writers_and_finalized_error_trace_correlation_are_reused() {
    use metric_application::{
        log_writer::{LogWriter, LogWriterConfig},
        shutdown::ShutdownRoot,
        span_writer::{SpanWriter, SpanWriterConfig},
    };
    use metric_domain::{
        AcceptedEvent, ScrubbedEventPayload,
        finalization::{FinalizationPolicy, FinalizeBatch, ProcessedEventPayload},
    };
    use metric_ports::{EventStore, FinalizationStore, LogSink, SpanSink};
    use std::{sync::Arc, time::Duration};
    let (fixture, database, store) = setup().await;
    let root = ShutdownRoot::new();
    let shared = Arc::new(store.clone());
    let (logs, log_task) =
        LogWriter::start(shared.clone(), LogWriterConfig::default(), root.signal()).unwrap();
    let (spans, span_task) =
        SpanWriter::start(shared, SpanWriterConfig::default(), root.signal()).unwrap();
    let (log_result, span_result) = tokio::join!(
        logs.persist_logs(vec![log(1, 1)]),
        spans.persist_spans(vec![span(1, 1)])
    );
    assert_eq!(log_result.unwrap(), vec![DurableOutcome::Accepted]);
    assert_eq!(span_result.unwrap(), vec![DurableOutcome::Accepted]);
    root.begin();
    log_task.wait().await;
    span_task.wait().await;
    let events =
        crate::PostgresEventStore::new(database.clone(), crate::EventCodecConfig::default())
            .unwrap();
    let finalizer = crate::PostgresFinalizationStore::new(
        database.clone(),
        crate::EventCodecConfig::default(),
        crate::IssueCodecConfig::default(),
    )
    .unwrap();
    let mut identities = vec![];
    for p in [1, 2] {
        let id = EventId::from_bytes([p as u8; 16]);
        let prepared = events
            .prepare(AcceptedEvent {
                project_id: project(p),
                event_id: id,
                received_at: at(2000),
                policy_revision: 1,
                payload: ScrubbedEventPayload::new(
                    serde_json::to_vec(&serde_json::json!({"event_id":id.to_string()})).unwrap(),
                ),
            })
            .unwrap();
        events.insert_batch(&[prepared]).await.unwrap();
        let mut value = crate::test_support::finalized(p, 1, id, 1000, 2000, None, None);
        let mut payload: serde_json::Value =
            serde_json::from_slice(value.payload.as_bytes()).unwrap();
        payload["contexts"] = serde_json::json!({"trace":{"trace_id":TraceId::from_bytes([1;16]).to_string(),"span_id":SpanId::from_bytes([2;8]).to_string()}});
        value.payload = ProcessedEventPayload::new(serde_json::to_vec(&payload).unwrap());
        finalizer
            .finalize(
                FinalizeBatch {
                    events: vec![value],
                },
                FinalizationPolicy {
                    event_retention: Duration::from_secs(86400),
                    hourly_retention: Duration::from_secs(864000),
                    archive_events: false,
                    max_implicit_releases_per_project_day: 10,
                    max_implicit_environments_per_project: 10,
                },
            )
            .await
            .unwrap();
        identities.push(id);
    }
    let view = store
        .trace(vec![project(1)], TraceId::from_bytes([1; 16]), 10, 10)
        .await
        .unwrap();
    assert_eq!(view.errors, vec![identities[0]]);
    assert_eq!(view.spans.len(), 1);
    assert_eq!(view.logs.len(), 1);
    assert!(!view.partial);
    let view = store
        .trace(
            vec![project(1), project(2), project(1)],
            TraceId::from_bytes([1; 16]),
            10,
            0,
        )
        .await
        .unwrap();
    assert_eq!(view.errors, identities);
    assert!(view.logs.is_empty());
    database.close().await;
    fixture.cleanup().await;
}
