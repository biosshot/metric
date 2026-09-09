//! Existing DSNs are aliases to ordinary project keys; they carry no policy state.
use futures_util::TryStreamExt;
use metric_domain::{DsnKey, DsnMapping, ExistingDsn, ProjectId};
use metric_ports::ProjectStoreError;
use mongodb::{
    Database, IndexModel,
    bson::{Document, doc},
    options::IndexOptions,
};

use crate::{MongoProjectStore, dsn_key_from_slice, duplicate_write, key_binary};

pub(crate) fn validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "project_id", "target_key", "dsn"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "project_id": { "bsonType": "int", "minimum": 1 },
            "target_key": { "bsonType": "binData" },
            "dsn": { "bsonType": "string", "minLength": 1, "maxLength": 2048 },
        }
    }}
}

pub(crate) async fn create_index(database: &Database) -> Result<(), mongodb::error::Error> {
    database
        .collection::<Document>("dsn_maps")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "project_id": 1, "target_key": 1 })
                .options(
                    IndexOptions::builder()
                        .name("dsn_map_project".to_owned())
                        .unique(true)
                        .build(),
                )
                .build(),
        )
        .await?;
    Ok(())
}

impl MongoProjectStore {
    pub(crate) async fn load_dsn_mapping_inner(
        &self,
        key: DsnKey,
    ) -> Result<DsnMapping, ProjectStoreError> {
        let document = self
            .database
            .collection::<Document>("dsn_maps")
            .find_one(doc! { "_id": key_binary(key) })
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
            .ok_or(ProjectStoreError::NotFound)?;
        let source = ExistingDsn::parse(
            document
                .get_str("dsn")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectStoreError::InvalidData)?;
        if source.key != key {
            return Err(ProjectStoreError::InvalidData);
        }
        let target_key = dsn_key_from_slice(
            document
                .get_binary_generic("target_key")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )?;
        Ok(DsnMapping { source, target_key })
    }

    pub(crate) async fn insert_dsn_mapping_inner(
        &self,
        project_id: ProjectId,
        mapping: DsnMapping,
    ) -> Result<(), ProjectStoreError> {
        // Disabled native keys are still reserved. A map must never shadow them.
        if self
            .database
            .collection::<Document>("project_keys")
            .find_one(doc! { "_id": key_binary(mapping.source.key) })
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
            .is_some()
        {
            return Err(ProjectStoreError::KeyCollision);
        }
        let target = self.load_project_inner(mapping.target_key).await?;
        if target.project_id != project_id {
            return Err(ProjectStoreError::InvalidData);
        }
        self.database
            .collection::<Document>("dsn_maps")
            .insert_one(doc! {
                "_id": key_binary(mapping.source.key),
                "project_id": project_id.get(),
                "target_key": key_binary(mapping.target_key),
                "dsn": mapping.source.public_dsn,
            })
            .await
            .map_err(|error| {
                if duplicate_write(&error).is_some() {
                    ProjectStoreError::KeyCollision
                } else {
                    ProjectStoreError::Unavailable
                }
            })?;
        Ok(())
    }

    pub(crate) async fn attach_existing_dsns(
        &self,
        project_id: ProjectId,
        keys: &mut [metric_domain::api::ProjectKeyView],
    ) -> Result<(), ProjectStoreError> {
        let mut cursor = self
            .database
            .collection::<Document>("dsn_maps")
            .find(doc! { "project_id": project_id.get() })
            .limit(self.max_keys_per_project.saturating_add(1) as i64)
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?;
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
        {
            let target = dsn_key_from_slice(
                document
                    .get_binary_generic("target_key")
                    .map_err(|_| ProjectStoreError::InvalidData)?,
            )?;
            if let Some(key) = keys.iter_mut().find(|key| key.key == target) {
                key.existing_dsn = Some(
                    document
                        .get_str("dsn")
                        .map_err(|_| ProjectStoreError::InvalidData)?
                        .to_owned(),
                );
            }
        }
        Ok(())
    }
}
