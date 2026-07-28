use std::{
    collections::BTreeMap,
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use metric_domain::{
    ProjectId, SecretBytes, Timestamp,
    explore::{
        ExploreAggregate, ExploreAggregateKind, ExploreDataset, ExploreField, ExplorePlan,
        ExploreQuery, ExploreValue, normalize_query,
    },
    metrics::{MetricAggregate, MetricDelta, MetricDeltaBatch, MetricKind, MetricSeries},
};
use metric_mongo::{MetricRetention, MongoProjectStore};
use metric_ports::{ExploreStore, MetricStore, SignalStoreError};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn metric_buckets_cardinality_retention_and_archive_are_bounded() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let store = control.metric_store(MetricRetention {
        days: 30,
        max_series_per_project: 2,
        archive: false,
    });
    let project = ProjectId::new(42)?;
    store
        .persist_metrics(batch(project, "requests", MetricKind::Counter, 2.0))
        .await?;
    store
        .persist_metrics(batch(project, "latency", MetricKind::Distribution, 12.5))
        .await?;
    let rejected = store
        .persist_metrics(batch(project, "queue", MetricKind::Gauge, 7.0))
        .await;
    assert_eq!(rejected, Err(SignalStoreError::Capacity));
    let buckets = database.collection::<mongodb::bson::Document>("metric_buckets");
    assert_eq!(
        buckets.count_documents(doc! { "p": project.get() }).await?,
        2
    );
    assert_eq!(
        buckets
            .count_documents(doc! { "p": project.get(), "z": { "$type": "date" } })
            .await?,
        2
    );
    let query = ExploreQuery {
        dataset: ExploreDataset::Metrics,
        from: Timestamp::from_unix_millis(1_699_999_000_000)?,
        until: Timestamp::from_unix_millis(1_700_001_000_000)?,
        predicates: Vec::new(),
        aggregates: vec![ExploreAggregate {
            kind: ExploreAggregateKind::Sum,
            field: Some(ExploreField::MetricCount),
            alias: "samples".into(),
        }],
        group_by: vec![ExploreField::MetricKind],
        interval: None,
        cursor: None,
        limit: 10,
    };
    let explored = control
        .explore_store()
        .execute(ExplorePlan {
            project_id: project,
            normalized: normalize_query(&query),
            query,
            cost: 1,
            maximum_time: std::time::Duration::from_secs(5),
        })
        .await?;
    assert_eq!(explored.rows.len(), 2);
    assert!(explored.rows.iter().all(|row| {
        row.values.get("samples") == Some(&ExploreValue::Integer(1))
            && matches!(
                row.values.get("metric_kind"),
                Some(ExploreValue::String(kind))
                    if matches!(kind.as_ref(), "counter" | "distribution")
            )
    }));

    let archived_project = ProjectId::new(43)?;
    control
        .metric_store(MetricRetention {
            days: 30,
            max_series_per_project: 2,
            archive: true,
        })
        .persist_metrics(batch(
            archived_project,
            "requests",
            MetricKind::Counter,
            1.0,
        ))
        .await?;
    assert_eq!(
        buckets
            .count_documents(doc! {
                "p": archived_project.get(),
                "h": { "$type": "date" },
                "z": { "$exists": false },
            })
            .await?,
        1
    );
    let index_names = buckets.list_index_names().await?;
    assert!(index_names.iter().any(|name| name == "metric_retention"));
    assert!(index_names.iter().any(|name| name == "metric_archive_due"));
    Ok(())
}

fn batch(
    project_id: ProjectId,
    name: &'static str,
    kind: MetricKind,
    value: f64,
) -> MetricDeltaBatch {
    let mut batch = MetricDeltaBatch {
        source_measurements: 1,
        ..MetricDeltaBatch::default()
    };
    batch.push(MetricDelta {
        series: MetricSeries {
            project_id,
            name: name.into(),
            kind,
            unit: "none".into(),
            tags: BTreeMap::new(),
        },
        bucket_start: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
        bucket_width_seconds: 60,
        received_at: Timestamp::from_unix_millis(1_700_000_000_001).unwrap(),
        trace_id: None,
        aggregate: MetricAggregate::from_measurement(kind, value),
    });
    batch
}

async fn test_database() -> Result<Database, Box<dyn Error>> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI")
        .unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_owned());
    let client = Client::with_uri_str(&uri).await?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(client.database(&format!("metric_metrics_test_{nonce}")))
}
