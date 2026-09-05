use metric_domain::notifications::DEFAULT_TELEGRAM_API_BASE;
use mongodb::{
    Database,
    bson::{Bson, Document, doc},
};

use super::{
    MigrationAttemptError, MigrationBatch, MigrationCheckpoint, MigrationDocumentLimits,
    MigrationFuture, MongoMigration, RecordErrorPolicy, load_bounded_document_page,
};
use crate::notifications::{
    destination_expand_validator_v20, destination_validator, destination_validator_v19,
};

const DESTINATIONS: &str = "notification_destinations";
const BATCH_DOCUMENTS: u32 = 256;
const BATCH_BYTES: usize = 1024 * 1024;

pub(crate) static TELEGRAM_CONFIGURATION_MIGRATION: TelegramConfigurationMigration =
    TelegramConfigurationMigration;

pub(crate) struct TelegramConfigurationMigration;

impl MongoMigration for TelegramConfigurationMigration {
    fn source_generation(&self) -> i32 {
        19
    }

    fn target_generation(&self) -> i32 {
        20
    }

    fn name(&self) -> &'static str {
        "telegram_destination_configuration"
    }

    fn step_count(&self) -> u32 {
        3
    }

    fn step_name(&self, step: u32) -> &'static str {
        match step {
            0 => "expand_destination_validator",
            1 => "backfill_telegram_configuration",
            2 => "contract_destination_validator",
            _ => "invalid_telegram_migration_step",
        }
    }

    fn record_error_policy(&self, _step: u32) -> RecordErrorPolicy {
        RecordErrorPolicy::Strict
    }

    fn preflight<'a>(
        &'a self,
        database: &'a Database,
    ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>> {
        Box::pin(async move {
            let validator = collection_validator(database).await?;
            if validator != destination_validator_v19()
                && validator != destination_expand_validator_v20()
                && validator != destination_validator()
            {
                return Err(MigrationAttemptError::blocked(
                    "telegram_destination_validator_unknown",
                ));
            }
            Ok(())
        })
    }

    fn run_batch<'a>(
        &'a self,
        database: &'a Database,
        step: u32,
        checkpoint: &'a MigrationCheckpoint,
    ) -> MigrationFuture<'a, Result<MigrationBatch, MigrationAttemptError>> {
        Box::pin(async move {
            match step {
                0 => {
                    install_validator(database, destination_expand_validator_v20()).await?;
                    Ok(MigrationBatch::complete(0))
                }
                1 => backfill_telegram_configuration(database, checkpoint).await,
                2 => {
                    install_validator(database, destination_validator()).await?;
                    Ok(MigrationBatch::complete(0))
                }
                _ => Err(MigrationAttemptError::blocked(
                    "telegram_destination_step_unknown",
                )),
            }
        })
    }

    fn verify<'a>(
        &'a self,
        database: &'a Database,
    ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>> {
        Box::pin(async move {
            if collection_validator(database).await? != destination_validator() {
                return Err(MigrationAttemptError::blocked(
                    "telegram_destination_validator_not_contracted",
                ));
            }
            let collection = database.collection::<Document>(DESTINATIONS);
            let missing = collection
                .find_one(doc! { "k": "telegram", "g": { "$exists": false } })
                .projection(doc! { "_id": 1 })
                .await
                .map_err(MigrationAttemptError::from_mongo)?;
            let misplaced = collection
                .find_one(doc! { "k": { "$ne": "telegram" }, "g": { "$exists": true } })
                .projection(doc! { "_id": 1 })
                .await
                .map_err(MigrationAttemptError::from_mongo)?;
            if missing.is_some() || misplaced.is_some() {
                return Err(MigrationAttemptError::blocked(
                    "telegram_destination_backfill_incomplete",
                ));
            }
            Ok(())
        })
    }
}

async fn backfill_telegram_configuration(
    database: &Database,
    checkpoint: &MigrationCheckpoint,
) -> Result<MigrationBatch, MigrationAttemptError> {
    let collection = database.collection::<Document>(DESTINATIONS);
    let limits = MigrationDocumentLimits::new(BATCH_DOCUMENTS, BATCH_BYTES)
        .expect("Telegram migration batch limits are bounded");
    let page = load_bounded_document_page(
        &collection,
        doc! { "k": "telegram", "g": { "$exists": false } },
        checkpoint.cursor.as_ref(),
        doc! { "k": 1 },
        limits,
    )
    .await?;
    if page.documents.is_empty() {
        return Ok(MigrationBatch::complete(0));
    }

    let ids = page
        .documents
        .iter()
        .filter_map(|document| document.get("_id").cloned())
        .collect::<Vec<Bson>>();
    if ids.len() != page.documents.len() {
        return Err(MigrationAttemptError::blocked(
            "telegram_destination_id_missing",
        ));
    }
    collection
        .update_many(
            doc! {
                "_id": { "$in": ids },
                "k": "telegram",
                "g": { "$exists": false },
            },
            doc! { "$set": { "g": { "h": DEFAULT_TELEGRAM_API_BASE } } },
        )
        .await
        .map_err(MigrationAttemptError::from_mongo)?;

    let processed = u64::try_from(page.documents.len()).unwrap_or(u64::MAX);
    if page.exhausted {
        Ok(MigrationBatch::complete(processed))
    } else {
        page.next_cursor.map_or_else(
            || {
                Err(MigrationAttemptError::blocked(
                    "telegram_destination_cursor_missing",
                ))
            },
            |cursor| Ok(MigrationBatch::more(cursor, processed)),
        )
    }
}

