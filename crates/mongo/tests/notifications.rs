use metric_domain::{ProjectId, SecretBytes};
use metric_mongo::MongoProjectStore;
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::test]
#[ignore = "requires Docker Mongo configured by METRIC_TEST_MONGODB_URI"]
async fn terminal_history_remains_readable_and_duplicates_immutable() {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap();
    let client = mongodb::Client::with_uri_str(uri).await.unwrap();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let database = client.database(&format!(
        "metric_pg_notification_conformance_{}_{nonce}",
        std::process::id()
    ));
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await.unwrap();
    metric_testkit::notification_conformance::terminal_history(
        &control.notification_store(),
        ProjectId::new(42).unwrap(),
        ProjectId::new(43).unwrap(),
    )
    .await;
    database.drop().await.unwrap();
}
