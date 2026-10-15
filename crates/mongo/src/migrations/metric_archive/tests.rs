use super::*;
use crate::{MongoProjectStore, SCHEMA_GENERATION, SCHEMA_ID};
use metric_domain::{
    ProjectId, SecretBytes, Timestamp,
    archive::{ArchiveKind, ArchiveSourceId},
    blob::{BlobChecksum, BlobKind, BlobObject},
};
use metric_ports::{
    ArchiveClaimRequest, ArchiveCompleteRequest, ArchiveSourceCommitRequest, ArchiveStore,
};
use mongodb::{
    bson::{Binary, DateTime, spec::BinarySubtype},
    options::IndexOptions,
};
use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

#[tokio::test]
#[ignore = "requires Docker MongoDB"]
async fn generation_21_archive_schema_upgrade_preserves_data_and_replays_partial_steps() {
    let database = database("upgrade").await;
    let result = upgrade(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}
async fn upgrade(database: &Database) -> Result<(), Box<dyn Error>> {
    let store = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 16);
    store.bootstrap_or_validate().await?;
    install(
        database,
        "archive_manifests",
        archive_manifest_validator_v21(),
    )
    .await?;
    install(database, "metric_buckets", metric_validator_v21()).await?;
    let metrics = database.collection::<Document>("metric_buckets");
    metrics.drop_index("metric_archive_retention").await?;
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let mut ordinary = metric(1, now);
    ordinary.insert("z", DateTime::from_millis(now + 86_400_000));
    let mut held = metric(2, now);
    held.insert("h", DateTime::from_millis(now - 86_400_000));
    metrics
        .insert_many([ordinary.clone(), held.clone()])
        .await?;
    let other_collection = database.collection::<Document>("dsn_maps");
    let other = doc! {"_id":binary(5,16),"project_id":1,"target_key":binary(6,16),"dsn":"https://synthetic@example.com/42"};
    other_collection.insert_one(other.clone()).await?;
    // The migration does not read or rewrite any user collection. Preserve a
    // real ordinary/held metric and all pre-existing index names exactly.
    let indexes_before = metrics.list_index_names().await?;
    let others_before = other_collection.count_documents(doc! {}).await?;
    let marker = database.collection::<Document>("schema_meta");
    marker
        .update_one(
            doc! {"_id":SCHEMA_ID},
            doc! {"$set":{"generation":21,"state":"complete"}},
        )
        .await?;
    let checkpoint = MigrationCheckpoint::default();
    // Replay the first DDL statement before its checkpoint to model interrupted
    // publication; startup must recognize and finish this intermediate shape.
    for step in 0..1 {
        METRIC_ARCHIVE_MIGRATION.preflight(database).await?;
        for _ in 0..2 {
            let batch = METRIC_ARCHIVE_MIGRATION
                .run_batch(database, step, &checkpoint)
                .await?;
            assert_eq!(batch.processed_records, 0);
        }
    }
    assert_eq!(
        validator(database, "metric_buckets").await?,
        metric_validator_v21()
    );
    assert!(archive_index(database).await?.is_none());
    store.bootstrap_or_validate().await?;
    store.bootstrap_or_validate().await?;
    assert_eq!(
        marker
            .find_one(doc! {"_id":SCHEMA_ID})
            .await?
            .unwrap()
            .get_i32("generation"),
        Ok(SCHEMA_GENERATION)
    );
    assert_eq!(
        metrics.find_one(doc! {"_id":binary(1,16)}).await?.unwrap(),
        ordinary
    );
    assert_eq!(
        metrics.find_one(doc! {"_id":binary(2,16)}).await?.unwrap(),
        held
    );
    assert_eq!(metrics.count_documents(doc! {}).await?, 2);
    assert_eq!(
        other_collection.count_documents(doc! {}).await?,
        others_before
    );
    assert_eq!(
        other_collection
            .find_one(doc! {"_id":binary(5,16)})
            .await?
            .unwrap(),
        other
    );
    let indexes_after = metrics.list_index_names().await?;
    assert_eq!(indexes_after.len(), indexes_before.len() + 1);
    assert!(
        indexes_before
            .iter()
            .all(|name| indexes_after.contains(name))
    );
    METRIC_ARCHIVE_MIGRATION.verify(database).await?;
    for step in 0..3 {
        METRIC_ARCHIVE_MIGRATION.preflight(database).await?;
        METRIC_ARCHIVE_MIGRATION
            .run_batch(database, step, &checkpoint)
            .await?;
    }
    // Existing held data becomes archivable after the schema-only upgrade.
    let at = |v| Timestamp::from_unix_millis(v).unwrap();
    let archives = store.archive_store(crate::EventCodecConfig::default());
    let batch = archives
        .claim(ArchiveClaimRequest {
            kind: ArchiveKind::Metric,
            now: at(now),
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
        })
        .await?
        .unwrap();
    assert_eq!(batch.source_ids, vec![ArchiveSourceId::Metric([2; 16])]);
    archives
        .complete(ArchiveCompleteRequest {
            segment_id: batch.segment_id,
            object: BlobObject {
                key: batch.object_key,
                kind: BlobKind::MetricArchive,
                size: 123,
                checksum: BlobChecksum::from_bytes([9; 32]),
                created_at: at(now),
            },
            completed_at: at(now),
        })
        .await?;
    assert_eq!(
        archives
            .commit_sources(ArchiveSourceCommitRequest {
                kind: ArchiveKind::Metric,
                segment_id: batch.segment_id,
                source_ids: batch.source_ids,
                expire_at: at(now + 86_400_000)
            })
            .await?,
        1
    );
    metric_testkit::archive_conformance::metric_archive_lifecycle(
        &store.metric_store(crate::MetricRetention {
            days: 1,
            max_series_per_project: 16,
            archive: true,
        }),
        &archives,
        ProjectId::new(42)?,
        at(now),
        at(now + 86_400_000),
    )
    .await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker MongoDB"]
async fn metric_archive_migration_blocks_unknown_validator_and_conflicting_index() {
    let database = database("blocked").await;
    let result = blocked(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}
async fn blocked(database: &Database) -> Result<(), Box<dyn Error>> {
    let store = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 16);
    store.bootstrap_or_validate().await?;
    let original = validator(database, "archive_manifests").await?;
    install(
        database,
        "metric_buckets",
        doc! {"$jsonSchema":{"bsonType":"object"}},
    )
    .await?;
    assert!(matches!(
        METRIC_ARCHIVE_MIGRATION.preflight(database).await,
        Err(MigrationAttemptError::Blocked {
            code: "metric_archive_validator_unknown",
            ..
        })
    ));
    assert_eq!(validator(database, "archive_manifests").await?, original);
    install(database, "metric_buckets", metric_validator_v21()).await?;
    let collection = database.collection::<Document>("metric_buckets");
    collection.drop_index("metric_archive_retention").await?;
    collection
        .create_index(
            IndexModel::builder()
                .keys(doc! {"x":1})
                .options(
                    IndexOptions::builder()
                        .name("metric_archive_retention".to_owned())
                        .build(),
                )
                .build(),
        )
        .await?;
    assert!(matches!(
        METRIC_ARCHIVE_MIGRATION.preflight(database).await,
        Err(MigrationAttemptError::Blocked {
            code: "metric_archive_expiry_index_conflict",
            ..
        })
    ));
    assert_eq!(validator(database, "archive_manifests").await?, original);
    Ok(())
}

pub(super) fn metric(seed: u8, now: i64) -> Document {
    doc! {"_id":binary(seed,16),"p":42,"s":binary(seed,16),"n":format!("synthetic-{seed}"),"k":1,"u":"none","a":{},"t":DateTime::from_millis(now),"w":60_i64,"r":DateTime::from_millis(now),"s0":2.0,"c":1_i64}
}
fn binary(seed: u8, len: usize) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: vec![seed; len],
    }
}
async fn database(label: &str) -> Database {
    let client = mongodb::Client::with_uri_str(std::env::var("METRIC_TEST_MONGODB_URI").unwrap())
        .await
        .unwrap();
    client.database(&format!(
        "metric_pg_metric_archive_{label}_{}",
        mongodb::bson::oid::ObjectId::new()
    ))
}
