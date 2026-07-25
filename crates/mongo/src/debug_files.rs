use std::collections::BTreeSet;

use metric_domain::{
    OrganizationId, ProjectId, Timestamp,
    api::ProjectView,
    debug_files::{
        CodeId, DebugFile, DebugFileId, DebugFileType, DebugId, DebugUpload, DebugUploadRecord,
        DebugUploadState, validate_debug_name,
    },
};
use metric_ports::{DebugFileStore, DebugFileStoreError, PortFuture};
use futures_util::TryStreamExt;
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::IndexOptions,
};

use crate::decode_project_view;

const CODEC_REVISION: i32 = 1;

#[derive(Debug, Clone, Copy)]
pub struct DebugFileQuota {
    pub maximum_bytes_per_project: u64,
    pub maximum_files_per_project: u64,
}

impl Default for DebugFileQuota {
    fn default() -> Self {
        Self {
            maximum_bytes_per_project: 10 * 1024 * 1024 * 1024,
            maximum_files_per_project: 10_000,
        }
    }
}

#[derive(Clone)]
pub struct MongoDebugFileStore {
    database: Database,
    quota: DebugFileQuota,
}

impl MongoDebugFileStore {
    #[must_use]
    pub fn from_database(database: Database, quota: DebugFileQuota) -> Self {
        Self { database, quota }
    }

    async fn resolve(
        &self,
        organization_slug: &str,
        project_slug: &str,
    ) -> Result<(OrganizationId, ProjectView), DebugFileStoreError> {
        let organization = self
            .database
            .collection::<Document>("organizations")
            .find_one(doc! { "slug": organization_slug })
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
            .ok_or(DebugFileStoreError::NotFound)?;
        let organization_id = OrganizationId::new(
            u64::try_from(
                organization
                    .get_i64("_id")
                    .map_err(|_| DebugFileStoreError::InvalidData)?,
            )
            .map_err(|_| DebugFileStoreError::InvalidData)?,
        )
        .map_err(|_| DebugFileStoreError::InvalidData)?;
        let project = self
            .database
            .collection::<Document>("projects")
            .find_one(doc! {
                "organization_id": i64::try_from(organization_id.get())
                    .map_err(|_| DebugFileStoreError::InvalidData)?,
                "slug": project_slug,
                "state": { "$in": ["active", "disabled"] },
            })
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
            .ok_or(DebugFileStoreError::NotFound)?;
        let view = decode_project_view(&project).map_err(|_| DebugFileStoreError::InvalidData)?;
        Ok((organization_id, view))
    }

    async fn by_sha1(
        &self,
        project_id: ProjectId,
        sha1: [u8; 20],
    ) -> Result<Option<DebugFile>, DebugFileStoreError> {
        self.database
            .collection::<Document>("debug_files")
            .find_one(doc! { "p": project_id.get(), "h": binary(&sha1) })
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
            .map(|document| decode_file(&document))
            .transpose()
    }

    async fn upsert(&self, upload: DebugUpload) -> Result<DebugUploadRecord, DebugFileStoreError> {
        let collection = self.database.collection::<Document>("debug_uploads");
        collection
            .update_one(
                doc! { "_id": binary(&upload.id) },
                doc! { "$setOnInsert": encode_upload(&upload)? },
            )
            .upsert(true)
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?;
        let document = collection
            .find_one(doc! { "_id": binary(&upload.id) })
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
            .ok_or(DebugFileStoreError::Unavailable)?;
        let record = decode_upload(&document)?;
        if record.upload.project_id != upload.project_id
            || record.upload.sha1 != upload.sha1
            || record.upload.chunks != upload.chunks
        {
            return Err(DebugFileStoreError::Conflict);
        }
        Ok(record)
    }

