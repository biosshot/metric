//! Durable bounded project-deletion job and versioned dataset registry.

use std::{collections::BTreeSet, time::Duration};

use faultkeep_domain::{
    DsnKey, OrganizationId, ProjectId, Timestamp,
    deletion::{
        ProjectDeletionChange, ProjectDeletionOperationId, ProjectDeletionPhase,
        ProjectDeletionRequest, ProjectDeletionStatus,
    },
};
use faultkeep_ports::{
    PortFuture, ProjectDeletionStore, ProjectDeletionStoreError, ProjectPurgeRequest,
};
use futures_util::TryStreamExt;
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::IndexOptions,
};

use crate::{MongoProjectStore, dsn_key_from_slice};

pub const DELETION_PLAN_VERSION: u16 = 1;
pub const FIRST_DATASET_CODE: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetOwnership {
    ProjectOwned,
    OrganizationShared,
    RetainedAudit,
    ControlPlane,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetRegistration {
    pub code: u16,
    pub name: &'static str,
    pub ownership: DatasetOwnership,
}

/// Numeric codes are append-only. Existing codes must never be renamed or reused.
pub const DATASET_REGISTRY: [DatasetRegistration; 25] = [
    DatasetRegistration {
        code: 0,
        name: "api_tokens",
        ownership: DatasetOwnership::Global,
    },
    DatasetRegistration {
        code: 0,
        name: "audit_log",
        ownership: DatasetOwnership::RetainedAudit,
    },
    DatasetRegistration {
        code: 64,
        name: "alert_rules",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 68,
        name: "archive_manifests",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 50,
        name: "environments",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 10,
        name: "events",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 52,
        name: "debug_files",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 54,
        name: "debug_uploads",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 56,
        name: "artifact_uploads",
        ownership: DatasetOwnership::OrganizationShared,
    },
    DatasetRegistration {
        code: 58,
        name: "artifact_bundles",
        ownership: DatasetOwnership::OrganizationShared,
    },
    DatasetRegistration {
        code: 20,
        name: "issue_activities",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 40,
        name: "issues",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 30,
        name: "issue_stats_hourly",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 66,
        name: "notification_deliveries",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 62,
        name: "notification_destinations",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 0,
        name: "organization_memberships",
        ownership: DatasetOwnership::Global,
    },
    DatasetRegistration {
        code: 0,
        name: "organizations",
        ownership: DatasetOwnership::Global,
    },
    DatasetRegistration {
        code: 0,
        name: "password_setup_tokens",
        ownership: DatasetOwnership::Global,
    },
    DatasetRegistration {
        code: 80,
        name: "project_deletions",
        ownership: DatasetOwnership::ControlPlane,
    },
    DatasetRegistration {
        code: 70,
        name: "project_keys",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 80,
        name: "projects",
        ownership: DatasetOwnership::ControlPlane,
    },
    DatasetRegistration {
        code: 60,
        name: "releases",
        ownership: DatasetOwnership::OrganizationShared,
    },
    DatasetRegistration {
        code: 0,
        name: "schema_meta",
        ownership: DatasetOwnership::Global,
    },
    DatasetRegistration {
        code: 0,
        name: "users",
        ownership: DatasetOwnership::Global,
    },
    DatasetRegistration {
        code: 0,
        name: "web_sessions",
        ownership: DatasetOwnership::Global,
    },
];

/// Blob cleanup owns the bounded physical purge after MongoDB parent deletion.
pub const FILESYSTEM_NAMESPACE_REGISTRY: [DatasetRegistration; 5] = [
    DatasetRegistration {
        code: 90,
        name: "blob:projects/{project_id}/events",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 91,
        name: "blob:d/{project_id_base36}",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 92,
        name: "blob:projects/{project_id}/archives/events",
        ownership: DatasetOwnership::ProjectOwned,
    },
    DatasetRegistration {
        code: 0,
        name: "blob:debug-chunks/{organization_id}",
        ownership: DatasetOwnership::OrganizationShared,
    },
    DatasetRegistration {
        code: 0,
        name: "blob:a/{organization_id_base36}",
        ownership: DatasetOwnership::OrganizationShared,
    },
];

const PURGE_CODES: [u16; 15] = [10, 20, 30, 40, 50, 52, 54, 56, 58, 60, 62, 64, 66, 68, 70];

impl MongoProjectStore {
    async fn request_deletion_inner(
        &self,
        request: ProjectDeletionRequest,
    ) -> Result<ProjectDeletionChange, ProjectDeletionStoreError> {
        let projects = self.database.collection::<Document>("projects");
        let project = projects
            .find_one(doc! {
                "_id": request.project_id.get(),
                "organization_id": i64::try_from(request.organization_id.get())
                    .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
                "state": { "$in": ["active", "disabled", "pending_delete"] },
            })
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
            .ok_or(ProjectDeletionStoreError::NotFound)?;
        let requested_operation = operation_binary(request.operation_id);
        let jobs = self.database.collection::<Document>("project_deletions");
        if let Some(existing) = jobs
            .find_one(doc! { "project_id": request.project_id.get(), "terminal": false })
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
        {
            if existing.get_binary_generic("_id") != Ok(&requested_operation.bytes) {
                return Err(ProjectDeletionStoreError::Conflict);
            }
            let keys = self.fence_project(request.project_id).await?;
            return Ok(ProjectDeletionChange {
                status: decode_status(&existing)?,
                affected_keys: keys,
            });
        }
        let previous_state = project
            .get_str("state")
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
        jobs.insert_one(doc! {
            "_id": requested_operation,
            "plan_version": i32::from(DELETION_PLAN_VERSION),
            "project_id": request.project_id.get(),
            "organization_id": i64::try_from(request.organization_id.get())
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
            "requested_by": i64::try_from(request.requested_by.get())
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
            "phase": "pending_grace",
            "previous_project_state": previous_state,
            "dataset_code": i32::from(FIRST_DATASET_CODE),
            "reconciliation_pass": false,
            "requested_at": date(request.requested_at),
            "purge_after": date(request.purge_after),
            "next_attempt_at": date(request.purge_after),
            "attempts": 0_i64,
            "terminal": false,
            "slug_released": false,
        })
        .await
        .map_err(|error| {
            if error.to_string().contains("E11000") {
                ProjectDeletionStoreError::Conflict
            } else {
                ProjectDeletionStoreError::Unavailable
            }
        })?;
        let keys = self.fence_project(request.project_id).await?;
        let status = jobs
            .find_one(doc! { "_id": operation_binary(request.operation_id) })
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
            .ok_or(ProjectDeletionStoreError::Unavailable)
            .and_then(|document| decode_status(&document))?;
        Ok(ProjectDeletionChange {
            status,
            affected_keys: keys,
        })
    }

    async fn fence_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<DsnKey>, ProjectDeletionStoreError> {
        self.database
            .collection::<Document>("projects")
            .update_one(
                doc! { "_id": project_id.get(), "state": { "$in": ["active", "disabled", "pending_delete"] } },
                doc! { "$set": { "state": "pending_delete" } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        let keys_collection = self.database.collection::<Document>("project_keys");
        keys_collection
            .update_many(
                doc! { "project_id": project_id.get(), "status": "active" },
                doc! { "$set": { "status": "suspended_by_deletion" } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        load_keys(&keys_collection, project_id, self.max_keys_per_project).await
    }

    async fn cancel_deletion_inner(
        &self,
        project_id: ProjectId,
        operation_id: ProjectDeletionOperationId,
        now: Timestamp,
        completed_retention: Duration,
    ) -> Result<ProjectDeletionChange, ProjectDeletionStoreError> {
        let jobs = self.database.collection::<Document>("project_deletions");
        let job = jobs
            .find_one(
                doc! { "_id": operation_binary(operation_id), "project_id": project_id.get() },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
            .ok_or(ProjectDeletionStoreError::NotFound)?;
        if job.get_str("phase") != Ok("pending_grace") {
            return Err(ProjectDeletionStoreError::NotCancellable);
        }
        let previous_state = job
            .get_str("previous_project_state")
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
        self.database
            .collection::<Document>("projects")
            .update_one(
                doc! { "_id": project_id.get(), "state": "pending_delete" },
                doc! { "$set": { "state": previous_state } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        let keys_collection = self.database.collection::<Document>("project_keys");
        keys_collection
            .update_many(
                doc! { "project_id": project_id.get(), "status": "suspended_by_deletion" },
                doc! { "$set": { "status": "active" } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        let expire_at = add_duration(now, completed_retention)?;
        let updated = jobs
            .find_one_and_update(
                doc! { "_id": operation_binary(operation_id), "phase": "pending_grace" },
                doc! { "$set": {
                    "phase": "cancelled",
                    "terminal": true,
                    "completed_at": date(now),
                    "expire_at": date(expire_at),
                    "next_attempt_at": date(expire_at),
                }},
            )
            .return_document(mongodb::options::ReturnDocument::After)
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
            .ok_or(ProjectDeletionStoreError::NotCancellable)?;
        Ok(ProjectDeletionChange {
            status: decode_status(&updated)?,
            affected_keys: load_keys(&keys_collection, project_id, self.max_keys_per_project)
                .await?,
        })
    }

    async fn deletion_status_inner(
        &self,
        project_id: ProjectId,
    ) -> Result<ProjectDeletionStatus, ProjectDeletionStoreError> {
        let document = self
            .database
            .collection::<Document>("project_deletions")
            .find_one(doc! { "project_id": project_id.get() })
            .sort(doc! { "requested_at": -1, "_id": -1 })
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
            .ok_or(ProjectDeletionStoreError::NotFound)?;
        decode_status(&document)
    }

    async fn purge_next_inner(
        &self,
        request: ProjectPurgeRequest,
    ) -> Result<Option<ProjectDeletionStatus>, ProjectDeletionStoreError> {
        if request.batch_size == 0 {
            return Err(ProjectDeletionStoreError::InvalidData);
        }
        self.release_one_expired_slug(request.now).await?;
        let jobs = self.database.collection::<Document>("project_deletions");
        let Some(mut job) = jobs
            .find_one(doc! {
                "terminal": false,
                "phase": { "$in": ["pending_grace", "purging"] },
                "next_attempt_at": { "$lte": date(request.now) },
            })
            .sort(doc! { "next_attempt_at": 1, "_id": 1 })
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
        else {
            return Ok(None);
        };
        let operation = job
            .get_binary_generic("_id")
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?
            .to_vec();
        let project_id = ProjectId::new(
            job.get_i32("project_id")
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
        if job.get_str("phase") == Ok("pending_grace") {
            self.database
                .collection::<Document>("projects")
                .update_one(
                    doc! { "_id": project_id.get(), "state": "pending_delete" },
                    doc! { "$set": { "state": "purging" } },
                )
                .await
                .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            jobs.update_one(
                doc! { "_id": generic_binary(&operation), "phase": "pending_grace" },
                doc! { "$set": { "phase": "purging" } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            job.insert("phase", "purging");
        }
        let result = self
            .purge_dataset(&job, project_id, request.batch_size)
            .await;
        if let Err(error) = result {
            let attempts = job.get_i64("attempts").unwrap_or(0).saturating_add(1);
            let delay = retry_delay(request.retry_base, request.retry_max, attempts);
            jobs.update_one(
                doc! { "_id": generic_binary(&operation) },
                doc! { "$set": {
                    "attempts": attempts,
                    "last_error": "dataset_batch_failed",
                    "next_attempt_at": date(add_duration(request.now, delay)?),
                }},
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            return Err(error);
        }
        self.advance_or_complete(
            &job,
            project_id,
            request.now,
            request.completed_retention,
            request.slug_reservation,
        )
        .await?;
        let updated = jobs
            .find_one(doc! { "_id": generic_binary(&operation) })
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
            .ok_or(ProjectDeletionStoreError::Unavailable)?;
        Ok(Some(decode_status(&updated)?))
    }

    async fn purge_dataset(
        &self,
        job: &Document,
        project_id: ProjectId,
        batch_size: usize,
    ) -> Result<(), ProjectDeletionStoreError> {
        let code = u16::try_from(
            job.get_i32("dataset_code")
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
        let cursor = job.get("cursor").cloned();
        match code {
            10 => {
                self.delete_owned_batch("events", "p", project_id, cursor, batch_size)
                    .await
            }
            20 => {
                self.delete_owned_batch("issue_activities", "p", project_id, cursor, batch_size)
                    .await
            }
            30 => {
                self.delete_owned_batch(
                    "issue_stats_hourly",
                    "project_id",
                    project_id,
                    cursor,
                    batch_size,
                )
                .await
            }
            40 => {
                self.delete_owned_batch("issues", "p", project_id, cursor, batch_size)
                    .await
            }
            50 => {
                self.delete_owned_batch(
                    "environments",
                    "project_id",
                    project_id,
                    cursor,
                    batch_size,
                )
                .await
            }
            52 => {
                self.delete_owned_batch("debug_files", "p", project_id, cursor, batch_size)
                    .await
            }
            54 => {
                self.delete_owned_batch("debug_uploads", "p", project_id, cursor, batch_size)
                    .await
            }
            56 => {
                self.detach_artifact_binding_batch(
                    "artifact_uploads",
                    job,
                    project_id,
                    cursor,
                    batch_size,
                    false,
                )
                .await
            }
            58 => {
                self.detach_artifact_binding_batch(
                    "artifact_bundles",
                    job,
                    project_id,
                    cursor,
                    batch_size,
                    true,
                )
                .await
            }
            60 => {
                self.detach_release_batch(project_id, cursor, batch_size)
                    .await
            }
            62 => {
                self.delete_owned_batch(
                    "notification_destinations",
                    "p",
                    project_id,
                    cursor,
                    batch_size,
                )
                .await
            }
            64 => {
                self.delete_owned_batch("alert_rules", "p", project_id, cursor, batch_size)
                    .await
            }
            66 => {
                self.delete_owned_batch(
                    "notification_deliveries",
                    "p",
                    project_id,
                    cursor,
                    batch_size,
                )
                .await
            }
            68 => {
                self.delete_owned_batch(
                    "archive_manifests",
                    "project_id",
                    project_id,
                    cursor,
                    batch_size,
                )
                .await
            }
            70 => {
                self.delete_owned_batch(
                    "project_keys",
                    "project_id",
                    project_id,
                    cursor,
                    batch_size,
                )
                .await
            }
            _ => Err(ProjectDeletionStoreError::InvalidData),
        }
    }

    async fn delete_owned_batch(
        &self,
        collection_name: &str,
        project_field: &str,
        project_id: ProjectId,
        cursor: Option<Bson>,
        batch_size: usize,
    ) -> Result<(), ProjectDeletionStoreError> {
        let collection = self.database.collection::<Document>(collection_name);
        let mut filter = doc! { project_field: project_id.get() };
        if let Some(cursor) = cursor {
            filter.insert("_id", doc! { "$gt": cursor });
        }
        let mut stream = collection
            .find(filter)
            .projection(doc! { "_id": 1 })
            .sort(doc! { "_id": 1 })
            .limit(i64::try_from(batch_size).map_err(|_| ProjectDeletionStoreError::InvalidData)?)
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        let mut ids = Vec::with_capacity(batch_size);
        while let Some(document) = stream
            .try_next()
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
        {
            ids.push(
                document
                    .get("_id")
                    .cloned()
                    .ok_or(ProjectDeletionStoreError::InvalidData)?,
            );
        }
        if !ids.is_empty() {
            collection
                .delete_many(doc! { "_id": { "$in": &ids }, project_field: project_id.get() })
                .await
                .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        }
        self.persist_batch_cursor(project_id, ids.last().cloned(), ids.len(), batch_size)
            .await
    }

    async fn detach_release_batch(
        &self,
        project_id: ProjectId,
        cursor: Option<Bson>,
        batch_size: usize,
    ) -> Result<(), ProjectDeletionStoreError> {
        let collection = self.database.collection::<Document>("releases");
        let mut filter = doc! { "project_ids": project_id.get() };
        if let Some(cursor) = cursor {
            filter.insert("_id", doc! { "$gt": cursor });
        }
        let mut stream = collection
            .find(filter)
            .projection(doc! { "_id": 1, "project_ids": 1 })
            .sort(doc! { "_id": 1 })
            .limit(i64::try_from(batch_size).map_err(|_| ProjectDeletionStoreError::InvalidData)?)
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        let mut ids = Vec::with_capacity(batch_size);
        while let Some(document) = stream
            .try_next()
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
        {
            let id = document
                .get("_id")
                .cloned()
                .ok_or(ProjectDeletionStoreError::InvalidData)?;
            let project_ids = document
                .get_array("project_ids")
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
            if project_ids.len() == 1 {
                collection
                    .delete_one(doc! { "_id": &id, "project_ids": project_id.get() })
                    .await
                    .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            } else {
                collection
                    .update_one(
                        doc! { "_id": &id, "project_ids": project_id.get() },
                        doc! { "$pull": { "project_ids": project_id.get() } },
                    )
                    .await
                    .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            }
            ids.push(id);
        }
        self.persist_batch_cursor(project_id, ids.last().cloned(), ids.len(), batch_size)
            .await
    }

    async fn detach_artifact_binding_batch(
        &self,
        collection_name: &str,
        job: &Document,
        project_id: ProjectId,
        cursor: Option<Bson>,
        batch_size: usize,
        orphan_when_empty: bool,
    ) -> Result<(), ProjectDeletionStoreError> {
        let collection = self.database.collection::<Document>(collection_name);
        let mut filter = doc! { "b": { "$elemMatch": { "p": project_id.get() } } };
        if let Some(cursor) = cursor {
            filter.insert("_id", doc! { "$gt": cursor });
        }
        let mut stream = collection
            .find(filter)
            .projection(doc! { "_id": 1, "b": 1 })
            .sort(doc! { "_id": 1 })
            .limit(i64::try_from(batch_size).map_err(|_| ProjectDeletionStoreError::InvalidData)?)
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        let mut documents = Vec::with_capacity(batch_size);
        while let Some(document) = stream
            .try_next()
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
        {
            documents.push(document);
        }
        for document in &documents {
            let id = document
                .get("_id")
                .cloned()
                .ok_or(ProjectDeletionStoreError::InvalidData)?;
            let old = document
                .get_array("b")
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
            let remaining = old
                .iter()
                .filter(|binding| {
                    binding
                        .as_document()
                        .and_then(|binding| binding.get_i32("p").ok())
                        != Some(project_id.get())
                })
                .cloned()
                .collect::<Vec<_>>();
            if remaining.is_empty() {
                if orphan_when_empty {
                    let operation = job
                        .get_binary_generic("_id")
                        .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
                    collection
                        .update_one(
                            doc! { "_id": id, "b": old },
                            doc! {
                                "$unset": { "b": "" },
                                "$set": { "e": DateTime::now(), "j": generic_binary(operation) },
                            },
                        )
                        .await
                        .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
                } else {
                    collection
                        .delete_one(doc! { "_id": id, "b": old })
                        .await
                        .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
                }
            } else {
                collection
                    .update_one(
                        doc! { "_id": id, "b": old },
                        doc! { "$set": { "b": remaining } },
                    )
                    .await
                    .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            }
        }
        self.persist_batch_cursor(
            project_id,
            documents
                .last()
                .and_then(|document| document.get("_id").cloned()),
            documents.len(),
            batch_size,
        )
        .await
    }

    async fn persist_batch_cursor(
        &self,
        project_id: ProjectId,
        cursor: Option<Bson>,
        count: usize,
        batch_size: usize,
    ) -> Result<(), ProjectDeletionStoreError> {
        let jobs = self.database.collection::<Document>("project_deletions");
        if count == batch_size {
            jobs.update_one(
                doc! { "project_id": project_id.get(), "terminal": false },
                doc! { "$set": { "cursor": cursor }, "$unset": { "last_error": "" } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        } else {
            let current = jobs
                .find_one(doc! { "project_id": project_id.get(), "terminal": false })
                .await
                .map_err(|_| ProjectDeletionStoreError::Unavailable)?
                .ok_or(ProjectDeletionStoreError::Unavailable)?;
            let code = u16::try_from(current.get_i32("dataset_code").unwrap_or_default())
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
            if let Some(next) = next_code(code) {
                jobs.update_one(
                    doc! { "_id": current.get("_id").cloned().ok_or(ProjectDeletionStoreError::InvalidData)? },
                    doc! { "$set": { "dataset_code": i32::from(next) }, "$unset": { "cursor": "", "last_error": "" } },
                )
                .await
                .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            } else {
                jobs.update_one(
                    doc! { "_id": current.get("_id").cloned().ok_or(ProjectDeletionStoreError::InvalidData)? },
                    doc! { "$unset": { "cursor": "", "last_error": "" } },
                )
                .await
                .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            }
        }
        Ok(())
    }

    async fn advance_or_complete(
        &self,
        job: &Document,
        project_id: ProjectId,
        now: Timestamp,
        completed_retention: Duration,
        slug_reservation: Duration,
    ) -> Result<(), ProjectDeletionStoreError> {
        let jobs = self.database.collection::<Document>("project_deletions");
        let processed_code = u16::try_from(
            job.get_i32("dataset_code")
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
        let current = jobs
            .find_one(doc! { "project_id": project_id.get(), "terminal": false })
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
            .ok_or(ProjectDeletionStoreError::Unavailable)?;
        let code = u16::try_from(current.get_i32("dataset_code").unwrap_or_default())
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
        if processed_code != 70 || code != 70 || current.contains_key("cursor") {
            jobs.update_one(
                doc! { "_id": current.get("_id").cloned().ok_or(ProjectDeletionStoreError::InvalidData)? },
                doc! { "$set": { "next_attempt_at": date(now), "attempts": 0_i64 }, "$unset": { "last_error": "" } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            return Ok(());
        }
        let operation = job
            .get_binary_generic("_id")
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
        if self
            .database
            .collection::<Document>("artifact_bundles")
            .find_one(doc! { "j": generic_binary(operation) })
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
            .is_some()
        {
            jobs.update_one(
                doc! { "_id": current.get("_id").cloned().ok_or(ProjectDeletionStoreError::InvalidData)? },
                doc! { "$set": { "next_attempt_at": date(now), "attempts": 0_i64 } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            return Ok(());
        }
        let reconciliation = current.get_bool("reconciliation_pass").unwrap_or(false);
        if !reconciliation {
            jobs.update_one(
                doc! { "_id": current.get("_id").cloned().ok_or(ProjectDeletionStoreError::InvalidData)? },
                doc! { "$set": {
                    "dataset_code": i32::from(FIRST_DATASET_CODE),
                    "reconciliation_pass": true,
                    "next_attempt_at": date(now),
                }, "$unset": { "cursor": "" } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
            return Ok(());
        }
        let slug_until = add_duration(now, slug_reservation)?;
        self.database
            .collection::<Document>("projects")
            .update_one(
                doc! { "_id": project_id.get(), "state": "purging" },
                doc! {
                    "$set": {
                        "state": "deleted",
                        "deleted_at": date(now),
                        "deletion_operation_id": generic_binary(operation),
                        "slug_reserved_until": date(slug_until),
                    },
                    "$unset": {
                        "display_name": "", "policy": "", "items": "", "limits": "",
                        "grouping_revision": "", "catalog_usage": "",
                    },
                },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        jobs.update_one(
            doc! { "_id": generic_binary(operation), "terminal": false },
            doc! { "$set": {
                "phase": "deleted",
                "terminal": true,
                "completed_at": date(now),
                "expire_at": date(add_duration(now, completed_retention)?),
                "slug_reserved_until": date(slug_until),
                "next_attempt_at": date(slug_until),
                "attempts": 0_i64,
            }, "$unset": { "cursor": "", "last_error": "" } },
        )
        .await
        .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        Ok(())
    }

    async fn release_one_expired_slug(
        &self,
        now: Timestamp,
    ) -> Result<(), ProjectDeletionStoreError> {
        let jobs = self.database.collection::<Document>("project_deletions");
        let Some(job) = jobs
            .find_one(doc! {
                "phase": "deleted",
                "slug_released": false,
                "slug_reserved_until": { "$lte": date(now) },
            })
            .sort(doc! { "slug_reserved_until": 1 })
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?
        else {
            return Ok(());
        };
        let project_id = job
            .get_i32("project_id")
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
        self.database
            .collection::<Document>("projects")
            .update_one(
                doc! { "_id": project_id, "state": "deleted" },
                doc! { "$unset": { "slug": "" } },
            )
            .await
            .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        jobs.update_one(
            doc! { "_id": job.get("_id").cloned().ok_or(ProjectDeletionStoreError::InvalidData)? },
            doc! { "$set": { "slug_released": true } },
        )
        .await
        .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
        Ok(())
    }
}

impl ProjectDeletionStore for MongoProjectStore {
    fn request_deletion(
        &self,
        request: ProjectDeletionRequest,
    ) -> PortFuture<'_, Result<ProjectDeletionChange, ProjectDeletionStoreError>> {
        Box::pin(self.request_deletion_inner(request))
    }

    fn cancel_deletion(
        &self,
        project_id: ProjectId,
        operation_id: ProjectDeletionOperationId,
        now: Timestamp,
        completed_retention: Duration,
    ) -> PortFuture<'_, Result<ProjectDeletionChange, ProjectDeletionStoreError>> {
        Box::pin(self.cancel_deletion_inner(project_id, operation_id, now, completed_retention))
    }

    fn deletion_status(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<ProjectDeletionStatus, ProjectDeletionStoreError>> {
        Box::pin(self.deletion_status_inner(project_id))
    }

    fn purge_next(
        &self,
        request: ProjectPurgeRequest,
    ) -> PortFuture<'_, Result<Option<ProjectDeletionStatus>, ProjectDeletionStoreError>> {
        Box::pin(self.purge_next_inner(request))
    }
}

pub(crate) fn deletion_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": [
            "_id", "plan_version", "project_id", "organization_id", "requested_by",
            "phase", "previous_project_state", "dataset_code", "reconciliation_pass",
            "requested_at", "purge_after", "next_attempt_at", "attempts", "terminal",
            "slug_released"
        ],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "plan_version": { "bsonType": "int", "minimum": 1 },
            "project_id": { "bsonType": "int", "minimum": 1 },
            "organization_id": { "bsonType": "long", "minimum": 1 },
            "requested_by": { "bsonType": "long", "minimum": 1 },
            "phase": { "enum": ["pending_grace", "purging", "deleted", "cancelled"] },
            "previous_project_state": { "enum": ["active", "disabled"] },
            "dataset_code": { "bsonType": "int", "minimum": 10 },
            "reconciliation_pass": { "bsonType": "bool" },
            "cursor": {},
            "requested_at": { "bsonType": "date" },
            "purge_after": { "bsonType": "date" },
            "next_attempt_at": { "bsonType": "date" },
            "attempts": { "bsonType": "long", "minimum": 0 },
            "last_error": { "bsonType": "string", "maxLength": 128 },
            "terminal": { "bsonType": "bool" },
            "completed_at": { "bsonType": "date" },
            "expire_at": { "bsonType": "date" },
            "slug_reserved_until": { "bsonType": "date" },
            "slug_released": { "bsonType": "bool" },
        },
    }}
}

pub(crate) fn deletion_indexes() -> [IndexModel; 3] {
    [
        IndexModel::builder()
            .keys(doc! { "project_id": 1 })
            .options(
                IndexOptions::builder()
                    .name("project_deletion_active_unique".to_owned())
                    .unique(true)
                    .partial_filter_expression(doc! { "terminal": false })
                    .build(),
            )
            .build(),
        IndexModel::builder()
            .keys(doc! { "terminal": 1, "next_attempt_at": 1, "_id": 1 })
            .options(
                IndexOptions::builder()
                    .name("project_deletion_due".to_owned())
                    .build(),
            )
            .build(),
        IndexModel::builder()
            .keys(doc! { "expire_at": 1 })
            .options(
                IndexOptions::builder()
                    .name("project_deletion_expiration".to_owned())
                    .expire_after(Duration::ZERO)
                    .build(),
            )
            .build(),
    ]
}

pub(crate) fn deletion_index_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "_id_",
        "project_deletion_active_unique",
        "project_deletion_due",
        "project_deletion_expiration",
    ])
}

pub(crate) async fn validate_deletion_indexes(
    database: &Database,
) -> Result<bool, mongodb::error::Error> {
    let mut expected = deletion_indexes()
        .into_iter()
        .map(|model| {
            let name = model
                .options
                .as_ref()
                .and_then(|options| options.name.clone())
                .expect("deletion index has a name");
            (name, model)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut actual = database
        .collection::<Document>("project_deletions")
        .list_indexes()
        .await?;
    while let Some(model) = actual.try_next().await? {
        let Some(options) = model.options.as_ref() else {
            return Ok(false);
        };
        let Some(name) = options.name.as_deref() else {
            return Ok(false);
        };
        if name == "_id_" {
            continue;
        }
        let Some(expected_model) = expected.remove(name) else {
            return Ok(false);
        };
        let expected_options = expected_model
            .options
            .as_ref()
            .expect("deletion index has options");
        if model.keys != expected_model.keys
            || options.unique != expected_options.unique
            || options.partial_filter_expression != expected_options.partial_filter_expression
            || options.expire_after != expected_options.expire_after
        {
            return Ok(false);
        }
    }
    Ok(expected.is_empty())
}

fn decode_status(document: &Document) -> Result<ProjectDeletionStatus, ProjectDeletionStoreError> {
    let operation = document
        .get_binary_generic("_id")
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
    let operation: [u8; 16] = operation
        .as_slice()
        .try_into()
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
    let timestamp = |field| {
        document
            .get_datetime(field)
            .map(|value| Timestamp::from_unix_millis(value.timestamp_millis()))
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?
            .map_err(|_| ProjectDeletionStoreError::InvalidData)
    };
    Ok(ProjectDeletionStatus {
        operation_id: ProjectDeletionOperationId::from_bytes(operation),
        project_id: ProjectId::new(
            document
                .get_i32("project_id")
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        organization_id: OrganizationId::new(
            u64::try_from(
                document
                    .get_i64("organization_id")
                    .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
            )
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        phase: match document
            .get_str("phase")
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?
        {
            "pending_grace" => ProjectDeletionPhase::PendingGrace,
            "purging" => ProjectDeletionPhase::Purging,
            "deleted" => ProjectDeletionPhase::Deleted,
            "cancelled" => ProjectDeletionPhase::Cancelled,
            _ => return Err(ProjectDeletionStoreError::InvalidData),
        },
        dataset_code: u16::try_from(
            document
                .get_i32("dataset_code")
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        reconciliation_pass: document
            .get_bool("reconciliation_pass")
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        requested_at: timestamp("requested_at")?,
        purge_after: timestamp("purge_after")?,
        completed_at: document
            .get_datetime("completed_at")
            .ok()
            .map(|value| Timestamp::from_unix_millis(value.timestamp_millis()))
            .transpose()
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        next_attempt_at: timestamp("next_attempt_at")?,
        attempts: u32::try_from(
            document
                .get_i64("attempts")
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        last_error: document.get_str("last_error").ok().map(str::to_owned),
    })
}

async fn load_keys(
    collection: &mongodb::Collection<Document>,
    project_id: ProjectId,
    maximum: usize,
) -> Result<Vec<DsnKey>, ProjectDeletionStoreError> {
    let mut stream = collection
        .find(doc! { "project_id": project_id.get() })
        .projection(doc! { "_id": 1 })
        .limit(i64::try_from(maximum.saturating_add(1)).unwrap_or(i64::MAX))
        .await
        .map_err(|_| ProjectDeletionStoreError::Unavailable)?;
    let mut keys = Vec::new();
    while let Some(document) = stream
        .try_next()
        .await
        .map_err(|_| ProjectDeletionStoreError::Unavailable)?
    {
        keys.push(
            dsn_key_from_slice(
                document
                    .get_binary_generic("_id")
                    .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
            )
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        );
    }
    if keys.len() > maximum {
        return Err(ProjectDeletionStoreError::InvalidData);
    }
    Ok(keys)
}

fn next_code(code: u16) -> Option<u16> {
    PURGE_CODES
        .iter()
        .position(|candidate| *candidate == code)
        .and_then(|position| PURGE_CODES.get(position.saturating_add(1)))
        .copied()
}

fn operation_binary(operation: ProjectDeletionOperationId) -> Binary {
    generic_binary(&operation.as_bytes())
}

fn generic_binary(bytes: &[u8]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn add_duration(
    timestamp: Timestamp,
    duration: Duration,
) -> Result<Timestamp, ProjectDeletionStoreError> {
    let millis =
        i64::try_from(duration.as_millis()).map_err(|_| ProjectDeletionStoreError::InvalidData)?;
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_add(millis))
        .map_err(|_| ProjectDeletionStoreError::InvalidData)
}

fn retry_delay(base: Duration, maximum: Duration, attempts: i64) -> Duration {
    let shift = u32::try_from(attempts.saturating_sub(1).min(31)).unwrap_or(31);
    base.checked_mul(1_u32.checked_shl(shift).unwrap_or(u32::MAX))
        .unwrap_or(maximum)
        .min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::REQUIRED_COLLECTIONS;

    #[test]
    fn every_schema_collection_is_classified_once() {
        let registered = DATASET_REGISTRY
            .iter()
            .map(|entry| entry.name)
            .collect::<BTreeSet<_>>();
        let required = REQUIRED_COLLECTIONS.into_iter().collect::<BTreeSet<_>>();
        assert_eq!(registered.len(), DATASET_REGISTRY.len());
        assert_eq!(registered, required);
    }

    #[test]
    fn purge_dataset_codes_are_stable_unique_and_ordered() {
        let registered = DATASET_REGISTRY
            .iter()
            .filter(|entry| entry.code > 0 && entry.code < 80)
            .map(|entry| entry.code)
            .collect::<BTreeSet<_>>();
        assert_eq!(registered, PURGE_CODES.into_iter().collect());
    }
}
