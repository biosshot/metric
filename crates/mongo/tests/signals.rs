use metric_domain::{ProjectId, SecretBytes};
use metric_mongo::{MongoProjectStore, SignalRetention};
use mongodb::{Client, bson::doc};
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn span_rebuild_preserves_all_intersected_hours() {
    let uri =
        std::env::var("METRIC_TEST_MONGODB_URI").expect("METRIC_TEST_MONGODB_URI is required");
    let client = Client::with_uri_str(uri).await.unwrap();
    client
        .database("admin")
        .run_command(doc! {"ping":1})
        .await
        .unwrap();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let database = client.database(&format!(
        "metric_pg_span_repair_{}_{}",
        std::process::id(),
        nonce
    ));
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    let result = async {
        control.bootstrap_or_validate().await.unwrap();
        metric_testkit::signal_conformance::whole_hour_rebuild(
            &control.signal_store_with_retention(SignalRetention::default()),
            ProjectId::new(42).unwrap(),
        )
        .await
    }
    .await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}