    async fn update_upload_state(
        &self,
        upload_id: [u8; 16],
        state: DebugUploadState,
        now: Timestamp,
        error_code: Option<Box<str>>,
    ) -> Result<(), DebugFileStoreError> {
        let mut set = doc! { "u": datetime(now) };
        let mut update = Document::new();
        match state_code(state) {
            None => {
                update.insert("$unset", doc! { "s": "", "e": "" });
            }
            Some(code) => {
                set.insert("s", code);
                if let Some(error) = error_code {
                    set.insert("e", error.as_ref());
                }
            }
        }
        if state == DebugUploadState::Assembling {
            update.insert("$inc", doc! { "a": 1_i32 });
        }
        update.insert("$set", set);
        let result = self
            .database
            .collection::<Document>("debug_uploads")
            .update_one(doc! { "_id": binary(&upload_id) }, update)
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?;
        if result.matched_count == 0 {
            return Err(DebugFileStoreError::NotFound);
        }
        Ok(())
    }

    async fn publish(
        &self,
        upload_id: [u8; 16],
        file: DebugFile,
    ) -> Result<u64, DebugFileStoreError> {
        let existing = self.by_sha1(file.project_id, file.sha1).await?;
        if let Some(existing) = existing {
            if existing.id != file.id || existing.checksum != file.checksum {
                return Err(DebugFileStoreError::Conflict);
            }
            self.update_upload_state(
                upload_id,
                DebugUploadState::Complete,
                file.uploaded_at,
                None,
            )
            .await?;
            return self.project_revision(file.project_id).await;
        }
        let maximum_bytes = i64::try_from(self.quota.maximum_bytes_per_project)
            .map_err(|_| DebugFileStoreError::InvalidData)?;
        let maximum_files = i64::try_from(self.quota.maximum_files_per_project)
            .map_err(|_| DebugFileStoreError::InvalidData)?;
        let size = i64::try_from(file.size).map_err(|_| DebugFileStoreError::InvalidData)?;
        let project = self
            .database
            .collection::<Document>("projects")
            .find_one(doc! {
                "_id": file.project_id.get(),
                "$expr": {
                    "$and": [
                        { "$lte": [{ "$add": [{ "$ifNull": ["$db", 0_i64] }, size] }, maximum_bytes] },
                        { "$lt": [{ "$ifNull": ["$dc", 0_i64] }, maximum_files] },
                    ]
                }
            })
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
            .ok_or(DebugFileStoreError::Quota)?;
        drop(project);
        self.database
            .collection::<Document>("debug_files")
            .insert_one(encode_file(&file)?)
            .await
            .map_err(|_| DebugFileStoreError::Conflict)?;
        let project = self
            .database
            .collection::<Document>("projects")
            .find_one_and_update(
                doc! { "_id": file.project_id.get() },
                doc! {
                    "$inc": { "dr": 1_i64, "db": size, "dc": 1_i64 },
                },
            )
            .return_document(mongodb::options::ReturnDocument::After)
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
            .ok_or(DebugFileStoreError::Unavailable)?;
        self.update_upload_state(
            upload_id,
            DebugUploadState::Complete,
            file.uploaded_at,
            None,
        )
        .await?;
        revision(&project)
    }

