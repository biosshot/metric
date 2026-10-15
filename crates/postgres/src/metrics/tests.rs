use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, seed_projects},
};
use metric_domain::{ProjectId, metrics::MetricSeries, signals::TraceId};

fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn delta(project_id: i32, name: &str, kind: MetricKind, value: f64) -> MetricDelta {
    MetricDelta {
        series: MetricSeries {
            project_id: project(project_id),
            name: name.into(),
            kind,
            unit: "none".into(),
            tags: BTreeMap::new(),
        },
        bucket_start: at(60_000),
        bucket_width_seconds: 60,
        received_at: at(60_001),
        trace_id: None,
        aggregate: MetricAggregate::from_measurement(kind, value),
    }
}
fn batch(values: impl IntoIterator<Item = MetricDelta>) -> MetricDeltaBatch {
    let mut batch = MetricDeltaBatch::default();
    for value in values {
        batch.push(value);
    }
    batch
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresMetricStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let store = PostgresMetricStore::new(database.clone(), MetricRetention::default()).unwrap();
    (fixture, database, store)
}
async fn row(database: &PostgresDatabase, value: &MetricDelta) -> sqlx::postgres::PgRow {
    sqlx::query("SELECT * FROM metric.metric_buckets WHERE project_id=$1 AND bucket_id=$2")
        .bind(value.series.project_id.get())
        .bind(value.bucket_id().as_slice())
        .fetch_one(&database.pool)
        .await
        .unwrap()
}
async fn count(database: &PostgresDatabase, value: &MetricDelta) -> i64 {
    row(database, value)
        .await
        .try_get("measurement_count")
        .unwrap()
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn aggregates_lossless_metadata_correlation_and_retention() {
    let (fixture, database, store) = setup().await;
    let counter = delta(1, "requests", MetricKind::Counter, 2.0);
    let gauge = delta(1, "queue", MetricKind::Gauge, 5.0);
    let distribution = delta(1, "latency", MetricKind::Distribution, 8.0);
    store
        .persist_metrics(batch([
            counter.clone(),
            gauge.clone(),
            distribution.clone(),
        ]))
        .await
        .unwrap();
    let mut second_gauge = delta(1, "queue", MetricKind::Gauge, -3.0);
    second_gauge.received_at = at(61_000);
    store
        .persist_metrics(batch([
            counter.clone(),
            second_gauge,
            delta(1, "latency", MetricKind::Distribution, 2.0),
        ]))
        .await
        .unwrap();
    assert_eq!(count(&database, &counter).await, 2);
    assert_eq!(
        row(&database, &counter)
            .await
            .try_get::<f64, _>("aggregate_sum")
            .unwrap(),
        4.0
    );
    let values = row(&database, &gauge).await;
    assert_eq!(
        (
            values.try_get::<f64, _>("last_value").unwrap(),
            values.try_get::<f64, _>("minimum").unwrap(),
            values.try_get::<f64, _>("maximum").unwrap(),
            values.try_get::<f64, _>("aggregate_sum").unwrap()
        ),
        (-3.0, -3.0, 5.0, 2.0)
    );
    assert_eq!(values.try_get::<i64, _>("received_at").unwrap(), 61_000);
    let values = row(&database, &distribution).await;
    let bins: Vec<i64> = values.try_get("bins").unwrap();
    assert_eq!(bins.len(), 64);
    assert_eq!(bins.iter().sum::<i64>(), 2);
    assert_eq!(
        (
            values.try_get::<f64, _>("minimum").unwrap(),
            values.try_get::<f64, _>("maximum").unwrap(),
            values.try_get::<f64, _>("aggregate_sum").unwrap()
        ),
        (2.0, 8.0, 10.0)
    );
    let mut text = delta(
        1,
        "'; DROP TABLE metric.projects; -- 雪\0tail",
        MetricKind::Counter,
        1.0,
    );
    text.series
        .tags
        .insert("a.$\0key".into(), "value\0\";--雪".into());
    text.trace_id = Some(TraceId::from_bytes([1; 16]));
    store.persist_metrics(batch([text.clone()])).await.unwrap();
    let mut newer = text.clone();
    newer.trace_id = Some(TraceId::from_bytes([2; 16]));
    newer.received_at = at(59_000);
    let archived = PostgresMetricStore::new(
        database.clone(),
        MetricRetention {
            days: 14,
            archive: true,
            ..MetricRetention::default()
        },
    )
    .unwrap();
    archived.persist_metrics(batch([newer])).await.unwrap();
    let values = row(&database, &text).await;
    assert_eq!(
        values.try_get::<Vec<u8>, _>("name").unwrap(),
        text.series.name.as_bytes()
    );
    assert_eq!(
        serde_json::from_slice::<BTreeMap<Box<str>, Box<str>>>(
            &values.try_get::<Vec<u8>, _>("tags").unwrap()
        )
        .unwrap(),
        text.series.tags
    );
    assert_eq!(
        values.try_get::<Vec<u8>, _>("trace_id").unwrap(),
        vec![1; 16]
    );
    assert_eq!(values.try_get::<i64, _>("received_at").unwrap(), 59_000);
    assert_eq!(
        values.try_get::<Option<i64>, _>("archive_due").unwrap(),
        None
    );
    assert_eq!(
        values.try_get::<i64, _>("expire_at").unwrap(),
        60_000 + 90 * DAY
    );
    let foreign = delta(2, "requests", MetricKind::Counter, 11.0);
    archived
        .persist_metrics(batch([foreign.clone()]))
        .await
        .unwrap();
    let values = row(&database, &foreign).await;
    assert_eq!(values.try_get::<Option<i64>, _>("expire_at").unwrap(), None);
    assert_eq!(
        values.try_get::<i64, _>("archive_due").unwrap(),
        60_000 + 14 * DAY
    );
    // Wide names are bounded metadata, never oversized B-tree keys.
    let wide = delta(1, &"w".repeat(20_000), MetricKind::Counter, 1.0);
    store.persist_metrics(batch([wide.clone()])).await.unwrap();
    assert_eq!(
        row(&database, &wide)
            .await
            .try_get::<Vec<u8>, _>("name")
            .unwrap()
            .len(),
        20_000
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn cardinality_admission_is_concurrent_scoped_and_batch_atomic() {
    let (fixture, database, _) = setup().await;
    let store = PostgresMetricStore::new(
        database.clone(),
        MetricRetention {
            max_series_per_project: 1,
            ..MetricRetention::default()
        },
    )
    .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..16 {
        let store = store.clone();
        tasks.spawn(async move {
            let value = delta(1, &format!("series-{n}"), MetricKind::Counter, 1.0);
            (store.persist_metrics(batch([value.clone()])).await, value)
        });
    }
    let mut accepted = vec![];
    while let Some(result) = tasks.join_next().await {
        let (result, value) = result.unwrap();
        match result {
            Ok(DurableOutcome::Accepted) => accepted.push(value),
            Err(SignalStoreError::Capacity) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
    assert_eq!(accepted.len(), 1);
    let first = accepted.pop().unwrap();
    let mut next = first.clone();
    next.bucket_start = at(120_000);
    store.persist_metrics(batch([next])).await.unwrap();
    let foreign = delta(2, "independent", MetricKind::Gauge, 1.0);
    store
        .persist_metrics(batch([foreign.clone()]))
        .await
        .unwrap();
    assert_eq!(
        store
            .persist_metrics(batch([
                first.clone(),
                delta(2, "new-foreign", MetricKind::Counter, 1.0)
            ]))
            .await,
        Err(SignalStoreError::Capacity)
    );
    assert_eq!(count(&database, &first).await, 1);
    assert_eq!(count(&database, &foreign).await, 1);
    sqlx::query("DELETE FROM metric.metric_buckets WHERE project_id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    store
        .persist_metrics(batch([delta(
            1,
            "released-capacity",
            MetricKind::Counter,
            1.0,
        )]))
        .await
        .unwrap();
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_additions_and_existing_metric_writer_preserve_deltas() {
    use metric_application::{
        metric_writer::{MetricWriter, MetricWriterConfig},
        shutdown::ShutdownRoot,
    };
    use metric_ports::MetricSink;
    use std::sync::Arc;
    let (fixture, database, store) = setup().await;
    let value = delta(1, "hot", MetricKind::Counter, 1.0);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let store = store.clone();
        let value = value.clone();
        tasks.spawn(async move { store.persist_metrics(batch([value])).await.unwrap() });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap(), DurableOutcome::Accepted);
    }
    assert_eq!(count(&database, &value).await, 32);
    let root = ShutdownRoot::new();
    let (writer, task) = MetricWriter::start(
        Arc::new(store),
        MetricWriterConfig::default(),
        root.signal(),
    )
    .unwrap();
    let (a, b) = tokio::join!(
        writer.persist_metrics(batch([value.clone()])),
        writer.persist_metrics(batch([value.clone()]))
    );
    assert_eq!(
        (a, b),
        (Ok(DurableOutcome::Accepted), Ok(DurableOutcome::Accepted))
    );
    root.begin();
    task.wait().await;
    assert_eq!(count(&database, &value).await, 34);
    assert_eq!(
        row(&database, &value)
            .await
            .try_get::<f64, _>("aggregate_sum")
            .unwrap(),
        34.0
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn bulk_failure_after_first_chunk_and_numeric_overflow_rollback() {
    let (fixture, database, store) = setup().await;
    let values =
        batch((0..2000).map(|n| delta(1, &format!("bucket-{n}"), MetricKind::Counter, 1.0)));
    sqlx::query("CREATE FUNCTION metric.fail_metric_chunk() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF (SELECT count(*) FROM metric.metric_buckets)>=1000 THEN RAISE EXCEPTION 'synthetic later chunk failure'; END IF; RETURN NEW; END $$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_metric_chunk BEFORE INSERT ON metric.metric_buckets FOR EACH ROW EXECUTE FUNCTION metric.fail_metric_chunk()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store.persist_metrics(values.clone()).await,
        Err(SignalStoreError::Unavailable)
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.metric_buckets")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    sqlx::query("DROP TRIGGER fail_metric_chunk ON metric.metric_buckets")
        .execute(&database.pool)
        .await
        .unwrap();
    store.persist_metrics(values).await.unwrap();
    let first = delta(1, "bucket-0", MetricKind::Counter, 1.0);
    let second = delta(1, "bucket-1", MetricKind::Counter, 1.0);
    sqlx::query(
        "UPDATE metric.metric_buckets SET measurement_count=$1 WHERE project_id=1 AND bucket_id=$2",
    )
    .bind(i64::MAX)
    .bind(first.bucket_id().as_slice())
    .execute(&database.pool)
    .await
    .unwrap();
    assert_eq!(
        store
            .persist_metrics(batch([first.clone(), second.clone()]))
            .await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(count(&database, &first).await, i64::MAX);
    assert_eq!(count(&database, &second).await, 1);
    sqlx::query("UPDATE metric.metric_buckets SET measurement_count=1,aggregate_sum=$1 WHERE project_id=1 AND bucket_id=$2").bind(f64::MAX).bind(first.bucket_id().as_slice()).execute(&database.pool).await.unwrap();
    let mut overflow = first;
    overflow.aggregate = MetricAggregate::from_measurement(MetricKind::Counter, f64::MAX);
    assert_eq!(
        store
            .persist_metrics(batch([overflow, second.clone()]))
            .await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(count(&database, &second).await, 1);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lost_commit_reply_is_not_retried_and_client_retry_is_additive() {
    use metric_application::{
        metric_writer::{MetricWriter, MetricWriterConfig},
        shutdown::ShutdownRoot,
    };
    use metric_ports::MetricSink;
    use std::sync::Arc;
    let (fixture, database, store) = setup().await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxied_database = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied =
        PostgresMetricStore::new(proxied_database.clone(), MetricRetention::default()).unwrap();
    let root = ShutdownRoot::new();
    let (writer, task) = MetricWriter::start(
        Arc::new(proxied),
        MetricWriterConfig::default(),
        root.signal(),
    )
    .unwrap();
    let value = delta(1, "ambiguous", MetricKind::Counter, 1.0);
    proxy.drop_next_commit_reply();
    assert_eq!(
        writer.persist_metrics(batch([value.clone()])).await,
        Err(SignalStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    root.begin();
    task.wait().await;
    assert_eq!(count(&database, &value).await, 1);
    assert_eq!(
        store.persist_metrics(batch([value.clone()])).await.unwrap(),
        DurableOutcome::Accepted
    );
    assert_eq!(count(&database, &value).await, 2);
    proxied_database.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn invalid_bounds_identity_conflicts_and_project_fences_leave_no_effects() {
    let (fixture, database, store) = setup().await;
    let normal = delta(1, "valid", MetricKind::Counter, 1.0);
    let mut bad = vec![];
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        bad.push(batch([delta(1, "invalid", MetricKind::Counter, value)]));
    }
    let mut value = normal.clone();
    value.series.name = "".into();
    bad.push(batch([value]));
    let mut value = normal.clone();
    value.series.unit = "u".repeat(MAX_METADATA + 1).into();
    bad.push(batch([value]));
    let mut value = normal.clone();
    value.bucket_width_seconds = 0;
    bad.push(batch([value]));
    let mut value = normal.clone();
    value.aggregate = MetricAggregate::Gauge {
        last: 1.0,
        min: 0.0,
        max: 2.0,
        sum: 1.0,
        count: 1,
    };
    bad.push(batch([value]));
    let mut value = normal.clone();
    value.aggregate = MetricAggregate::Counter {
        sum: 1.0,
        count: u64::MAX,
    };
    bad.push(batch([value]));
    let mut value = normal.clone();
    value.bucket_start = at(253402300799999);
    bad.push(batch([value]));
    let mut value = batch([normal.clone()]);
    let (_, record) = value.deltas.pop_first().unwrap();
    value.deltas.insert(([0; 16], 0), record);
    bad.push(value);
    bad.push(MetricDeltaBatch::default());
    bad.push(batch((0..MAX_BATCH + 1).map(|n| {
        delta(1, &format!("large-{n}"), MetricKind::Counter, 1.0)
    })));
    for invalid in bad {
        assert_eq!(
            store.persist_metrics(invalid).await,
            Err(SignalStoreError::InvalidData)
        );
    }
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.metric_buckets")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let mut missing = normal.clone();
    missing.series.project_id = project(99);
    assert_eq!(
        store.persist_metrics(batch([missing])).await,
        Err(SignalStoreError::NotFound)
    );
    sqlx::query("UPDATE metric.projects SET state=1 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    store
        .persist_metrics(batch([normal.clone()]))
        .await
        .unwrap();
    sqlx::query("UPDATE metric.projects SET state=2 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_metrics(batch([normal.clone()])).await,
        Err(SignalStoreError::NotFound)
    );
    assert_eq!(count(&database, &normal).await, 1);
    sqlx::query("UPDATE metric.projects SET state=0 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE metric.metric_buckets SET name=$1 WHERE project_id=1")
        .bind(b"corrupt identity".as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_metrics(batch([normal])).await,
        Err(SignalStoreError::Conflict)
    );
    // A separator-bearing series hash collision must not merge unrelated metadata.
    let mut first = delta(2, "same", MetricKind::Counter, 1.0);
    first.series.unit = "x\0a\0b".into();
    let mut second = delta(2, "same", MetricKind::Counter, 1.0);
    second.series.unit = "x".into();
    second.series.tags.insert("a".into(), "b".into());
    assert_eq!(first.series.id(), second.series.id());
    store.persist_metrics(batch([first.clone()])).await.unwrap();
    assert_eq!(
        store.persist_metrics(batch([second])).await,
        Err(SignalStoreError::Conflict)
    );
    assert_eq!(count(&database, &first).await, 1);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn maximum_batch_and_scoped_time_series_name_indexes_are_bounded() {
    let (fixture, database, store) = setup().await;
    let value = delta(1, "selected", MetricKind::Counter, 1.0);
    store.persist_metrics(batch([value.clone()])).await.unwrap();
    store
        .persist_metrics(batch(
            (0..MAX_BATCH).map(|n| delta(2, &format!("unrelated-{n}"), MetricKind::Counter, 1.0)),
        ))
        .await
        .unwrap();
    sqlx::query("ANALYZE metric.metric_buckets")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT bucket_id FROM metric.metric_buckets WHERE project_id=1 AND bucket_start>=0 AND bucket_start<120000 ORDER BY bucket_start DESC,bucket_id LIMIT 501").fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("metric_project_time_name"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(1.0));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT DISTINCT series_id FROM metric.metric_buckets WHERE project_id=1 LIMIT 10001").fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("metric_project_series"));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT bucket_id FROM metric.metric_buckets WHERE project_id=2 AND name_key=sha256($1) AND name=$1 AND bucket_start>=0 AND bucket_start<120000").bind(b"unrelated-42".as_slice()).fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("metric_project_name_time"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(1.0));
    assert_eq!(count(&database, &value).await, 1);
    database.close().await;
    fixture.cleanup().await;
}