async fn install_validator(
    database: &Database,
    validator: Document,
) -> Result<(), MigrationAttemptError> {
    database
        .run_command(doc! {
            "collMod": DESTINATIONS,
            "validator": validator,
            "validationLevel": "strict",
            "validationAction": "error",
        })
        .await
        .map(|_| ())
        .map_err(MigrationAttemptError::from_mongo)
}

async fn collection_validator(database: &Database) -> Result<Document, MigrationAttemptError> {
    let response = database
        .run_command(doc! {
            "listCollections": 1,
            "filter": { "name": DESTINATIONS },
            "nameOnly": false,
        })
        .await
        .map_err(MigrationAttemptError::from_mongo)?;
    response
        .get_document("cursor")
        .ok()
        .and_then(|cursor| cursor.get_array("firstBatch").ok())
        .and_then(|batch| batch.first())
        .and_then(Bson::as_document)
        .and_then(|collection| collection.get_document("options").ok())
        .and_then(|options| options.get_document("validator").ok())
        .cloned()
        .ok_or_else(|| MigrationAttemptError::blocked("telegram_destination_validator_missing"))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use metric_domain::SecretBytes;
    use mongodb::{
        Client,
        bson::{Binary, DateTime, oid::ObjectId, spec::BinarySubtype},
    };

    use super::*;
    use crate::{MongoProjectStore, SCHEMA_ID, SchemaMigrationProgress};

    #[tokio::test]
    #[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
    async fn generation_19_backfills_telegram_destinations_in_bounded_pages() {
        let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
            "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
        });
        let client = Client::with_uri_str(uri).await.unwrap();
        let database = client.database(&format!(
            "metric_telegram_migration_test_{}",
            ObjectId::new().to_hex()
        ));
        let store =
            MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 16);
        store.bootstrap_or_validate().await.unwrap();

        install_validator(&database, destination_validator_v19())
            .await
            .unwrap();
        let destinations = database.collection::<Document>(DESTINATIONS);
        let legacy = (0_u64..513).map(|index| {
            doc! {
                "_id": Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: telegram_id(index),
                },
                "p": 1_i32,
                "k": "telegram",
                "u": format!("-100{index:010}"),
                "s": Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: vec![u8::try_from(index % 251).unwrap(); 48],
                },
                "e": true,
                "c": DateTime::from_millis(1_000),
                "m": DateTime::from_millis(2_000),
            }
        });
        destinations.insert_many(legacy).await.unwrap();
        let webhook = doc! {
            "_id": Binary { subtype: BinarySubtype::Generic, bytes: vec![255; 16] },
            "p": 1_i32,
            "k": "webhook",
            "u": "https://example.com/hook",
            "s": Binary { subtype: BinarySubtype::Generic, bytes: vec![3; 48] },
            "e": true,
            "c": DateTime::from_millis(1_000),
            "m": DateTime::from_millis(2_000),
        };
        destinations.insert_one(webhook.clone()).await.unwrap();
        let rules = database.collection::<Document>("alert_rules");
        let rule = doc! {
            "_id": Binary { subtype: BinarySubtype::Generic, bytes: vec![91; 16] },
            "p": 1_i32,
            "n": "migration reference",
            "e": true,
            "k": ["new_issue"],
            "d": [Binary { subtype: BinarySubtype::Generic, bytes: telegram_id(0) }],
            "o": 0_i64,
            "b": 100_i64,
            "sc": 0_i64,
            "tm": false,
            "c": DateTime::from_millis(1_000),
            "u": DateTime::from_millis(2_000),
        };
        rules.insert_one(rule.clone()).await.unwrap();
        database
            .collection::<Document>("schema_meta")
            .update_one(
                doc! { "_id": SCHEMA_ID },
                doc! { "$set": { "generation": 19_i32, "state": "complete" } },
            )
            .await
            .unwrap();

        let reports = Mutex::new(Vec::new());
        store
            .bootstrap_or_migrate(&|progress| reports.lock().unwrap().push(progress))
            .await
            .unwrap();
        store.bootstrap_or_validate().await.unwrap();

        assert_eq!(
            destinations
                .count_documents(doc! { "k": "telegram", "g.h": DEFAULT_TELEGRAM_API_BASE })
                .await
                .unwrap(),
            513
        );
        assert_eq!(
            destinations
                .count_documents(doc! { "k": "telegram", "g": { "$exists": false } })
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            destinations
                .find_one(doc! { "_id": webhook.get("_id").unwrap().clone() })
                .await
                .unwrap()
                .unwrap(),
            webhook
        );
        assert_eq!(
            rules
                .find_one(doc! { "_id": rule.get("_id").unwrap().clone() })
                .await
                .unwrap()
                .unwrap(),
            rule
        );
        let first = destinations
            .find_one(
                doc! { "_id": Binary { subtype: BinarySubtype::Generic, bytes: telegram_id(0) } },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.get_binary_generic("s").unwrap().as_slice(), &[0; 48]);
        let marker = database
            .collection::<Document>("schema_meta")
            .find_one(doc! { "_id": SCHEMA_ID })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(marker.get_i32("generation"), Ok(20));
        assert_eq!(marker.get_str("state"), Ok("complete"));
        assert!(
            reports
                .lock()
                .unwrap()
                .iter()
                .any(|progress| matches!(progress, SchemaMigrationProgress::Running { processed_records, .. } if *processed_records >= 256))
        );
        database.drop().await.unwrap();
    }

    fn telegram_id(index: u64) -> Vec<u8> {
        let mut id = vec![0; 16];
        id[8..].copy_from_slice(&index.to_be_bytes());
        id
    }
}