    async fn find_files(
        &self,
        project_id: ProjectId,
        debug_id: Option<DebugId>,
        code_id: Option<CodeId>,
        limit: usize,
    ) -> Result<Vec<DebugFile>, DebugFileStoreError> {
        if !(1..=20).contains(&limit) || (debug_id.is_none() && code_id.is_none()) {
            return Err(DebugFileStoreError::InvalidData);
        }
        let mut alternatives = Vec::new();
        if let Some(debug_id) = debug_id {
            alternatives.push(doc! { "d": binary(&debug_id.encode()) });
        }
        if let Some(code_id) = code_id {
            alternatives.push(doc! { "c": binary(&code_id.encode()) });
        }
        let mut cursor = self
            .database
            .collection::<Document>("debug_files")
            .find(doc! { "p": project_id.get(), "$or": alternatives })
            .sort(doc! { "u": -1, "_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(20))
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?;
        let mut files = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
        {
            files.push(decode_file(&document)?);
        }
        Ok(files)
    }

    async fn load_file(
        &self,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> Result<DebugFile, DebugFileStoreError> {
        self.database
            .collection::<Document>("debug_files")
            .find_one(doc! { "_id": binary(&file_id.as_bytes()), "p": project_id.get() })
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
            .ok_or(DebugFileStoreError::NotFound)
            .and_then(|document| decode_file(&document))
    }

    async fn delete_file(
        &self,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> Result<Option<(DebugFile, u64)>, DebugFileStoreError> {
        let Some(document) = self
            .database
            .collection::<Document>("debug_files")
            .find_one_and_delete(doc! { "_id": binary(&file_id.as_bytes()), "p": project_id.get() })
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
        else {
            return Ok(None);
        };
        let file = decode_file(&document)?;
        let project = self
            .database
            .collection::<Document>("projects")
            .find_one_and_update(
                doc! { "_id": project_id.get() },
                doc! {
                    "$inc": {
                        "dr": 1_i64,
                        "db": -i64::try_from(file.size).map_err(|_| DebugFileStoreError::InvalidData)?,
                        "dc": -1_i64,
                    }
                },
            )
            .return_document(mongodb::options::ReturnDocument::After)
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
            .ok_or(DebugFileStoreError::Unavailable)?;
        Ok(Some((file, revision(&project)?)))
    }

    async fn recoverable(
        &self,
        limit: usize,
    ) -> Result<Vec<DebugUploadRecord>, DebugFileStoreError> {
        if !(1..=1000).contains(&limit) {
            return Err(DebugFileStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("debug_uploads")
            .find(doc! { "s": { "$in": [Bson::Null, 1_i32] } })
            .sort(doc! { "u": 1, "_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(1000))
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?;
        let mut records = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
        {
            records.push(decode_upload(&document)?);
        }
        Ok(records)
    }

    async fn project_revision(&self, project_id: ProjectId) -> Result<u64, DebugFileStoreError> {
        let project = self
            .database
            .collection::<Document>("projects")
            .find_one(doc! { "_id": project_id.get() })
            .await
            .map_err(|_| DebugFileStoreError::Unavailable)?
            .ok_or(DebugFileStoreError::NotFound)?;
        revision(&project)
    }
}

impl DebugFileStore for MongoDebugFileStore {
    fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<OrganizationId, DebugFileStoreError>> {
        Box::pin(async move {
            let document = self
                .database
                .collection::<Document>("projects")
                .find_one(doc! { "_id": project_id.get(), "state": { "$ne": "deleted" } })
                .await
                .map_err(|_| DebugFileStoreError::Unavailable)?
                .ok_or(DebugFileStoreError::NotFound)?;
            OrganizationId::new(
                u64::try_from(
                    document
                        .get_i64("organization_id")
                        .map_err(|_| DebugFileStoreError::InvalidData)?,
                )
                .map_err(|_| DebugFileStoreError::InvalidData)?,
            )
            .map_err(|_| DebugFileStoreError::InvalidData)
        })
    }

    fn resolve_project_slugs(
        &self,
        organization_slug: Box<str>,
        project_slug: Box<str>,
    ) -> PortFuture<'_, Result<(OrganizationId, ProjectView), DebugFileStoreError>> {
        Box::pin(async move { self.resolve(&organization_slug, &project_slug).await })
    }

    fn load_by_sha1(
        &self,
        project_id: ProjectId,
        sha1: [u8; 20],
    ) -> PortFuture<'_, Result<Option<DebugFile>, DebugFileStoreError>> {
        Box::pin(self.by_sha1(project_id, sha1))
    }

    fn upsert_upload(
        &self,
        upload: DebugUpload,
    ) -> PortFuture<'_, Result<DebugUploadRecord, DebugFileStoreError>> {
        Box::pin(self.upsert(upload))
    }

    fn set_upload_state(
        &self,
        upload_id: [u8; 16],
        state: DebugUploadState,
        now: Timestamp,
        error_code: Option<Box<str>>,
    ) -> PortFuture<'_, Result<(), DebugFileStoreError>> {
        Box::pin(self.update_upload_state(upload_id, state, now, error_code))
    }

    fn publish_debug_file(
        &self,
        upload_id: [u8; 16],
        file: DebugFile,
    ) -> PortFuture<'_, Result<u64, DebugFileStoreError>> {
        Box::pin(self.publish(upload_id, file))
    }

    fn find_debug_files(
        &self,
        project_id: ProjectId,
        debug_id: Option<DebugId>,
        code_id: Option<CodeId>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<DebugFile>, DebugFileStoreError>> {
        Box::pin(self.find_files(project_id, debug_id, code_id, limit))
    }

    fn load_debug_file(
        &self,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> PortFuture<'_, Result<DebugFile, DebugFileStoreError>> {
        Box::pin(self.load_file(project_id, file_id))
    }

    fn delete_debug_file(
        &self,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> PortFuture<'_, Result<Option<(DebugFile, u64)>, DebugFileStoreError>> {
        Box::pin(self.delete_file(project_id, file_id))
    }

    fn recoverable_uploads(
        &self,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<DebugUploadRecord>, DebugFileStoreError>> {
        Box::pin(self.recoverable(limit))
    }
}

pub(crate) fn debug_file_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "y", "x", "h", "z", "n", "u"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "p": { "bsonType": "int", "minimum": 1 },
            "d": { "bsonType": "binData" },
            "c": { "bsonType": "binData" },
            "y": { "bsonType": "int", "minimum": 0 },
            "x": { "bsonType": "binData" },
            "h": { "bsonType": "binData" },
            "z": { "bsonType": "long", "minimum": 0 },
            "n": { "bsonType": "string", "minLength": 1, "maxLength": 255 },
            "u": { "bsonType": "date" },
        },
        "anyOf": [{ "required": ["d"] }, { "required": ["c"] }]
    }}
}

pub(crate) fn debug_upload_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "o", "h", "n", "c", "a", "t", "u"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "p": { "bsonType": "int", "minimum": 1 },
            "o": { "bsonType": "long", "minimum": 1 },
            "h": { "bsonType": "binData" },
            "n": { "bsonType": "string", "minLength": 1, "maxLength": 255 },
            "d": { "bsonType": "binData" },
            "i": { "bsonType": "binData" },
            "c": { "bsonType": "binData" },
            "s": { "enum": [1, 2, 3] },
            "a": { "bsonType": "int", "minimum": 0 },
            "t": { "bsonType": "date" },
            "u": { "bsonType": "date" },
            "e": { "bsonType": "string", "maxLength": 64 },
        }
    }}
}

