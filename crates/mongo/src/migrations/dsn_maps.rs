use super::{
    MigrationAttemptError, MigrationBatch, MigrationCheckpoint, MigrationFuture, MongoMigration,
    RecordErrorPolicy,
};
use futures_util::TryStreamExt;
use mongodb::{
    Database,
    bson::{Document, doc},
};

pub(crate) static DSN_MAPS_MIGRATION: DsnMapsMigration = DsnMapsMigration;
pub(crate) struct DsnMapsMigration;

impl MongoMigration for DsnMapsMigration {
    fn source_generation(&self) -> i32 {
        20
    }
    fn target_generation(&self) -> i32 {
        21
    }
    fn name(&self) -> &'static str {
        "existing_sentry_dsns"
    }
    fn step_count(&self) -> u32 {
        1
    }
    fn step_name(&self, _step: u32) -> &'static str {
        "create_dsn_maps"
    }
    fn record_error_policy(&self, _step: u32) -> RecordErrorPolicy {
        RecordErrorPolicy::Strict
    }

    fn preflight<'a>(
        &'a self,
        _database: &'a Database,
    ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>> {
        Box::pin(async { Ok(()) })
    }

    fn run_batch<'a>(
        &'a self,
        database: &'a Database,
        _step: u32,
        _checkpoint: &'a MigrationCheckpoint,
    ) -> MigrationFuture<'a, Result<MigrationBatch, MigrationAttemptError>> {
        Box::pin(async move {
            let exists = database
                .list_collection_names()
                .await
                .map_err(MigrationAttemptError::from_mongo)?
                .iter()
                .any(|name| name == "dsn_maps");
            if !exists {
                database
                    .run_command(
                        doc! { "create": "dsn_maps", "validator": crate::dsn_maps::validator(),
                        "validationLevel": "strict", "validationAction": "error" },
                    )
                    .await
                    .map_err(MigrationAttemptError::from_mongo)?;
            }
            crate::dsn_maps::create_index(database)
                .await
                .map_err(MigrationAttemptError::from_mongo)?;
            Ok(MigrationBatch::complete(0))
        })
    }

    fn verify<'a>(
        &'a self,
        database: &'a Database,
    ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>> {
        Box::pin(async move {
            let mut cursor = database
                .list_collections()
                .filter(doc! { "name": "dsn_maps" })
                .await
                .map_err(MigrationAttemptError::from_mongo)?;
            let collection = cursor
                .try_next()
                .await
                .map_err(MigrationAttemptError::from_mongo)?
                .ok_or_else(|| MigrationAttemptError::blocked("dsn_maps_missing"))?;
            if collection.options.validator != Some(crate::dsn_maps::validator()) {
                return Err(MigrationAttemptError::blocked(
                    "dsn_maps_validator_mismatch",
                ));
            }
            let mut indexes = database
                .collection::<Document>("dsn_maps")
                .list_indexes()
                .await
                .map_err(MigrationAttemptError::from_mongo)?;
            while let Some(index) = indexes
                .try_next()
                .await
                .map_err(MigrationAttemptError::from_mongo)?
            {
                if index.keys == doc! { "project_id": 1, "target_key": 1 }
                    && index.options.is_some_and(|options| {
                        options.unique == Some(true)
                            && options.name.as_deref() == Some("dsn_map_project")
                    })
                {
                    return Ok(());
                }
            }
            Err(MigrationAttemptError::blocked("dsn_maps_index_missing"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires MongoDB"]
    async fn infrastructure_generation_20_adds_only_dsn_maps_and_resumes() {
        let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
            "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin".to_owned()
        });
        let client = mongodb::Client::with_uri_str(uri).await.unwrap();
        let database = client.database(&format!(
            "metric_dsn_migration_{}",
            mongodb::bson::oid::ObjectId::new()
        ));
        let store = crate::MongoProjectStore::from_database(
            database.clone(),
            metric_domain::SecretBytes::new([7; 32]),
            16,
        );
        store.bootstrap_or_validate().await.unwrap();
        database
            .collection::<Document>("dsn_maps")
            .drop()
            .await
            .unwrap();
        database
            .collection::<Document>("schema_meta")
            .update_one(
                doc! { "_id": "metric.schema" },
                doc! { "$set": { "generation": 20 } },
            )
            .await
            .unwrap();
        store.bootstrap_or_validate().await.unwrap();
        // Repeating the idempotent DDL step models a crash before its checkpoint.
        DSN_MAPS_MIGRATION
            .run_batch(&database, 0, &MigrationCheckpoint::default())
            .await
            .unwrap();
        store.bootstrap_or_validate().await.unwrap();
        assert_eq!(
            database
                .collection::<Document>("dsn_maps")
                .count_documents(doc! {})
                .await
                .unwrap(),
            0
        );
        let marker = database
            .collection::<Document>("schema_meta")
            .find_one(doc! { "_id": "metric.schema" })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(marker.get_i32("generation").unwrap(), 21);
        database.drop().await.unwrap();
    }
}
