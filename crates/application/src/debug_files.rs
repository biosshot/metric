//! Bounded Sentry CLI debug-file upload and private project lookup service.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use metric_domain::{
    OrganizationId, ProjectId, Timestamp,
    api::ProjectView,
    auth::{AuthContext, Permission},
    blob::{BlobKey, BlobKind, BlobNamespace, BlobObject},
    debug_files::{
        CodeId, DebugFile, DebugFileId, DebugFileType, DebugId, DebugUpload, DebugUploadRecord,
        DebugUploadState, validate_debug_name,
    },
};
use metric_ports::{
    BlobReadSession, BlobScanRequest, BlobStore, BlobStoreError, Clock, DebugFileStore,
    DebugFileStoreError,
};
use sha1::{Digest as _, Sha1};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use crate::shutdown::ShutdownSignal;

pub const SENTRY_CLI_CHUNK_BYTES: usize = 8 * 1024 * 1024;
pub const SENTRY_CLI_MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
pub const SENTRY_CLI_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const SENTRY_CLI_CHUNKS_PER_REQUEST: usize = 64;

#[derive(Debug, Clone)]
pub struct AssembleDebugFile {
    pub sha1: [u8; 20],
    pub name: Box<str>,
    pub debug_id: Option<DebugId>,
    pub code_id: Option<CodeId>,
    pub chunks: Vec<[u8; 20]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssembleState {
    Missing { chunks: Vec<[u8; 20]> },
    Ok { file: DebugFile, revision: u64 },
    Error { code: &'static str },
}

#[derive(Debug, Clone, Copy)]
pub struct DebugFileConfig {
    pub max_file_bytes: u64,
    pub stream_chunk_bytes: usize,
    pub max_concurrent_assemblies: usize,
    pub chunk_expiry: Duration,
    pub orphan_grace: Duration,
    pub cleanup_batch_size: usize,
}

impl Default for DebugFileConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: SENTRY_CLI_MAX_FILE_BYTES,
            stream_chunk_bytes: 64 * 1024,
            max_concurrent_assemblies: 2,
            chunk_expiry: Duration::from_secs(24 * 60 * 60),
            orphan_grace: Duration::from_secs(24 * 60 * 60),
            cleanup_batch_size: 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DebugFileError {
    #[error("debug file request is invalid")]
    InvalidRequest,
    #[error("debug file request is forbidden")]
    Forbidden,
    #[error("debug file target was not found")]
    NotFound,
    #[error("debug file quota is exhausted")]
    Quota,
    #[error("debug file request conflicts with existing data")]
    Conflict,
    #[error("debug file storage is temporarily unavailable")]
    Unavailable,
}

pub struct DebugFileService {
    metadata: Arc<dyn DebugFileStore>,
    blobs: Arc<dyn BlobStore>,
    clock: Arc<dyn Clock>,
    config: DebugFileConfig,
    assemblies: Semaphore,
    locks: Mutex<BTreeMap<DebugFileId, Weak<AsyncMutex<()>>>>,
}

impl DebugFileService {
    pub fn new(
        metadata: Arc<dyn DebugFileStore>,
        blobs: Arc<dyn BlobStore>,
        clock: Arc<dyn Clock>,
        config: DebugFileConfig,
    ) -> Result<Self, DebugFileError> {
        if config.max_file_bytes == 0
            || !(4 * 1024..=1024 * 1024).contains(&config.stream_chunk_bytes)
            || config.max_concurrent_assemblies == 0
            || config.max_concurrent_assemblies > 64
            || config.chunk_expiry.is_zero()
            || config.orphan_grace.is_zero()
            || !(1..=10_000).contains(&config.cleanup_batch_size)
        {
            return Err(DebugFileError::InvalidRequest);
        }
        Ok(Self {
            metadata,
            blobs,
            clock,
            config,
            assemblies: Semaphore::new(config.max_concurrent_assemblies),
            locks: Mutex::new(BTreeMap::new()),
        })
    }

    pub async fn resolve_project(
        &self,
        context: &AuthContext,
        organization_slug: &str,
        project_slug: &str,
        permission: Permission,
    ) -> Result<(OrganizationId, ProjectView), DebugFileError> {
        require(context, permission)?;
        let (organization_id, project) = self
            .metadata
            .resolve_project_slugs(organization_slug.into(), project_slug.into())
            .await
            .map_err(map_store)?;
        if organization_id != context.organization_id {
            return Err(DebugFileError::NotFound);
        }
        Ok((organization_id, project))
    }

    #[must_use]
    pub const fn maximum_file_bytes(&self) -> u64 {
        self.config.max_file_bytes
    }

    pub async fn upload_chunk(
        &self,
        context: &AuthContext,
        organization_id: OrganizationId,
        sha1: [u8; 20],
        bytes: Box<[u8]>,
    ) -> Result<BlobObject, DebugFileError> {
        require(context, Permission::DebugFileWrite)?;
        if context.organization_id != organization_id || bytes.len() > SENTRY_CLI_CHUNK_BYTES {
            return Err(DebugFileError::InvalidRequest);
        }
        let actual: [u8; 20] = Sha1::digest(&bytes).into();
        if actual != sha1 {
            return Err(DebugFileError::InvalidRequest);
        }
        let mut writer = self
            .blobs
            .begin(BlobKind::DebugChunk, self.clock.now())
            .await
            .map_err(map_blob)?;
        writer.write_chunk(bytes).await.map_err(map_blob)?;
        writer
            .commit(BlobKey::debug_chunk(organization_id, sha1))
            .await
            .map_err(map_blob)
    }

    pub async fn assemble(
        &self,
        context: &AuthContext,
        organization_id: OrganizationId,
        project_id: ProjectId,
        request: AssembleDebugFile,
    ) -> Result<AssembleState, DebugFileError> {
        require(context, Permission::DebugFileWrite)?;
        if context.organization_id != organization_id
            || request.chunks.is_empty()
            || request.chunks.len() > SENTRY_CLI_CHUNKS_PER_REQUEST
            || (request.debug_id.is_none() && request.code_id.is_none())
        {
            return Err(DebugFileError::InvalidRequest);
        }
        let name =
            validate_debug_name(&request.name).map_err(|_| DebugFileError::InvalidRequest)?;
        if let Some(file) = self
            .metadata
            .load_by_sha1(project_id, request.sha1)
            .await
            .map_err(map_store)?
        {
            return Ok(AssembleState::Ok { file, revision: 0 });
        }
        let mut missing = Vec::new();
        for chunk in &request.chunks {
            match self
                .blobs
                .open(&BlobKey::debug_chunk(organization_id, *chunk))
                .await
            {
                Ok(_) => {}
                Err(BlobStoreError::NotFound) => missing.push(*chunk),
                Err(error) => return Err(map_blob(error)),
            }
        }
        if !missing.is_empty() {
            return Ok(AssembleState::Missing { chunks: missing });
        }
        let now = self.clock.now();
        let upload = DebugUpload {
            id: upload_id(project_id, request.sha1),
            project_id,
            organization_id,
            sha1: request.sha1,
            name,
            debug_id: request.debug_id,
            code_id: request.code_id,
            chunks: request.chunks,
            created_at: now,
            updated_at: now,
        };
        let record = self
            .metadata
            .upsert_upload(upload)
            .await
            .map_err(map_store)?;
        self.assemble_record(record).await
    }

    pub async fn recover(&self, limit: usize) -> Result<usize, DebugFileError> {
        let records = self
            .metadata
            .recoverable_uploads(limit)
            .await
            .map_err(map_store)?;
        let mut completed = 0_usize;
        for record in records {
            if let Ok(AssembleState::Ok { .. }) = self.assemble_record(record).await {
                completed = completed.saturating_add(1);
            }
        }
        Ok(completed)
    }

    pub async fn find(
        &self,
        project_id: ProjectId,
        debug_id: Option<DebugId>,
        code_id: Option<CodeId>,
    ) -> Result<Vec<DebugFile>, DebugFileError> {
        self.metadata
            .find_debug_files(project_id, debug_id, code_id, 20)
            .await
            .map_err(map_store)
    }

    pub async fn open(
        &self,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> Result<(DebugFile, Box<dyn BlobReadSession>), DebugFileError> {
        let file = self
            .metadata
            .load_debug_file(project_id, file_id)
            .await
            .map_err(map_store)?;
        let reader = self
            .blobs
            .open(&BlobKey::debug_file(project_id, file_id))
            .await
            .map_err(map_blob)?;
        Ok((file, reader))
    }

    pub async fn delete(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> Result<bool, DebugFileError> {
        require(context, Permission::DebugFileDelete)?;
        let organization_id = self
            .metadata
            .project_organization(project_id)
            .await
            .map_err(map_store)?;
        if organization_id != context.organization_id {
            return Err(DebugFileError::NotFound);
        }
        let lock = self.lock_for(file_id);
        let _guard = lock.lock().await;
        let Some((file, _revision)) = self
            .metadata
            .delete_debug_file(project_id, file_id)
            .await
            .map_err(map_store)?
        else {
            return Ok(false);
        };
        let _ = self
            .blobs
            .delete(&BlobKey::debug_file(file.project_id, file.id))
            .await;
        Ok(true)
    }

    pub async fn cleanup_once(&self) -> Result<(u64, u64), DebugFileError> {
        let now = self.clock.now();
        let chunk_cutoff = subtract(now, self.config.chunk_expiry)?;
        let orphan_cutoff = subtract(now, self.config.orphan_grace)?;
        let chunks = self
            .cleanup_namespace(BlobNamespace::DebugChunks, chunk_cutoff, true)
            .await?;
        let orphans = self
            .cleanup_namespace(BlobNamespace::DebugFiles, orphan_cutoff, false)
            .await?;
        Ok((chunks, orphans))
    }

    async fn assemble_record(
        &self,
        record: DebugUploadRecord,
    ) -> Result<AssembleState, DebugFileError> {
        if record.state == DebugUploadState::Complete {
            if let Some(file) = self
                .metadata
                .load_by_sha1(record.upload.project_id, record.upload.sha1)
                .await
                .map_err(map_store)?
            {
                return Ok(AssembleState::Ok { file, revision: 0 });
            }
        }
        let _permit = self
            .assemblies
            .acquire()
            .await
            .map_err(|_| DebugFileError::Unavailable)?;
        self.metadata
            .set_upload_state(
                record.upload.id,
                DebugUploadState::Assembling,
                self.clock.now(),
                None,
            )
            .await
            .map_err(map_store)?;
        match self.assemble_stream(&record.upload).await {
            Ok((file, revision)) => Ok(AssembleState::Ok { file, revision }),
            Err(error) => {
                let terminal = matches!(
                    error,
                    DebugFileError::InvalidRequest
                        | DebugFileError::Conflict
                        | DebugFileError::Quota
                );
                let _ = self
                    .metadata
                    .set_upload_state(
                        record.upload.id,
                        if terminal {
                            DebugUploadState::Failed
                        } else {
                            DebugUploadState::Pending
                        },
                        self.clock.now(),
                        Some(error_code(error).into()),
                    )
                    .await;
                if terminal {
                    Ok(AssembleState::Error {
                        code: error_code(error),
                    })
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn assemble_stream(
        &self,
        upload: &DebugUpload,
    ) -> Result<(DebugFile, u64), DebugFileError> {
        let mut writer = self
            .blobs
            .begin(BlobKind::DebugFile, self.clock.now())
            .await
            .map_err(map_blob)?;
        let mut sha1 = Sha1::new();
        let mut blake3 = blake3::Hasher::new();
        let mut size = 0_u64;
        let mut header = Vec::with_capacity(4096);
        for chunk_id in &upload.chunks {
            let mut reader = self
                .blobs
                .open(&BlobKey::debug_chunk(upload.organization_id, *chunk_id))
                .await
                .map_err(map_blob)?;
            while let Some(bytes) = reader
                .read_chunk(self.config.stream_chunk_bytes)
                .await
                .map_err(map_blob)?
            {
                size = size
                    .checked_add(
                        u64::try_from(bytes.len()).map_err(|_| DebugFileError::InvalidRequest)?,
                    )
                    .filter(|size| *size <= self.config.max_file_bytes)
                    .ok_or(DebugFileError::InvalidRequest)?;
                if header.len() < 4096 {
                    let count = (4096 - header.len()).min(bytes.len());
                    header.extend_from_slice(&bytes[..count]);
                }
                sha1.update(&bytes);
                blake3.update(&bytes);
                writer.write_chunk(bytes).await.map_err(map_blob)?;
            }
        }
        let complete_sha1: [u8; 20] = sha1.finalize().into();
        if complete_sha1 != upload.sha1 || size == 0 {
            let _ = writer.abort().await;
            return Err(DebugFileError::InvalidRequest);
        }
        let file_type =
            DebugFileType::from_name(&upload.name).map_err(|_| DebugFileError::InvalidRequest)?;
        if file_type == DebugFileType::Breakpad && !header.starts_with(b"MODULE ") {
            let _ = writer.abort().await;
            return Err(DebugFileError::InvalidRequest);
        }
        let checksum = *blake3.finalize().as_bytes();
        let id = DebugFileId::derive(upload.project_id, checksum);
        let lock = self.lock_for(id);
        let _guard = lock.lock().await;
        let key = BlobKey::debug_file(upload.project_id, id);
        writer.commit(key).await.map_err(map_blob)?;
        let file = DebugFile {
            id,
            project_id: upload.project_id,
            debug_id: upload.debug_id.clone(),
            code_id: upload.code_id.clone(),
            file_type,
            checksum,
            sha1: upload.sha1,
            size,
            name: upload.name.clone(),
            uploaded_at: self.clock.now(),
        };
        let revision = self
            .metadata
            .publish_debug_file(upload.id, file.clone())
            .await
            .map_err(map_store)?;
        for chunk in &upload.chunks {
            let _ = self
                .blobs
                .delete(&BlobKey::debug_chunk(upload.organization_id, *chunk))
                .await;
        }
        Ok((file, revision))
    }

    async fn cleanup_namespace(
        &self,
        namespace: BlobNamespace,
        cutoff: Timestamp,
        delete_all: bool,
    ) -> Result<u64, DebugFileError> {
        let mut cursor = None;
        let mut deleted = 0_u64;
        loop {
            let page = self
                .blobs
                .scan(BlobScanRequest {
                    namespace: namespace.clone(),
                    older_than: cutoff,
                    cursor,
                    limit: self.config.cleanup_batch_size,
                })
                .await
                .map_err(map_blob)?;
            for object in page.objects {
                let orphan = if delete_all {
                    true
                } else {
                    let (project_id, file_id) = object
                        .key
                        .debug_file_relation()
                        .map_err(|_| DebugFileError::Unavailable)?;
                    matches!(
                        self.metadata.load_debug_file(project_id, file_id).await,
                        Err(DebugFileStoreError::NotFound)
                    )
                };
                if orphan {
                    self.blobs.delete(&object.key).await.map_err(map_blob)?;
                    deleted = deleted.saturating_add(1);
                }
            }
            let Some(next) = page.next_cursor else {
                break;
            };
            cursor = Some(next);
        }
        Ok(deleted)
    }

    fn lock_for(&self, id: DebugFileId) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().expect("debug-file lock registry");
        locks.retain(|_, value| value.strong_count() > 0);
        if let Some(lock) = locks.get(&id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(id, Arc::downgrade(&lock));
        lock
    }
}

pub struct DebugFileCleanupTask {
    handle: tokio::task::JoinHandle<()>,
}

impl DebugFileCleanupTask {
    pub async fn wait(self) {
        let _ = self.handle.await;
    }
}

pub fn start_debug_file_cleanup(
    service: Arc<DebugFileService>,
    interval: Duration,
    shutdown: ShutdownSignal,
) -> Result<DebugFileCleanupTask, DebugFileError> {
    if interval.is_zero() {
        return Err(DebugFileError::InvalidRequest);
    }
    let handle = tokio::spawn(async move {
        let mut ticks = tokio::time::interval(interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticks.tick().await;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = ticks.tick() => {
                    let _ = service.cleanup_once().await;
                    let _ = service.recover(100).await;
                }
            }
        }
    });
    Ok(DebugFileCleanupTask { handle })
}

fn upload_id(project_id: ProjectId, sha1: [u8; 20]) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric/debug-upload-id/v1");
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(&sha1);
    let mut id = [0_u8; 16];
    id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    id
}

fn require(context: &AuthContext, permission: Permission) -> Result<(), DebugFileError> {
    context
        .permissions
        .contains(permission)
        .then_some(())
        .ok_or(DebugFileError::Forbidden)
}

fn map_store(error: DebugFileStoreError) -> DebugFileError {
    match error {
        DebugFileStoreError::NotFound => DebugFileError::NotFound,
        DebugFileStoreError::Conflict => DebugFileError::Conflict,
        DebugFileStoreError::Quota => DebugFileError::Quota,
        DebugFileStoreError::InvalidData | DebugFileStoreError::Unavailable => {
            DebugFileError::Unavailable
        }
    }
}

fn map_blob(error: BlobStoreError) -> DebugFileError {
    match error {
        BlobStoreError::TooLarge | BlobStoreError::Invalid => DebugFileError::InvalidRequest,
        BlobStoreError::Capacity => DebugFileError::Quota,
        BlobStoreError::NotFound => DebugFileError::NotFound,
        BlobStoreError::Corrupt | BlobStoreError::Unavailable => DebugFileError::Unavailable,
    }
}

const fn error_code(error: DebugFileError) -> &'static str {
    match error {
        DebugFileError::InvalidRequest => "invalid_debug_file",
        DebugFileError::Forbidden => "forbidden",
        DebugFileError::NotFound => "missing_chunk",
        DebugFileError::Quota => "quota_exceeded",
        DebugFileError::Conflict => "conflict",
        DebugFileError::Unavailable => "storage_unavailable",
    }
}

fn subtract(now: Timestamp, duration: Duration) -> Result<Timestamp, DebugFileError> {
    let millis = i64::try_from(duration.as_millis()).map_err(|_| DebugFileError::InvalidRequest)?;
    Timestamp::from_unix_millis(
        now.unix_millis()
            .checked_sub(millis)
            .ok_or(DebugFileError::InvalidRequest)?,
    )
    .map_err(|_| DebugFileError::InvalidRequest)
}
