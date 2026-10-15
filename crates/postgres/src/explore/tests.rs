use super::*;
use crate::{
    EventCodecConfig, IssueCodecConfig, MetricRetention, MigrationPolicy, PostgresEventStore,
    PostgresFinalizationStore, PostgresMetricStore, PostgresSchemaManager, PostgresSignalStore,
    SignalRetention,
    test_support::{Fixture, finalized, seed_projects},
};
use metric_domain::{
    AcceptedEvent, EventId, ProjectId, ScrubbedEventPayload,
    finalization::{FinalizationPolicy, FinalizeBatch},
    metrics::{MetricAggregate, MetricDelta, MetricDeltaBatch, MetricKind, MetricSeries},
    signals::{
        LogId, LogRecord, LogSeverity, SignalBody, SpanId, SpanOperationClass, SpanRecord,
        SpanRecordId, TraceId,
    },
};
use metric_ports::{EventStore, FinalizationStore, MetricStore, SignalStore};
fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn plan(dataset: ExploreDataset) -> ExplorePlan {
    ExplorePlan {
        project_id: project(1),
        query: ExploreQuery {
            dataset,
            from: at(0),
            until: at(120_000),
            predicates: vec![],
            expression: None,
            aggregates: vec![],
            group_by: vec![],
            interval: None,
            cursor: None,
            limit: 50,
        },
        normalized: "test".into(),
        cost: 100,
        maximum_time: Duration::from_secs(5),
    }
}
fn predicate(field: ExploreField, op: ExplorePredicateOp, value: ExploreValue) -> ExplorePredicate {
    ExplorePredicate {
        field,
        op,
        value: Some(value),
        upper: None,
    }
}
fn text(field: ExploreField, value: &str) -> ExplorePredicate {
    predicate(
        field,
        ExplorePredicateOp::Exact,
        ExploreValue::String(value.into()),
    )
}
fn aggregate(
    kind: ExploreAggregateKind,
    field: Option<ExploreField>,
    alias: &str,
) -> ExploreAggregate {
    ExploreAggregate {
        kind,
        field,
        alias: alias.into(),
    }
}
fn log(p: i32, seed: u128) -> LogRecord {
    LogRecord {
        id: LogId::from_bytes(seed.to_be_bytes()),
        project_id: project(p),
        received_at: at(1000),
        occurred_at_ns: 1_000_000,
        severity: LogSeverity::Info,
        message: "SQL.*%_'\\ — СНЕГ\0tail".into(),
        trace_id: Some(TraceId::from_bytes([1; 16])),
        span_id: Some(SpanId::from_bytes([2; 8])),
        environment: Some("prod\0east".into()),
        release: Some("web@1".into()),
        service: None,
        body: SignalBody::new(b"{}".to_vec()),
    }
}
fn span(p: i32, seed: u128) -> SpanRecord {
    SpanRecord {
        id: SpanRecordId::from_bytes(seed.to_be_bytes()),
        project_id: project(p),
        received_at: at(1000),
        started_at_ns: 1_000_000,
        duration_ns: seed as i64 * 1_000_000,
        trace_id: TraceId::from_bytes([1; 16]),
        span_id: SpanId::from_bytes((seed as u64).to_be_bytes()),
        parent_span_id: None,
        is_segment: false,
        operation_class: SpanOperationClass::HttpServer,
        operation: "http.server".into(),
        status: "ok".into(),
        name: "GET /checkout\0tail".into(),
        environment: Some("prod\0east".into()),
        release: None,
        service: Some("api".into()),
        insight_flags: 0,
        body: SignalBody::new(b"{}".to_vec()),
    }
}
fn delta(p: i32, name: &str, kind: MetricKind, value: f64) -> MetricDelta {
    MetricDelta {
        series: MetricSeries {
            project_id: project(p),
            name: name.into(),
            kind,
            unit: "none".into(),
            tags: BTreeMap::new(),
        },
        bucket_start: at(60_000),
        bucket_width_seconds: 60,
        received_at: at(61_000),
        trace_id: Some(TraceId::from_bytes([1; 16])),
        aggregate: MetricAggregate::from_measurement(kind, value),
    }
}
async fn metrics(database: &PostgresDatabase, values: impl IntoIterator<Item = MetricDelta>) {
    let mut batch = MetricDeltaBatch::default();
    for value in values {
        batch.push(value);
    }
    PostgresMetricStore::new(database.clone(), MetricRetention::default())
        .unwrap()
        .persist_metrics(batch)
        .await
        .unwrap();
}
async fn setup() -> (
    Fixture,
    PostgresDatabase,
    PostgresExploreStore,
    PostgresSignalStore,
) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let explore = PostgresExploreStore::new(database.clone());
    let signals = PostgresSignalStore::new(database.clone(), SignalRetention::default()).unwrap();
    (fixture, database, explore, signals)
}
async fn errors(database: &PostgresDatabase) {
    let events = PostgresEventStore::new(database.clone(), EventCodecConfig::default()).unwrap();
    let finalizer = PostgresFinalizationStore::new(
        database.clone(),
        EventCodecConfig::default(),
        IssueCodecConfig::default(),
    )
    .unwrap();
    let mut batch = Vec::new();
    for (p, seed) in [(1, 1_u128), (1, 2), (2, 1), (1, 3)] {
        let value = finalized(
            p,
            1,
            EventId::from_bytes(seed.to_be_bytes()),
            1000,
            2000,
            None,
            None,
        );
        let prepared = events
            .prepare(AcceptedEvent {
                project_id: value.project_id,
                event_id: value.event_id,
                received_at: value.received_at,
                policy_revision: 1,
                payload: ScrubbedEventPayload::new(serde_json::to_vec(&serde_json::json!({"event_id":value.event_id.to_string(),"message":"synthetic Explore event"})).unwrap()),
            })
            .unwrap();
        events.insert_batch(&[prepared]).await.unwrap();
        if seed != 3 {
            batch.push(value);
        }
    }
    finalizer
        .finalize(
            FinalizeBatch { events: batch },
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
}

#[test]
fn compiler_binds_values_and_rejects_unbounded_or_untyped_input() {
    let marker = "'; DROP TABLE metric.projects; --\0tail";
    let mut query = plan(ExploreDataset::Logs);
    query
        .query
        .predicates
        .push(text(ExploreField::Message, marker));
    let compiled = compile(&query).unwrap();
    assert!(!compiled.query.sql().as_str().contains(marker));
    assert!(compiled.query.sql().as_str().contains("project_id=$"));
    query.query.predicates[0].field = ExploreField::Platform;
    assert_eq!(validate(&query), Err(ExploreStoreError::InvalidData));
    query = plan(ExploreDataset::Logs);
    query.query.expression = Some(ExploreExpression::Or(vec![
        ExploreExpression::And(vec![]);
        65
    ]));
    assert_eq!(validate(&query), Err(ExploreStoreError::InvalidData));
    query.query.expression = Some(ExploreExpression::Or(vec![]));
    assert_eq!(validate(&query), Err(ExploreStoreError::InvalidData));
    let mut nested = ExploreExpression::And(vec![]);
    for _ in 0..257 {
        nested = ExploreExpression::Not(Box::new(nested));
    }
    query.query.expression = Some(nested);
    assert_eq!(validate(&query), Err(ExploreStoreError::InvalidData));
    query = plan(ExploreDataset::Metrics);
    query.query.cursor = Some(ExploreCursor {
        time: 0,
        id: [0; 20],
        id_len: 21,
    });
    assert_eq!(validate(&query), Err(ExploreStoreError::InvalidData));
    query.query.cursor = None;
    query
        .query
        .aggregates
        .push(aggregate(ExploreAggregateKind::Count, None, "x);select"));
    assert_eq!(validate(&query), Err(ExploreStoreError::InvalidData));
    query = plan(ExploreDataset::Spans);
    query.query.predicates.push(predicate(
        ExploreField::DurationMs,
        ExplorePredicateOp::Greater,
        ExploreValue::Integer(i64::MAX),
    ));
    assert!(compile(&query).is_err());
    query = plan(ExploreDataset::Logs);
    query.query.until = at(i64::MAX / 1_000_000 + 1);
    assert!(compile(&query).is_err());
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn all_datasets_raw_fields_scope_and_stable_keysets() {
    let (fixture, database, store, signals) = setup().await;
    signals
        .persist_logs(vec![log(1, 1), log(1, 2), log(2, 2)])
        .await
        .unwrap();
    signals
        .persist_spans(vec![span(1, 1), span(1, 2), span(2, 2)])
        .await
        .unwrap();
    metrics(
        &database,
        [
            delta(1, "requests\0tail", MetricKind::Counter, 2.0),
            delta(1, "queue", MetricKind::Gauge, 5.0),
            delta(2, "queue", MetricKind::Gauge, 50.0),
        ],
    )
    .await;
    errors(&database).await;
    for dataset in [
        ExploreDataset::Errors,
        ExploreDataset::Logs,
        ExploreDataset::Spans,
        ExploreDataset::Metrics,
    ] {
        let mut query = plan(dataset);
        query.query.limit = 1;
        let first = store.execute(query.clone()).await.unwrap();
        assert_eq!(first.rows.len(), 1);
        let cursor = first.next.unwrap();
        assert_eq!(
            cursor.id_len,
            if dataset == ExploreDataset::Errors {
                20
            } else {
                16
            }
        );
        assert_eq!(
            cursor.time,
            if dataset == ExploreDataset::Metrics {
                60_000
            } else if nanos(dataset) {
                1_000_000
            } else {
                1000
            }
        );
        query.query.cursor = Some(cursor);
        let second = store.execute(query).await.unwrap();
        assert_eq!(second.rows.len(), 1);
        assert!(second.next.is_none());
        assert!(first.rows[0].values["id"] != second.rows[0].values["id"]);
        assert_eq!(
            first.rows[0].values.len(),
            1 + FIELDS.iter().filter(|f| f.accepted_by(dataset)).count()
        );
        match dataset {
            ExploreDataset::Errors => {
                assert_eq!(
                    first.rows[0].values["level"],
                    ExploreValue::String("error".into())
                );
                assert_eq!(
                    first.rows[0].values["platform"],
                    ExploreValue::String("rust".into())
                );
                assert_eq!(
                    first.rows[0].values["timestamp"],
                    ExploreValue::Integer(1000)
                );
            }
            ExploreDataset::Logs => {
                assert_eq!(
                    first.rows[0].values["message"],
                    ExploreValue::String(log(1, 2).message)
                );
                assert_eq!(first.rows[0].values["service"], ExploreValue::Null);
                assert_eq!(first.rows[0].values["timestamp"], ExploreValue::Number(1.0));
            }
            ExploreDataset::Spans => {
                assert_eq!(
                    first.rows[0].values["duration_ms"],
                    ExploreValue::Number(2.0)
                );
                assert_eq!(
                    first.rows[0].values["is_segment"],
                    ExploreValue::Bool(false)
                );
            }
            ExploreDataset::Metrics => {
                assert_eq!(
                    first.rows[0].values["trace_id"],
                    ExploreValue::String("01".repeat(16).into())
                );
                assert_eq!(
                    first.rows[0].values["metric_count"],
                    ExploreValue::Integer(1)
                );
            }
        }
        let mut foreign = plan(dataset);
        foreign.project_id = project(2);
        assert_eq!(store.execute(foreign).await.unwrap().rows.len(), 1);
    }
    // Exact full binary text and traced Metrics use the same public typed plan.
    let mut query = plan(ExploreDataset::Metrics);
    query.query.predicates = vec![
        text(ExploreField::Name, "requests\0tail"),
        text(ExploreField::TraceId, &"01".repeat(16).to_uppercase()),
    ];
    let rows = store.execute(query).await.unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values["metric_min"], ExploreValue::Null);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn literal_boolean_filters_null_presence_and_numeric_boundaries() {
    let (fixture, database, store, signals) = setup().await;
    let mut second = log(1, 2);
    second.message = "sql.*%_'\\".into();
    second.service = Some("api".into());
    second.environment = None;
    signals
        .persist_logs(vec![log(1, 1), second, log(2, 3)])
        .await
        .unwrap();
    signals
        .persist_spans(vec![span(1, 1), span(1, 2)])
        .await
        .unwrap();
    let mut query = plan(ExploreDataset::Logs);
    for (op, value, expected) in [
        (ExplorePredicateOp::Contains, ".*%_'\\", 2),
        (ExplorePredicateOp::Contains, "СНЕГ\0tail", 1),
        (ExplorePredicateOp::StartsWith, "SQL", 1),
        (ExplorePredicateOp::EndsWith, "\0tail", 1),
        (
            ExplorePredicateOp::Contains,
            "';DROP TABLE metric.logs;--",
            0,
        ),
    ] {
        query.query.predicates = vec![predicate(
            ExploreField::Message,
            op,
            ExploreValue::String(value.into()),
        )];
        assert_eq!(
            store.execute(query.clone()).await.unwrap().rows.len(),
            expected
        );
    }
    query.query.predicates = vec![text(
        ExploreField::Service,
        "this legacy predicate is ignored",
    )];
    query.query.expression = Some(ExploreExpression::And(vec![
        ExploreExpression::Not(Box::new(ExploreExpression::Predicate(text(
            ExploreField::Service,
            "api",
        )))),
        ExploreExpression::Or(vec![
            ExploreExpression::Predicate(text(ExploreField::Environment, "prod\0east")),
            ExploreExpression::Predicate(text(ExploreField::Environment, "other")),
        ]),
    ]));
    assert_eq!(store.execute(query.clone()).await.unwrap().rows.len(), 1);
    query.query.expression = Some(ExploreExpression::Not(Box::new(
        ExploreExpression::Predicate(predicate(
            ExploreField::Environment,
            ExplorePredicateOp::Present,
            ExploreValue::Bool(true),
        )),
    )));
    assert_eq!(store.execute(query).await.unwrap().rows.len(), 1);
    let mut query = plan(ExploreDataset::Spans);
    query.query.predicates = vec![
        predicate(
            ExploreField::IsSegment,
            ExplorePredicateOp::Present,
            ExploreValue::Bool(true),
        ),
        predicate(
            ExploreField::IsSegment,
            ExplorePredicateOp::Exact,
            ExploreValue::Bool(false),
        ),
        ExplorePredicate {
            field: ExploreField::DurationMs,
            op: ExplorePredicateOp::Range,
            value: Some(ExploreValue::Number(1.0)),
            upper: Some(ExploreValue::Number(2.0)),
        },
    ];
    assert_eq!(store.execute(query.clone()).await.unwrap().rows.len(), 1);
    query.query.predicates = vec![predicate(
        ExploreField::DurationMs,
        ExplorePredicateOp::Greater,
        ExploreValue::Number(1.000001),
    )];
    assert_eq!(store.execute(query.clone()).await.unwrap().rows.len(), 1);
    query.query.predicates = vec![predicate(
        ExploreField::DurationMs,
        ExplorePredicateOp::LessOrEqual,
        ExploreValue::Integer(1),
    )];
    assert_eq!(store.execute(query.clone()).await.unwrap().rows.len(), 1);
    query.query.predicates = vec![predicate(
        ExploreField::Timestamp,
        ExplorePredicateOp::Exact,
        ExploreValue::Integer(1),
    )];
    assert_eq!(store.execute(query).await.unwrap().rows.len(), 2);
    let newline_logs = ["tail", "tail\n", "tail\r\n", "tail\nother"]
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            let mut value = log(1, 100 + index as u128);
            value.message = message.into();
            value
        })
        .collect();
    signals.persist_logs(newline_logs).await.unwrap();
    query = plan(ExploreDataset::Logs);
    query.query.predicates = vec![predicate(
        ExploreField::Message,
        ExplorePredicateOp::EndsWith,
        ExploreValue::String("tail".into()),
    )];
    // Original NUL-tail message plus the two `$`-compatible newline cases.
    assert_eq!(store.execute(query).await.unwrap().rows.len(), 3);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn native_aggregates_percentiles_intervals_and_empty_sources() {
    let (fixture, database, store, signals) = setup().await;
    let mut values = Vec::new();
    for seed in 1..=100 {
        let mut value = span(1, seed);
        value.is_segment = seed % 2 == 0;
        value.started_at_ns = if seed <= 50 {
            1_000_000
        } else {
            61_000_000_000
        };
        values.push(value);
    }
    let mut foreign = span(2, 1);
    foreign.duration_ns = 9_000_000_000;
    values.push(foreign);
    signals.persist_spans(values).await.unwrap();
    let mut query = plan(ExploreDataset::Spans);
    query.query.group_by = vec![ExploreField::OperationClass, ExploreField::IsSegment];
    query.query.interval = Some(ExploreInterval::Minute);
    query.query.aggregates = vec![
        aggregate(ExploreAggregateKind::Count, None, "count"),
        aggregate(
            ExploreAggregateKind::Sum,
            Some(ExploreField::DurationMs),
            "duration",
        ),
        aggregate(
            ExploreAggregateKind::Avg,
            Some(ExploreField::DurationMs),
            "average",
        ),
        aggregate(
            ExploreAggregateKind::P95,
            Some(ExploreField::DurationMs),
            "p95",
        ),
    ];
    let result = store.execute(query.clone()).await.unwrap();
    assert_eq!(result.rows.len(), 4);
    assert!(result.next.is_none());
    assert_eq!(result.rows[0].values["timestamp"], ExploreValue::Integer(0));
    assert_eq!(result.rows[0].values["count"], ExploreValue::Integer(25));
    assert_eq!(
        result.rows[0].values["is_segment"],
        ExploreValue::Bool(false)
    );
    assert_eq!(
        result.rows[0].values["duration"],
        ExploreValue::Number(625.0)
    );
    assert_eq!(result.rows[0].values["average"], ExploreValue::Number(25.0));
    query.query.group_by.clear();
    query.query.interval = None;
    for percentile in [
        ExploreAggregateKind::P50,
        ExploreAggregateKind::P75,
        ExploreAggregateKind::P90,
        ExploreAggregateKind::P95,
        ExploreAggregateKind::P99,
    ] {
        query.query.aggregates = vec![aggregate(percentile, Some(ExploreField::DurationMs), "p")];
        let result = store.execute(query.clone()).await.unwrap();
        let ExploreValue::Number(value) = result.rows[0].values["p"] else {
            panic!("expected numeric percentile")
        };
        assert!((value - (1.0 + 99.0 * percentile.percentile().unwrap())).abs() < 0.00001);
    }
    metrics(
        &database,
        [
            delta(1, "requests", MetricKind::Counter, 2.0),
            delta(1, "queue", MetricKind::Gauge, 5.0),
            delta(1, "latency", MetricKind::Distribution, 8.0),
        ],
    )
    .await;
    query = plan(ExploreDataset::Metrics);
    query.query.group_by = vec![ExploreField::MetricKind];
    query.query.aggregates = vec![
        aggregate(
            ExploreAggregateKind::Sum,
            Some(ExploreField::MetricCount),
            "count",
        ),
        aggregate(
            ExploreAggregateKind::Min,
            Some(ExploreField::MetricMin),
            "min",
        ),
        aggregate(
            ExploreAggregateKind::Max,
            Some(ExploreField::MetricMax),
            "max",
        ),
        aggregate(
            ExploreAggregateKind::Sum,
            Some(ExploreField::MetricMin),
            "sum",
        ),
    ];
    let result = store.execute(query.clone()).await.unwrap();
    assert_eq!(result.rows.len(), 3);
    assert_eq!(
        result.rows[0].values["metric_kind"],
        ExploreValue::String("counter".into())
    );
    assert_eq!(result.rows[0].values["min"], ExploreValue::Null);
    assert_eq!(result.rows[0].values["sum"], ExploreValue::Number(0.0));
    assert_eq!(result.rows[1].values["count"], ExploreValue::Integer(1));
    query.query.predicates = vec![text(ExploreField::Name, "absent")];
    query.query.group_by.clear();
    assert!(store.execute(query).await.unwrap().rows.is_empty());
    // UTC flooring before epoch, at both the negative and exact upper boundary.
    let mut negative = log(1, 500);
    negative.occurred_at_ns = -1_000_000;
    let mut upper = log(1, 501);
    upper.occurred_at_ns = 0;
    let mut fractional = log(1, 502);
    fractional.occurred_at_ns = -1;
    signals
        .persist_logs(vec![negative, upper, fractional])
        .await
        .unwrap();
    query = plan(ExploreDataset::Logs);
    query.query.from = at(-60_000);
    query.query.until = at(1);
    query.query.interval = Some(ExploreInterval::Minute);
    query.query.aggregates = vec![aggregate(ExploreAggregateKind::Count, None, "count")];
    let result = store.execute(query).await.unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        result.rows[0].values["timestamp"],
        ExploreValue::Integer(-60_000)
    );
    assert_eq!(result.rows[1].values["timestamp"], ExploreValue::Integer(0));
    assert_eq!(result.rows[1].values["count"], ExploreValue::Integer(2));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn response_byte_budget_continues_raw_pages_and_rejects_large_aggregate() {
    let (fixture, database, store, _) = setup().await;
    metrics(
        &database,
        (0..25).map(|seed| {
            delta(
                1,
                &format!("{seed:02}{}", "ж".repeat(225_000)),
                MetricKind::Counter,
                1.0,
            )
        }),
    )
    .await;
    let mut query = plan(ExploreDataset::Metrics);
    query.query.limit = 500;
    let first = store.execute(query.clone()).await.unwrap();
    assert!(first.rows.len() < 25);
    assert!(!first.rows.is_empty());
    assert!(first.next.is_some());
    query.query.cursor = first.next;
    let second = store.execute(query.clone()).await.unwrap();
    assert!(second.next.is_none());
    assert_eq!(first.rows.len() + second.rows.len(), 25);
    let ids: std::collections::BTreeSet<_> = first
        .rows
        .iter()
        .chain(second.rows.iter())
        .map(|row| match &row.values["id"] {
            ExploreValue::String(id) => id.clone(),
            _ => panic!("id"),
        })
        .collect();
    assert_eq!(ids.len(), 25);
    query.query.cursor = None;
    query.query.group_by = vec![ExploreField::Name];
    query.query.aggregates = vec![aggregate(ExploreAggregateKind::Count, None, "count")];
    assert_eq!(
        store.execute(query).await,
        Err(ExploreStoreError::InvalidData)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn timeout_cancellation_resets_pool_settings_and_leaves_data_unchanged() {
    let (fixture, database, _, signals) = setup().await;
    signals.persist_logs(vec![log(1, 1)]).await.unwrap();
    let mut policy = fixture.policy.clone();
    policy.max_connections = 1;
    policy.min_connections = 1;
    let isolated = PostgresDatabase::connect(&fixture.uri, &policy)
        .await
        .unwrap();
    let store = PostgresExploreStore::new(isolated.clone());
    let mut blocker = database.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE metric.logs IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let mut query = plan(ExploreDataset::Logs);
    query.maximum_time = Duration::from_millis(100);
    let start = std::time::Instant::now();
    assert_eq!(
        store.execute(query.clone()).await,
        Err(ExploreStoreError::Unavailable)
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    blocker.rollback().await.unwrap();
    assert_eq!(store.execute(query).await.unwrap().rows.len(), 1);
    let row=sqlx::query("SELECT current_setting('statement_timeout') AS timeout,current_setting('transaction_read_only') AS readonly").fetch_one(&isolated.pool).await.unwrap();
    assert_eq!(row.get::<String, _>("timeout"), "10s");
    assert_eq!(row.get::<String, _>("readonly"), "off");
    isolated.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn bigint_numeric_filters_and_sum_promotion_preserve_boundary_values() {
    let (fixture, database, store, _) = setup().await;
    let mut above = delta(1, "above", MetricKind::Counter, 1.0);
    above.aggregate = MetricAggregate::Counter {
        sum: 1.0,
        count: (1_u64 << 53) + 1,
    };
    let mut equal = delta(1, "equal", MetricKind::Counter, 1.0);
    equal.aggregate = MetricAggregate::Counter {
        sum: 1.0,
        count: 1_u64 << 53,
    };
    metrics(&database, [above, equal]).await;
    let mut query = plan(ExploreDataset::Metrics);
    query.query.predicates = vec![predicate(
        ExploreField::MetricCount,
        ExplorePredicateOp::Greater,
        ExploreValue::Number((1_u64 << 53) as f64),
    )];
    let result = store.execute(query).await.unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(
        result.rows[0].values["metric_count"],
        ExploreValue::Integer((1_i64 << 53) + 1)
    );
    let values = ["huge_a", "huge_b"].map(|name| {
        let mut value = delta(1, name, MetricKind::Counter, 1.0);
        value.aggregate = MetricAggregate::Counter {
            sum: 1.0,
            count: i64::MAX as u64,
        };
        value
    });
    metrics(&database, values).await;
    query = plan(ExploreDataset::Metrics);
    query.query.predicates = vec![predicate(
        ExploreField::Name,
        ExplorePredicateOp::StartsWith,
        ExploreValue::String("huge_".into()),
    )];
    query.query.aggregates = vec![
        aggregate(
            ExploreAggregateKind::Sum,
            Some(ExploreField::MetricCount),
            "sum",
        ),
        aggregate(
            ExploreAggregateKind::Min,
            Some(ExploreField::MetricCount),
            "min",
        ),
        aggregate(
            ExploreAggregateKind::Avg,
            Some(ExploreField::MetricCount),
            "avg",
        ),
    ];
    let result = store.execute(query).await.unwrap();
    assert_eq!(
        result.rows[0].values["sum"],
        ExploreValue::Number(i64::MAX as f64 * 2.0)
    );
    assert_eq!(
        result.rows[0].values["min"],
        ExploreValue::Integer(i64::MAX)
    );
    assert_eq!(
        result.rows[0].values["avg"],
        ExploreValue::Number(i64::MAX as f64)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn compiled_metric_pages_use_native_timeline_and_exact_name_indexes() {
    use sqlx::Execute;
    let (fixture, database, store, _) = setup().await;
    metrics(
        &database,
        (0..3000).map(|seed| delta(1, &format!("requests.{seed:04}"), MetricKind::Counter, 1.0)),
    )
    .await;
    sqlx::query("ANALYZE metric.metric_buckets")
        .execute(&database.pool)
        .await
        .unwrap();
    for name in [None, Some("requests.1234")] {
        let mut query = plan(ExploreDataset::Metrics);
        query.query.limit = 10;
        if let Some(name) = name {
            query.query.predicates.push(text(ExploreField::Name, name));
        }
        let mut compiled = compile(&query).unwrap();
        let explain = format!(
            "EXPLAIN (ANALYZE,FORMAT JSON) {}",
            compiled.query.sql().as_str()
        );
        let args = compiled.query.build().take_arguments().unwrap().unwrap();
        let row = sqlx::query_with(sqlx::AssertSqlSafe(explain), args)
            .fetch_one(&database.pool)
            .await
            .unwrap();
        let value: sqlx::types::Json<serde_json::Value> = row.get(0);
        let native = value.0.to_string();
        assert!(
            native.contains(if name.is_some() {
                "metric_project_name_time"
            } else {
                "metric_explore_timeline"
            }),
            "native plan: {native}"
        );
        assert_eq!(
            store.execute(query).await.unwrap().rows.len(),
            if name.is_some() { 1 } else { 10 }
        );
    }
    database.close().await;
    fixture.cleanup().await;
}
