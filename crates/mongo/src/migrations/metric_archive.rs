//! Schema-only repair of Metrics archive admission and hot-copy expiry.
use super::{
    MigrationAttemptError, MigrationBatch, MigrationCheckpoint, MigrationFuture, MongoMigration,
    RecordErrorPolicy,
};
use crate::{
    archive::{archive_manifest_validator, archive_manifest_validator_v21},
    metric_storage::{metric_archive_retention_index, metric_validator, metric_validator_v21},
};
use futures_util::TryStreamExt;
use mongodb::{
    Database, IndexModel,
    bson::{Document, doc},
};

pub(crate) static METRIC_ARCHIVE_MIGRATION: MetricArchiveMigration = MetricArchiveMigration;
pub(crate) struct MetricArchiveMigration;

impl MongoMigration for MetricArchiveMigration {
    fn source_generation(&self) -> i32 {
        21
    }
    fn target_generation(&self) -> i32 {
        22
    }
    fn name(&self) -> &'static str {
        "metric_archive_validation_and_retention"
    }
    fn step_count(&self) -> u32 {
        3
    }
    fn step_name(&self, step: u32) -> &'static str {
        match step {
            0 => "allow_metric_archive_manifests",
            1 => "allow_committed_metric_segments",
            2 => "index_archived_metric_expiry",
            _ => "invalid_metric_archive_step",
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
            for (name, legacy, current) in [
                (
                    "archive_manifests",
                    archive_manifest_validator_v21(),
                    archive_manifest_validator(),
                ),
                ("metric_buckets", metric_validator_v21(), metric_validator()),
            ] {
                let actual = validator(database, name).await?;
                if actual != legacy && actual != current {
                    return Err(MigrationAttemptError::blocked(
                        "metric_archive_validator_unknown",
                    ));
                }
            }
            if archive_index(database)
                .await?
                .is_some_and(|index| !correct_index(&index))
            {
                return Err(MigrationAttemptError::blocked(
                    "metric_archive_expiry_index_conflict",
                ));
            }
            Ok(())
        })
    }
    fn run_batch<'a>(
        &'a self,
        database: &'a Database,
        step: u32,
        _checkpoint: &'a MigrationCheckpoint,
    ) -> MigrationFuture<'a, Result<MigrationBatch, MigrationAttemptError>> {
        Box::pin(async move {
            match step {
                0 => install(database, "archive_manifests", archive_manifest_validator()).await?,
                1 => install(database, "metric_buckets", metric_validator()).await?,
                2 => {
                    database
                        .collection::<Document>("metric_buckets")
                        .create_index(metric_archive_retention_index())
                        .await
                        .map_err(MigrationAttemptError::from_mongo)?;
                }
                _ => {
                    return Err(MigrationAttemptError::blocked(
                        "metric_archive_step_unknown",
                    ));
                }
            }
            // DDL only: no metric payload, archive manifest or user data is rewritten.
            Ok(MigrationBatch::complete(0))
        })
    }
    fn verify<'a>(
        &'a self,
        database: &'a Database,
    ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>> {
        Box::pin(async move {
            if validator(database, "archive_manifests").await? != archive_manifest_validator()
                || validator(database, "metric_buckets").await? != metric_validator()
                || !archive_index(database)
                    .await?
                    .is_some_and(|index| correct_index(&index))
            {
                return Err(MigrationAttemptError::blocked(
                    "metric_archive_schema_incomplete",
                ));
            }
            Ok(())
        })
    }
}
fn correct_index(index: &IndexModel) -> bool {
    let expected = metric_archive_retention_index();
    index.keys == expected.keys
        && index
            .options
            .as_ref()
            .zip(expected.options.as_ref())
            .is_some_and(|(actual, expected)| {
                actual.expire_after == expected.expire_after
                    && actual.partial_filter_expression == expected.partial_filter_expression
            })
}
async fn archive_index(database: &Database) -> Result<Option<IndexModel>, MigrationAttemptError> {
    let mut cursor = database
        .collection::<Document>("metric_buckets")
        .list_indexes()
        .await
        .map_err(MigrationAttemptError::from_mongo)?;
    while let Some(index) = cursor
        .try_next()
        .await
        .map_err(MigrationAttemptError::from_mongo)?
    {
        if index
            .options
            .as_ref()
            .and_then(|options| options.name.as_deref())
            == Some("metric_archive_retention")
        {
            return Ok(Some(index));
        }
    }
    Ok(None)
}
async fn validator(database: &Database, name: &str) -> Result<Document, MigrationAttemptError> {
    let mut cursor = database
        .list_collections()
        .filter(doc! {"name":name})
        .await
        .map_err(MigrationAttemptError::from_mongo)?;
    cursor
        .try_next()
        .await
        .map_err(MigrationAttemptError::from_mongo)?
        .and_then(|collection| collection.options.validator)
        .ok_or_else(|| MigrationAttemptError::blocked("metric_archive_validator_missing"))
}
async fn install(
    database: &Database,
    name: &str,
    validator: Document,
) -> Result<(), MigrationAttemptError> {
    database.run_command(doc!{"collMod":name,"validator":validator,"validationLevel":"strict","validationAction":"error"})
        .await.map(|_|()).map_err(MigrationAttemptError::from_mongo)
}

#[cfg(test)]
mod tests;