pub(crate) async fn create_debug_file_indexes(
    database: &Database,
) -> Result<(), mongodb::error::Error> {
    let files = database.collection::<Document>("debug_files");
    for model in [
        index(doc! { "p": 1, "d": 1 }, "debug_file_debug_id", false),
        index(doc! { "p": 1, "c": 1 }, "debug_file_code_id", false),
        index(
            doc! { "p": 1, "h": 1 },
            "debug_file_project_sha1_unique",
            true,
        ),
        index(
            doc! { "p": 1, "u": -1, "_id": 1 },
            "debug_file_project_list",
            false,
        ),
    ] {
        files.create_index(model).await?;
    }
    let uploads = database.collection::<Document>("debug_uploads");
    uploads
        .create_index(index(
            doc! { "s": 1, "u": 1 },
            "debug_upload_recovery",
            false,
        ))
        .await?;
    uploads
        .create_index(
            IndexModel::builder()
                .keys(doc! { "u": 1 })
                .options(
                    IndexOptions::builder()
                        .name("debug_upload_expiry".to_owned())
                        .expire_after(std::time::Duration::from_secs(24 * 60 * 60))
                        .build(),
                )
                .build(),
        )
        .await?;
    Ok(())
}

pub(crate) async fn validate_debug_file_indexes(
    database: &Database,
) -> Result<bool, mongodb::error::Error> {
    for (collection, expected) in [
        (
            "debug_files",
            BTreeSet::from([
                "_id_".to_owned(),
                "debug_file_debug_id".to_owned(),
                "debug_file_code_id".to_owned(),
                "debug_file_project_sha1_unique".to_owned(),
                "debug_file_project_list".to_owned(),
            ]),
        ),
        (
            "debug_uploads",
            BTreeSet::from([
                "_id_".to_owned(),
                "debug_upload_recovery".to_owned(),
                "debug_upload_expiry".to_owned(),
            ]),
        ),
    ] {
        let mut cursor = database
            .collection::<Document>(collection)
            .list_indexes()
            .await?;
        let mut actual = BTreeSet::new();
        while let Some(model) = cursor.try_next().await? {
            if let Some(name) = model.options.and_then(|options| options.name) {
                actual.insert(name);
            }
        }
        if actual != expected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn index(keys: Document, name: &str, unique: bool) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .unique(unique)
                .build(),
        )
        .build()
}

