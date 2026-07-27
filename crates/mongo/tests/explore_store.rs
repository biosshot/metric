use std::{error::Error, sync::Arc};

use metric_domain::{
    ProjectId, SecretBytes, Timestamp,
    explore::{
        ExploreAggregate, ExploreAggregateKind, ExploreDataset, ExplorePlan, ExploreQuery,
        ExploreValue, normalize_query,
    },
    signals::{LogId, LogRecord, LogSeverity, SignalBody},
};
use metric_mongo::{MongoProjectStore, SignalRetention};
use metric_ports::{ExploreStore, SignalStore};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn explore_remains_project_scoped_during_ingest_and_ttl_deletion() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let signals = Arc::new(control.signal_store_with_retention(SignalRetention::default()));
    let explore = Arc::new(control.explore_store());
    let project = ProjectId::new(42)?;
    let other = ProjectId::new(43)?;
    let records = (0..100)
        .map(|index| log(project, index))
        .collect::<Vec<_>>();
    let writer = {
        let signals = Arc::clone(&signals);
        tokio::spawn(async move { signals.persist_logs(records).await })
    };

    let during = explore.execute(count_plan(project)).await?;
    let during_count = count(&during)?;
    assert!(during_count <= 100);
    let outcomes = writer.await??;
    assert_eq!(outcomes.len(), 100);
    signals.persist_logs(vec![log(other, 50)]).await?;

    assert_eq!(count(&explore.execute(count_plan(project)).await?)?, 100);
    assert_eq!(count(&explore.execute(count_plan(other)).await?)?, 1);
    let analytic = explore.execute(analytic_plan(project)).await?;
    assert_eq!(analytic.rows.len(), 1);
    assert_eq!(
        analytic.rows[0].values.get("level"),
        Some(&ExploreValue::String("info".into()))
    );
    assert_eq!(
        analytic.rows[0].values.get("count"),
        Some(&ExploreValue::Integer(100))
    );
    assert!(matches!(
        analytic.rows[0].values.get("p95_timestamp"),
        Some(ExploreValue::Number(value)) if value.is_finite()
    ));

    database
        .collection::<mongodb::bson::Document>("logs")
        .delete_many(doc! {
            "p": project.get(),
            "o": { "$lt": 1_700_000_020_000_000_000_i64 },
        })
        .await?;
    assert_eq!(count(&explore.execute(count_plan(project)).await?)?, 80);
    Ok(())
}

fn analytic_plan(project_id: ProjectId) -> ExplorePlan {
    let query = ExploreQuery {
        dataset: ExploreDataset::Logs,
        from: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
        until: Timestamp::from_unix_millis(1_700_000_200_000).unwrap(),
        predicates: Vec::new(),
        aggregates: vec![
            ExploreAggregate {
                kind: ExploreAggregateKind::Count,
                field: None,
                alias: "count".into(),
            },
            ExploreAggregate {
                kind: ExploreAggregateKind::P95,
                field: Some(metric_domain::explore::ExploreField::Timestamp),
                alias: "p95_timestamp".into(),
            },
        ],
        group_by: vec![metric_domain::explore::ExploreField::Level],
        interval: Some(metric_domain::explore::ExploreInterval::Hour),
        cursor: None,
        limit: 50,
    };
    ExplorePlan {
        project_id,
        normalized: normalize_query(&query),
        query,
        cost: 1,
        maximum_time: std::time::Duration::from_secs(5),
    }
}

fn count_plan(project_id: ProjectId) -> ExplorePlan {
    let query = ExploreQuery {
        dataset: ExploreDataset::Logs,
        from: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
        until: Timestamp::from_unix_millis(1_700_000_200_000).unwrap(),
        predicates: Vec::new(),
        aggregates: vec![ExploreAggregate {
            kind: ExploreAggregateKind::Count,
            field: None,
            alias: "count".into(),
        }],
        group_by: Vec::new(),
        interval: None,
        cursor: None,
        limit: 50,
    };
    ExplorePlan {
        project_id,
        normalized: normalize_query(&query),
        query,
        cost: 1,
        maximum_time: std::time::Duration::from_secs(5),
    }
}

fn count(result: &metric_domain::explore::ExploreResult) -> Result<u64, Box<dyn Error>> {
    if result.rows.is_empty() {
        return Ok(0);
    }
    match result.rows.first().and_then(|row| row.values.get("count")) {
        Some(ExploreValue::Integer(value)) => Ok(u64::try_from(*value)?),
        value => Err(format!("unexpected count {value:?}").into()),
    }
}

fn log(project_id: ProjectId, index: i64) -> LogRecord {
    let occurred_at_ns = 1_700_000_000_000_000_000_i64 + index * 1_000_000_000;
    let received_at = Timestamp::from_unix_millis(1_700_000_000_000 + index * 1_000).unwrap();
    let body = format!(r#"{{"body":"log {index}","severity_text":"info"}}"#).into_bytes();
    LogRecord {
        id: LogId::deterministic(project_id, received_at, occurred_at_ns, &body),
        project_id,
        received_at,
        occurred_at_ns,
        severity: LogSeverity::Info,
        message: format!("log {index}").into(),
        trace_id: None,
        span_id: None,
        environment: Some("production".into()),
        release: Some("1.0.0".into()),
        service: Some("api".into()),
        body: SignalBody::new(body),
    }
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&retryWrites=false&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "metric_phase32_explore_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