fn encode_file(file: &DebugFile) -> Result<Document, DebugFileStoreError> {
    let mut document = doc! {
        "_id": binary(&file.id.as_bytes()),
        "p": file.project_id.get(),
        "y": (CODEC_REVISION << 20) | i32::from(file.file_type as u8),
        "x": binary(&file.checksum),
        "h": binary(&file.sha1),
        "z": i64::try_from(file.size).map_err(|_| DebugFileStoreError::InvalidData)?,
        "n": validate_debug_name(&file.name).map_err(|_| DebugFileStoreError::InvalidData)?.as_ref(),
        "u": datetime(file.uploaded_at),
    };
    if let Some(debug_id) = &file.debug_id {
        document.insert("d", binary(&debug_id.encode()));
    }
    if let Some(code_id) = &file.code_id {
        document.insert("c", binary(&code_id.encode()));
    }
    Ok(document)
}

fn decode_file(document: &Document) -> Result<DebugFile, DebugFileStoreError> {
    let id = fixed::<16>(document, "_id")?;
    let packed = document
        .get_i32("y")
        .map_err(|_| DebugFileStoreError::InvalidData)?;
    if (packed >> 20) & 0x0f != CODEC_REVISION {
        return Err(DebugFileStoreError::InvalidData);
    }
    let file_type = match packed & 0x3f {
        0 => DebugFileType::Elf,
        1 => DebugFileType::MachO,
        2 => DebugFileType::Pe,
        3 => DebugFileType::Pdb,
        4 => DebugFileType::PortablePdb,
        5 => DebugFileType::Breakpad,
        _ => return Err(DebugFileStoreError::InvalidData),
    };
    let debug_id = optional_binary(document, "d")?
        .map(DebugId::decode)
        .transpose()
        .map_err(|_| DebugFileStoreError::InvalidData)?;
    let code_id = optional_binary(document, "c")?
        .map(CodeId::decode)
        .transpose()
        .map_err(|_| DebugFileStoreError::InvalidData)?;
    if debug_id.is_none() && code_id.is_none() {
        return Err(DebugFileStoreError::InvalidData);
    }
    Ok(DebugFile {
        id: DebugFileId::from_bytes(id),
        project_id: ProjectId::new(
            document
                .get_i32("p")
                .map_err(|_| DebugFileStoreError::InvalidData)?,
        )
        .map_err(|_| DebugFileStoreError::InvalidData)?,
        debug_id,
        code_id,
        file_type,
        checksum: fixed(document, "x")?,
        sha1: fixed(document, "h")?,
        size: u64::try_from(
            document
                .get_i64("z")
                .map_err(|_| DebugFileStoreError::InvalidData)?,
        )
        .map_err(|_| DebugFileStoreError::InvalidData)?,
        name: validate_debug_name(
            document
                .get_str("n")
                .map_err(|_| DebugFileStoreError::InvalidData)?,
        )
        .map_err(|_| DebugFileStoreError::InvalidData)?,
        uploaded_at: timestamp(document, "u")?,
    })
}

fn encode_upload(upload: &DebugUpload) -> Result<Document, DebugFileStoreError> {
    if upload.chunks.is_empty() {
        return Err(DebugFileStoreError::InvalidData);
    }
    let mut chunks = Vec::with_capacity(upload.chunks.len() * 20);
    for chunk in &upload.chunks {
        chunks.extend_from_slice(chunk);
    }
    let mut document = doc! {
        "_id": binary(&upload.id),
        "p": upload.project_id.get(),
        "o": i64::try_from(upload.organization_id.get()).map_err(|_| DebugFileStoreError::InvalidData)?,
        "h": binary(&upload.sha1),
        "n": validate_debug_name(&upload.name).map_err(|_| DebugFileStoreError::InvalidData)?.as_ref(),
        "c": binary(&chunks),
        "a": 0_i32,
        "t": datetime(upload.created_at),
        "u": datetime(upload.updated_at),
    };
    if let Some(debug_id) = &upload.debug_id {
        document.insert("d", binary(&debug_id.encode()));
    }
    if let Some(code_id) = &upload.code_id {
        document.insert("i", binary(&code_id.encode()));
    }
    Ok(document)
}

fn decode_upload(document: &Document) -> Result<DebugUploadRecord, DebugFileStoreError> {
    let packed = binary_slice(document, "c")?;
    if packed.is_empty() || packed.len() % 20 != 0 {
        return Err(DebugFileStoreError::InvalidData);
    }
    let chunks = packed
        .chunks_exact(20)
        .map(|chunk| chunk.try_into().expect("exact SHA-1 chunk"))
        .collect();
    let state = match document.get_i32("s") {
        Err(_) if !document.contains_key("s") => DebugUploadState::Pending,
        Ok(1) => DebugUploadState::Assembling,
        Ok(2) => DebugUploadState::Complete,
        Ok(3) => DebugUploadState::Failed,
        _ => return Err(DebugFileStoreError::InvalidData),
    };
    Ok(DebugUploadRecord {
        upload: DebugUpload {
            id: fixed(document, "_id")?,
            project_id: ProjectId::new(
                document
                    .get_i32("p")
                    .map_err(|_| DebugFileStoreError::InvalidData)?,
            )
            .map_err(|_| DebugFileStoreError::InvalidData)?,
            organization_id: OrganizationId::new(
                u64::try_from(
                    document
                        .get_i64("o")
                        .map_err(|_| DebugFileStoreError::InvalidData)?,
                )
                .map_err(|_| DebugFileStoreError::InvalidData)?,
            )
            .map_err(|_| DebugFileStoreError::InvalidData)?,
            sha1: fixed(document, "h")?,
            name: validate_debug_name(
                document
                    .get_str("n")
                    .map_err(|_| DebugFileStoreError::InvalidData)?,
            )
            .map_err(|_| DebugFileStoreError::InvalidData)?,
            debug_id: optional_binary(document, "d")?
                .map(DebugId::decode)
                .transpose()
                .map_err(|_| DebugFileStoreError::InvalidData)?,
            code_id: optional_binary(document, "i")?
                .map(CodeId::decode)
                .transpose()
                .map_err(|_| DebugFileStoreError::InvalidData)?,
            chunks,
            created_at: timestamp(document, "t")?,
            updated_at: timestamp(document, "u")?,
        },
        state,
        attempts: u32::try_from(
            document
                .get_i32("a")
                .map_err(|_| DebugFileStoreError::InvalidData)?,
        )
        .map_err(|_| DebugFileStoreError::InvalidData)?,
        error_code: document.get_str("e").ok().map(Into::into),
    })
}

fn state_code(state: DebugUploadState) -> Option<i32> {
    match state {
        DebugUploadState::Pending => None,
        DebugUploadState::Assembling => Some(1),
        DebugUploadState::Complete => Some(2),
        DebugUploadState::Failed => Some(3),
    }
}

fn revision(project: &Document) -> Result<u64, DebugFileStoreError> {
    match project.get_i64("dr") {
        Ok(value) => u64::try_from(value).map_err(|_| DebugFileStoreError::InvalidData),
        Err(_) if !project.contains_key("dr") => Ok(0),
        Err(_) => Err(DebugFileStoreError::InvalidData),
    }
}

fn binary(bytes: &[u8]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn binary_slice<'a>(document: &'a Document, field: &str) -> Result<&'a [u8], DebugFileStoreError> {
    document
        .get_binary_generic(field)
        .map(Vec::as_slice)
        .map_err(|_| DebugFileStoreError::InvalidData)
}

fn optional_binary<'a>(
    document: &'a Document,
    field: &str,
) -> Result<Option<&'a [u8]>, DebugFileStoreError> {
    if !document.contains_key(field) {
        return Ok(None);
    }
    binary_slice(document, field).map(Some)
}

fn fixed<const N: usize>(document: &Document, field: &str) -> Result<[u8; N], DebugFileStoreError> {
    binary_slice(document, field)?
        .try_into()
        .map_err(|_| DebugFileStoreError::InvalidData)
}

fn datetime(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn timestamp(document: &Document, field: &str) -> Result<Timestamp, DebugFileStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(field)
            .map_err(|_| DebugFileStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| DebugFileStoreError::InvalidData)
}
