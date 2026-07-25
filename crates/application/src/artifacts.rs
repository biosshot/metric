//! Bounded Sentry CLI Artifact Bundle assembly, lookup, and generation-fenced GC.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::shutdown::ShutdownSignal;
use metric_domain::{
    OrganizationId, ProjectId, Timestamp,
    artifacts::{
        ArtifactBinding, ArtifactBundle, ArtifactBundleId, ArtifactCandidate, ArtifactDebugIdToken,
        ArtifactGcClaim, ArtifactLookup, ArtifactUpload, ArtifactUploadRecord, ArtifactUploadState,
        MAX_ARTIFACT_BINDINGS, MAX_ARTIFACT_CHUNKS, MAX_ARTIFACT_DEBUG_IDS,
    },
    auth::{AuthContext, Permission},
    blob::{BlobKey, BlobKind},
    debug_files::DebugId,
    finalization::derive_release_id,
};
use metric_ports::{
    ArtifactStore, ArtifactStoreError, BlobReadSession, BlobStore, BlobStoreError, Clock,
    RandomSource,
};
use futures_util::{StreamExt, TryStreamExt};
use serde::Deserialize;
use sha1::{Digest as _, Sha1};
use thiserror::Error;
use tokio::sync::Semaphore;

const MAX_RELEASE_BYTES: usize = 200;

#[derive(Debug, Clone, Copy)]
pub struct ArtifactConfig {
    pub maximum_bundle_bytes: u64,
    pub stream_chunk_bytes: usize,
    pub maximum_logical_bytes: u64,
    pub maximum_entries: usize,
    pub maximum_entry_bytes: u64,
    pub maximum_manifest_bytes: u64,
    pub maximum_path_bytes: usize,
    pub maximum_url_bytes: usize,
    pub maximum_metadata_fields: usize,
    pub maximum_compression_ratio: u64,
    pub maximum_concurrent_assemblies: usize,
    pub parse_timeout: Duration,
    pub orphan_grace: Duration,
    pub claim_lease: Duration,
    pub blob_operation_timeout: Duration,
    pub tombstone_retention: Duration,
    pub gc_batch_size: usize,
    pub gc_max_concurrency: usize,
}

impl Default for ArtifactConfig {
    fn default() -> Self {
        Self {
            maximum_bundle_bytes: 64 * 1024 * 1024,
            stream_chunk_bytes: 64 * 1024,
            maximum_logical_bytes: 512 * 1024 * 1024,
            maximum_entries: 10_000,
            maximum_entry_bytes: 16 * 1024 * 1024,
            maximum_manifest_bytes: 4 * 1024 * 1024,
            maximum_path_bytes: 1_024,
            maximum_url_bytes: 4_096,
            maximum_metadata_fields: 64,
            maximum_compression_ratio: 100,
            maximum_concurrent_assemblies: 2,
            parse_timeout: Duration::from_secs(30),
            orphan_grace: Duration::from_secs(24 * 60 * 60),
            claim_lease: Duration::from_secs(5 * 60),
            blob_operation_timeout: Duration::from_secs(30),
            tombstone_retention: Duration::from_secs(24 * 60 * 60),
            gc_batch_size: 100,
            gc_max_concurrency: 4,
        }
    }
}

impl ArtifactConfig {
    pub fn validate(self) -> Result<Self, ArtifactError> {
        let valid = self.maximum_bundle_bytes > 0
            && self.maximum_bundle_bytes <= 512 * 1024 * 1024
            && (4 * 1024..=1024 * 1024).contains(&self.stream_chunk_bytes)
            && self.maximum_logical_bytes >= self.maximum_bundle_bytes
            && self.maximum_logical_bytes <= 4 * 1024 * 1024 * 1024
            && (1..=100_000).contains(&self.maximum_entries)
            && self.maximum_entry_bytes > 0
            && self.maximum_entry_bytes <= self.maximum_logical_bytes
            && self.maximum_manifest_bytes > 0
            && self.maximum_manifest_bytes <= self.maximum_entry_bytes
            && (64..=8_192).contains(&self.maximum_path_bytes)
            && (64..=16_384).contains(&self.maximum_url_bytes)
            && (1..=1_024).contains(&self.maximum_metadata_fields)
            && (1..=1_000).contains(&self.maximum_compression_ratio)
            && (1..=64).contains(&self.maximum_concurrent_assemblies)
            && !self.parse_timeout.is_zero()
            && self.parse_timeout <= Duration::from_secs(120)
            && !self.orphan_grace.is_zero()
            && !self.claim_lease.is_zero()
            && !self.blob_operation_timeout.is_zero()
            && self.claim_lease > self.blob_operation_timeout
            && self.tombstone_retention > self.claim_lease
            && (1..=100).contains(&self.gc_batch_size)
            && (1..=4).contains(&self.gc_max_concurrency);
        valid.then_some(self).ok_or(ArtifactError::InvalidRequest)
    }
}

#[derive(Debug, Clone)]
pub struct AssembleArtifact {
    pub sha1: [u8; 20],
    pub chunks: Vec<[u8; 20]>,
    pub project_slugs: Vec<Box<str>>,
    pub release: Option<Box<str>>,
    pub dist: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssembleArtifactState {
    Missing { chunks: Vec<[u8; 20]> },
    Ok { bundle: ArtifactBundle },
    Error { code: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactError {
    #[error("artifact request is invalid")]
    InvalidRequest,
    #[error("artifact request is forbidden")]
    Forbidden,
    #[error("artifact target was not found")]
    NotFound,
    #[error("artifact bundle is malformed")]
    MalformedBundle,
    #[error("artifact archive exceeds a configured limit")]
    ArchiveLimit,
    #[error("artifact archive compression is unsupported")]
    UnsupportedCompression,
    #[error("artifact quota is exhausted")]
    Quota,
    #[error("artifact conflicts with existing content")]
    Conflict,
    #[error("artifact operation is already in progress")]
    Busy,
    #[error("artifact storage is temporarily unavailable")]
    Unavailable,
}

impl ArtifactError {
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::InvalidRequest => 1,
            Self::Forbidden => 2,
            Self::NotFound => 3,
            Self::MalformedBundle => 10,
            Self::ArchiveLimit => 11,
            Self::UnsupportedCompression => 12,
            Self::Quota => 20,
            Self::Conflict => 21,
            Self::Busy => 22,
            Self::Unavailable => 30,
        }
    }
}

pub struct ArtifactService {
    metadata: Arc<dyn ArtifactStore>,
    blobs: Arc<dyn BlobStore>,
    clock: Arc<dyn Clock>,
    random: Arc<dyn RandomSource>,
    config: ArtifactConfig,
    assemblies: Semaphore,
}

pub struct ArtifactCleanupTask {
    handle: tokio::task::JoinHandle<()>,
}

impl ArtifactCleanupTask {
    pub async fn wait(self) {
        let _ = self.handle.await;
    }
}

pub fn start_artifact_cleanup(
    service: Arc<ArtifactService>,
    interval: Duration,
    shutdown: ShutdownSignal,
) -> Result<ArtifactCleanupTask, ArtifactError> {
    if interval.is_zero() {
        return Err(ArtifactError::InvalidRequest);
    }
    let handle = tokio::spawn(async move {
        let mut ticks = tokio::time::interval(interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticks.tick().await;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = ticks.tick() => {
                    let _ = service.gc_once().await;
                    let _ = service.recover(100).await;
                }
            }
        }
    });
    Ok(ArtifactCleanupTask { handle })
}

impl ArtifactService {
    pub fn new(
        metadata: Arc<dyn ArtifactStore>,
        blobs: Arc<dyn BlobStore>,
        clock: Arc<dyn Clock>,
        random: Arc<dyn RandomSource>,
        config: ArtifactConfig,
    ) -> Result<Self, ArtifactError> {
        let config = config.validate()?;
        Ok(Self {
            metadata,
            blobs,
            clock,
            random,
            assemblies: Semaphore::new(config.maximum_concurrent_assemblies),
            config,
        })
    }

    pub async fn assemble(
        &self,
        context: &AuthContext,
        organization_slug: &str,
        request: AssembleArtifact,
    ) -> Result<AssembleArtifactState, ArtifactError> {
        require(context, Permission::ArtifactWrite)?;
        if request.chunks.is_empty()
            || request.chunks.len() > MAX_ARTIFACT_CHUNKS
            || request.project_slugs.is_empty()
            || request.project_slugs.len() > MAX_ARTIFACT_BINDINGS
            || request.release.as_ref().is_some_and(|value| {
                value.is_empty()
                    || value.len() > MAX_RELEASE_BYTES
                    || value.chars().any(char::is_control)
            })
            || request.dist.is_some() && request.release.is_none()
        {
            return Err(ArtifactError::InvalidRequest);
        }
        let mut project_slugs = request.project_slugs;
        project_slugs.sort();
        project_slugs.dedup();
        let (organization_id, projects) = self
            .metadata
            .resolve_projects(organization_slug.into(), project_slugs)
            .await
            .map_err(map_store)?;
        if organization_id != context.organization_id {
            return Err(ArtifactError::NotFound);
        }
        let release_id = request
            .release
            .as_deref()
            .map(|release| derive_release_id(organization_id, release));
        let bindings = projects
            .into_iter()
            .map(|project| {
                ArtifactBinding::new(project.id, release_id, request.dist.clone())
                    .map_err(|_| ArtifactError::InvalidRequest)
            })
            .collect::<Result<Vec<_>, _>>()?;
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
            return Ok(AssembleArtifactState::Missing { chunks: missing });
        }
        let now = self.clock.now();
        let upload = ArtifactUpload {
            id: upload_id(organization_id, request.sha1),
            organization_id,
            sha1: request.sha1,
            chunks: request.chunks,
            bindings,
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

    pub async fn recover(&self, limit: usize) -> Result<usize, ArtifactError> {
        let records = self
            .metadata
            .recoverable_uploads(limit)
            .await
            .map_err(map_store)?;
        let mut completed = 0_usize;
        for record in records {
            if let Ok(AssembleArtifactState::Ok { .. }) = self.assemble_record(record).await {
                completed = completed.saturating_add(1);
            }
        }
        Ok(completed)
    }

    pub async fn lookup(
        &self,
        project_id: ProjectId,
        debug_ids: Vec<DebugId>,
        release: Option<&str>,
        dist: Option<Box<str>>,
    ) -> Result<Vec<ArtifactCandidate>, ArtifactError> {
        if debug_ids.len() > MAX_ARTIFACT_DEBUG_IDS
            || release.is_some_and(|value| value.is_empty() || value.len() > MAX_RELEASE_BYTES)
            || dist.is_some() && release.is_none()
        {
            return Err(ArtifactError::InvalidRequest);
        }
        let organization_id = self
            .metadata
            .project_organization(project_id)
            .await
            .map_err(map_store)?;
        self.metadata
            .lookup(ArtifactLookup {
                project_id,
                organization_id,
                debug_ids,
                release_id: release.map(|value| derive_release_id(organization_id, value)),
                dist,
                limit: 20,
            })
            .await
            .map_err(map_store)
    }

    pub async fn open(
        &self,
        project_id: ProjectId,
        bundle_id: ArtifactBundleId,
    ) -> Result<(ArtifactBundle, Box<dyn BlobReadSession>), ArtifactError> {
        let bundle = self
            .metadata
            .load_for_project(project_id, bundle_id)
            .await
            .map_err(map_store)?;
        let reader = self
            .blobs
            .open(&BlobKey::artifact_bundle(
                bundle.organization_id,
                bundle.id,
                bundle.generation,
            ))
            .await
            .map_err(map_blob)?;
        Ok((bundle, reader))
    }

    pub async fn remove_binding(
        &self,
        context: &AuthContext,
        bundle_id: ArtifactBundleId,
        binding: ArtifactBinding,
    ) -> Result<bool, ArtifactError> {
        require(context, Permission::ArtifactDelete)?;
        let organization_id = self
            .metadata
            .project_organization(binding.project_id)
            .await
            .map_err(map_store)?;
        if organization_id != context.organization_id {
            return Err(ArtifactError::NotFound);
        }
        let orphan_at = add_duration(self.clock.now(), self.config.orphan_grace)?;
        self.metadata
            .remove_binding(organization_id, bundle_id, binding, orphan_at)
            .await
            .map(|revision| revision.is_some())
            .map_err(map_store)
    }

    pub async fn gc_once(&self) -> Result<usize, ArtifactError> {
        let now = self.clock.now();
        let lease_until = add_duration(now, self.config.claim_lease)?;
        let mut claim = [0_u8; 16];
        self.random
            .fill_bytes(&mut claim)
            .map_err(|_| ArtifactError::Unavailable)?;
        let claims = self
            .metadata
            .claim_gc(now, lease_until, claim, self.config.gc_batch_size)
            .await
            .map_err(map_store)?;
        futures_util::stream::iter(claims)
            .map(|claimed| self.gc_claimed(claimed))
            .buffer_unordered(self.config.gc_max_concurrency)
            .try_fold(0_usize, |completed, deleted| async move {
                Ok(completed + usize::from(deleted))
            })
            .await
    }

    async fn gc_claimed(&self, claimed: ArtifactGcClaim) -> Result<bool, ArtifactError> {
        let minimum_lease_until =
            add_duration(self.clock.now(), self.config.blob_operation_timeout)?;
        if !self
            .metadata
            .validate_gc_claim(
                claimed.bundle.id,
                claimed.bundle.generation,
                claimed.claim,
                minimum_lease_until,
            )
            .await
            .map_err(map_store)?
        {
            return Ok(false);
        }
        let key = BlobKey::artifact_bundle(
            claimed.bundle.organization_id,
            claimed.bundle.id,
            claimed.bundle.generation,
        );
        match tokio::time::timeout(self.config.blob_operation_timeout, self.blobs.delete(&key))
            .await
            .map_err(|_| ArtifactError::Unavailable)?
        {
            Ok(()) | Err(BlobStoreError::NotFound) => {}
            Err(error) => return Err(map_blob(error)),
        }
        let tombstone_until = add_duration(self.clock.now(), self.config.tombstone_retention)?;
        self.metadata
            .finish_gc(
                claimed.bundle.id,
                claimed.bundle.generation,
                claimed.claim,
                tombstone_until,
            )
            .await
            .map_err(map_store)
    }

    async fn assemble_record(
        &self,
        record: ArtifactUploadRecord,
    ) -> Result<AssembleArtifactState, ArtifactError> {
        if record.state == ArtifactUploadState::Complete {
            if let Some(mut bundle) = self
                .metadata
                .load_by_sha1(record.upload.organization_id, record.upload.sha1)
                .await
                .map_err(map_store)?
            {
                bundle.bindings = record.upload.bindings.clone();
                self.metadata
                    .publish_bundle(record.upload.id, bundle.clone())
                    .await
                    .map_err(map_store)?;
                return Ok(AssembleArtifactState::Ok { bundle });
            }
        }
        let _permit = self
            .assemblies
            .acquire()
            .await
            .map_err(|_| ArtifactError::Unavailable)?;
        self.metadata
            .set_upload_state(
                record.upload.id,
                ArtifactUploadState::Assembling,
                self.clock.now(),
                None,
                None,
            )
            .await
            .map_err(map_store)?;
        match self.assemble_bytes(&record.upload).await {
            Ok(bundle) => Ok(AssembleArtifactState::Ok { bundle }),
            Err(error) => {
                let permanent = matches!(
                    error,
                    ArtifactError::InvalidRequest
                        | ArtifactError::MalformedBundle
                        | ArtifactError::ArchiveLimit
                        | ArtifactError::UnsupportedCompression
                        | ArtifactError::Quota
                        | ArtifactError::Conflict
                );
                let _ = self
                    .metadata
                    .set_upload_state(
                        record.upload.id,
                        if permanent {
                            ArtifactUploadState::Failed
                        } else {
                            ArtifactUploadState::Pending
                        },
                        self.clock.now(),
                        None,
                        permanent.then_some(error.code()),
                    )
                    .await;
                if permanent {
                    Ok(AssembleArtifactState::Error { code: error.code() })
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn assemble_bytes(
        &self,
        upload: &ArtifactUpload,
    ) -> Result<ArtifactBundle, ArtifactError> {
        let maximum = usize::try_from(self.config.maximum_bundle_bytes)
            .map_err(|_| ArtifactError::ArchiveLimit)?;
        let mut bytes = Vec::with_capacity(maximum.min(1024 * 1024));
        let mut sha1 = Sha1::new();
        let mut blake3 = blake3::Hasher::new();
        for chunk in &upload.chunks {
            let mut reader = self
                .blobs
                .open(&BlobKey::debug_chunk(upload.organization_id, *chunk))
                .await
                .map_err(map_blob)?;
            while let Some(part) = reader
                .read_chunk(self.config.stream_chunk_bytes)
                .await
                .map_err(map_blob)?
            {
                if bytes
                    .len()
                    .checked_add(part.len())
                    .is_none_or(|size| size > maximum)
                {
                    return Err(ArtifactError::ArchiveLimit);
                }
                sha1.update(&part);
                blake3.update(&part);
                bytes.extend_from_slice(&part);
            }
        }
        let complete_sha1: [u8; 20] = sha1.finalize().into();
        if complete_sha1 != upload.sha1 || bytes.is_empty() {
            return Err(ArtifactError::InvalidRequest);
        }
        let checksum = *blake3.finalize().as_bytes();
        let config = self.config;
        let deadline = Instant::now() + config.parse_timeout;
        let validated =
            tokio::task::spawn_blocking(move || validate_source_bundle(&bytes, config, deadline))
                .await
                .map_err(|_| ArtifactError::Unavailable)??;
        if validated.debug_ids.is_empty()
            && upload
                .bindings
                .iter()
                .all(|binding| binding.release_id.is_none())
        {
            return Err(ArtifactError::InvalidRequest);
        }
        let generation = self
            .metadata
            .publication_generation(
                upload.organization_id,
                upload.sha1,
                upload.id,
                add_duration(self.clock.now(), self.config.claim_lease)?,
            )
            .await
            .map_err(map_store)?;
        let id = ArtifactBundleId::derive(upload.organization_id, checksum);
        let key = BlobKey::artifact_bundle(upload.organization_id, id, generation);
        let mut writer = self
            .blobs
            .begin(BlobKind::ArtifactBundle, self.clock.now())
            .await
            .map_err(map_blob)?;
        for part in validated.bytes.chunks(self.config.stream_chunk_bytes) {
            writer
                .write_chunk(part.to_vec().into_boxed_slice())
                .await
                .map_err(map_blob)?;
        }
        writer.commit(key).await.map_err(map_blob)?;
        let mut tokens = validated
            .debug_ids
            .iter()
            .map(|debug_id| ArtifactDebugIdToken::derive(upload.organization_id, debug_id))
            .collect::<Vec<_>>();
        tokens.sort();
        tokens.dedup();
        let bundle = ArtifactBundle {
            id,
            organization_id: upload.organization_id,
            bindings: upload.bindings.clone(),
            bundle_debug_id: validated.bundle_debug_id,
            debug_id_tokens: tokens,
            checksum,
            sha1: upload.sha1,
            size: u64::try_from(validated.bytes.len()).map_err(|_| ArtifactError::ArchiveLimit)?,
            uploaded_at: self.clock.now(),
            generation,
        };
        self.metadata
            .publish_bundle(upload.id, bundle.clone())
            .await
            .map_err(map_store)?;
        Ok(bundle)
    }
}

struct ValidatedBundle {
    bytes: Vec<u8>,
    bundle_debug_id: DebugId,
    debug_ids: Vec<DebugId>,
}

#[derive(Deserialize)]
struct SourceBundleManifest {
    #[serde(default)]
    files: BTreeMap<String, SourceFileInfo>,
    #[serde(flatten)]
    attributes: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct SourceFileInfo {
    #[serde(rename = "type")]
    ty: Option<String>,
    #[serde(default)]
    path: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

fn validate_source_bundle(
    bytes: &[u8],
    config: ArtifactConfig,
    deadline: Instant,
) -> Result<ValidatedBundle, ArtifactError> {
    if bytes.len() < 8 || &bytes[..4] != b"SYSB" {
        return Err(ArtifactError::MalformedBundle);
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().expect("bundle header version"));
    if !(1..=2).contains(&version) {
        return Err(ArtifactError::MalformedBundle);
    }
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| ArtifactError::MalformedBundle)?;
    if archive.is_empty() || archive.len() > config.maximum_entries.saturating_add(1) {
        return Err(ArtifactError::ArchiveLimit);
    }
    let mut names = BTreeSet::new();
    let mut logical = 0_u64;
    let mut compressed = 0_u64;
    for index in 0..archive.len() {
        check_deadline(deadline)?;
        let entry = archive
            .by_index(index)
            .map_err(|_| ArtifactError::MalformedBundle)?;
        let name = entry.name();
        if entry.is_dir()
            || name.is_empty()
            || name.len() > config.maximum_path_bytes
            || name.chars().any(char::is_control)
            || name.contains('\\')
            || entry.enclosed_name().is_none()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            || (name != "manifest.json" && !name.starts_with("files/"))
            || !names.insert(name.to_owned())
        {
            return Err(ArtifactError::MalformedBundle);
        }
        if !matches!(
            entry.compression(),
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
        ) {
            return Err(ArtifactError::UnsupportedCompression);
        }
        let limit = if name == "manifest.json" {
            config.maximum_manifest_bytes
        } else {
            config.maximum_entry_bytes
        };
        if entry.size() > limit {
            return Err(ArtifactError::ArchiveLimit);
        }
        logical = logical
            .checked_add(entry.size())
            .filter(|value| *value <= config.maximum_logical_bytes)
            .ok_or(ArtifactError::ArchiveLimit)?;
        compressed = compressed
            .checked_add(entry.compressed_size())
            .ok_or(ArtifactError::ArchiveLimit)?;
        if entry.size() > 0
            && (entry.compressed_size() == 0
                || entry.size()
                    > entry
                        .compressed_size()
                        .saturating_mul(config.maximum_compression_ratio))
        {
            return Err(ArtifactError::ArchiveLimit);
        }
    }
    if logical > 0
        && (compressed == 0
            || logical > compressed.saturating_mul(config.maximum_compression_ratio))
    {
        return Err(ArtifactError::ArchiveLimit);
    }
    let manifest_bytes = read_entry(&mut archive, "manifest.json", config.maximum_manifest_bytes)?;
    let manifest: SourceBundleManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| ArtifactError::MalformedBundle)?;
    if manifest.files.is_empty()
        || manifest.files.len() > config.maximum_entries
        || manifest.attributes.len() > config.maximum_metadata_fields
    {
        return Err(ArtifactError::ArchiveLimit);
    }
    let bundle_debug_id = manifest
        .attributes
        .get("debug_id")
        .and_then(serde_json::Value::as_str)
        .ok_or(ArtifactError::MalformedBundle)
        .and_then(|value| DebugId::parse(value).map_err(|_| ArtifactError::MalformedBundle))?;
    for value in manifest.attributes.values() {
        let Some(value) = value.as_str() else {
            return Err(ArtifactError::MalformedBundle);
        };
        if value.len() > config.maximum_url_bytes || value.chars().any(char::is_control) {
            return Err(ArtifactError::ArchiveLimit);
        }
    }
    let expected = manifest.files.keys().cloned().collect::<BTreeSet<_>>();
    let actual = names
        .iter()
        .filter(|name| name.as_str() != "manifest.json")
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(ArtifactError::MalformedBundle);
    }
    let mut debug_ids = Vec::new();
    for (path, info) in &manifest.files {
        check_deadline(deadline)?;
        if path.is_empty()
            || path.len() > config.maximum_path_bytes
            || path.contains('\\')
            || !path.starts_with("files/")
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || info.path.len() > config.maximum_path_bytes
            || info.url.len() > config.maximum_url_bytes
            || info.headers.len() > config.maximum_metadata_fields
            || info.headers.iter().any(|(key, value)| {
                key.is_empty()
                    || key.len() > 128
                    || value.len() > config.maximum_url_bytes
                    || key.chars().any(char::is_control)
                    || value.chars().any(char::is_control)
            })
        {
            return Err(ArtifactError::ArchiveLimit);
        }
        let ty = info.ty.as_deref().unwrap_or("source");
        if !matches!(
            ty,
            "source" | "minified_source" | "source_map" | "indexed_ram_bundle"
        ) {
            return Err(ArtifactError::MalformedBundle);
        }
        if let Some(value) = info
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("debug-id"))
            .map(|(_, value)| value)
        {
            debug_ids.push(DebugId::parse(value).map_err(|_| ArtifactError::MalformedBundle)?);
        }
        let entry_bytes = read_entry(&mut archive, path, config.maximum_entry_bytes)?;
        if ty != "indexed_ram_bundle" {
            std::str::from_utf8(&entry_bytes).map_err(|_| ArtifactError::MalformedBundle)?;
        }
        if ty == "source_map" {
            let map: serde_json::Value =
                serde_json::from_slice(&entry_bytes).map_err(|_| ArtifactError::MalformedBundle)?;
            if !map.is_object() {
                return Err(ArtifactError::MalformedBundle);
            }
        }
        if path.to_ascii_lowercase().ends_with(".zip") || entry_bytes.starts_with(b"SYSB") {
            return Err(ArtifactError::MalformedBundle);
        }
    }
    debug_ids.sort_by_key(ToString::to_string);
    debug_ids.dedup();
    if debug_ids.len() > MAX_ARTIFACT_DEBUG_IDS {
        return Err(ArtifactError::ArchiveLimit);
    }
    Ok(ValidatedBundle {
        bytes: bytes.to_vec(),
        bundle_debug_id,
        debug_ids,
    })
}

fn read_entry(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    maximum: u64,
) -> Result<Vec<u8>, ArtifactError> {
    let mut entry = archive
        .by_name(name)
        .map_err(|_| ArtifactError::MalformedBundle)?;
    let capacity =
        usize::try_from(entry.size().min(maximum)).map_err(|_| ArtifactError::ArchiveLimit)?;
    let mut bytes = Vec::with_capacity(capacity);
    entry
        .by_ref()
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ArtifactError::MalformedBundle)?;
    if u64::try_from(bytes.len()).map_or(true, |size| size > maximum) {
        return Err(ArtifactError::ArchiveLimit);
    }
    Ok(bytes)
}

fn check_deadline(deadline: Instant) -> Result<(), ArtifactError> {
    (Instant::now() <= deadline)
        .then_some(())
        .ok_or(ArtifactError::ArchiveLimit)
}

fn upload_id(organization_id: OrganizationId, sha1: [u8; 20]) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"artifact-upload-id-v1");
    hasher.update(&organization_id.get().to_be_bytes());
    hasher.update(&sha1);
    let mut id = [0_u8; 16];
    id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    id
}

fn require(context: &AuthContext, permission: Permission) -> Result<(), ArtifactError> {
    context
        .permissions
        .contains(permission)
        .then_some(())
        .ok_or(ArtifactError::Forbidden)
}

fn map_store(error: ArtifactStoreError) -> ArtifactError {
    match error {
        ArtifactStoreError::NotFound => ArtifactError::NotFound,
        ArtifactStoreError::Conflict => ArtifactError::Conflict,
        ArtifactStoreError::Quota => ArtifactError::Quota,
        ArtifactStoreError::Busy => ArtifactError::Busy,
        ArtifactStoreError::InvalidData => ArtifactError::Unavailable,
        ArtifactStoreError::Unavailable => ArtifactError::Unavailable,
    }
}

fn map_blob(error: BlobStoreError) -> ArtifactError {
    match error {
        BlobStoreError::TooLarge | BlobStoreError::Capacity => ArtifactError::ArchiveLimit,
        BlobStoreError::NotFound => ArtifactError::NotFound,
        BlobStoreError::Invalid => ArtifactError::InvalidRequest,
        BlobStoreError::Corrupt | BlobStoreError::Unavailable => ArtifactError::Unavailable,
    }
}

fn add_duration(value: Timestamp, duration: Duration) -> Result<Timestamp, ArtifactError> {
    Timestamp::from_unix_millis(
        value
            .unix_millis()
            .checked_add(
                i64::try_from(duration.as_millis()).map_err(|_| ArtifactError::InvalidRequest)?,
            )
            .ok_or(ArtifactError::InvalidRequest)?,
    )
    .map_err(|_| ArtifactError::InvalidRequest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn bundle(path: &str, manifest: serde_json::Value, body: &[u8]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        cursor.write_all(b"SYSB").unwrap();
        cursor.write_all(&2_u32.to_le_bytes()).unwrap();
        let mut writer = zip::ZipWriter::new(cursor);
        writer
            .start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        writer
            .start_file(format!("files/{path}"), SimpleFileOptions::default())
            .unwrap();
        writer.write_all(body).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn validates_compatible_bundle_and_extracts_debug_ids() {
        let debug_id = "67e9247c-814e-392b-a027-dbde6748fcbf";
        let bytes = bundle(
            "_/_/app.js",
            serde_json::json!({
                "debug_id": "11111111-2222-3333-4444-555555555555",
                "files": { "files/_/_/app.js": { "type": "minified_source", "url": "~/app.js", "headers": { "debug-id": debug_id } } }
            }),
            b"function a(){}",
        );
        let output = validate_source_bundle(
            &bytes,
            ArtifactConfig::default(),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(output.debug_ids[0].to_string(), debug_id);
    }

    #[test]
    fn rejects_traversal_duplicate_manifest_and_malformed_map() {
        let malformed = bundle(
            "../app.js.map",
            serde_json::json!({
                "debug_id": "11111111-2222-3333-4444-555555555555",
                "files": { "files/../app.js.map": { "type": "source_map" } }
            }),
            b"not-json",
        );
        assert!(
            validate_source_bundle(
                &malformed,
                ArtifactConfig::default(),
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
        );
        let malformed_map = bundle(
            "maps/app.js.map",
            serde_json::json!({
                "debug_id": "11111111-2222-3333-4444-555555555555",
                "files": { "files/maps/app.js.map": { "type": "source_map" } }
            }),
            b"not-json",
        );
        assert_eq!(
            validate_source_bundle(
                &malformed_map,
                ArtifactConfig::default(),
                Instant::now() + Duration::from_secs(1)
            )
            .err(),
            Some(ArtifactError::MalformedBundle)
        );
    }

    #[test]
    fn rejects_wrong_header_unlisted_entry_symlink_and_compression_bomb() {
        let manifest = serde_json::json!({
            "debug_id": "11111111-2222-3333-4444-555555555555",
            "files": { "files/app.js": { "type": "minified_source" } }
        });
        let mut wrong = bundle("app.js", manifest.clone(), b"ok");
        wrong[0] = b'X';
        assert!(
            validate_source_bundle(
                &wrong,
                ArtifactConfig::default(),
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
        );

        let mut cursor = Cursor::new(Vec::new());
        cursor.write_all(b"SYSB").unwrap();
        cursor.write_all(&2_u32.to_le_bytes()).unwrap();
        let mut writer = zip::ZipWriter::new(cursor);
        writer
            .start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        writer
            .start_file("files/app.js", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"ok").unwrap();
        writer
            .start_file("files/unlisted.js", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"hidden").unwrap();
        let unlisted = writer.finish().unwrap().into_inner();
        assert!(
            validate_source_bundle(
                &unlisted,
                ArtifactConfig::default(),
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
        );

        let mut cursor = Cursor::new(Vec::new());
        cursor.write_all(b"SYSB").unwrap();
        cursor.write_all(&2_u32.to_le_bytes()).unwrap();
        let mut writer = zip::ZipWriter::new(cursor);
        writer
            .start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        writer
            .start_file(
                "files/app.js",
                SimpleFileOptions::default().unix_permissions(0o120777),
            )
            .unwrap();
        writer.write_all(b"target").unwrap();
        let mut symlink = writer.finish().unwrap().into_inner();
        let central = symlink
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .unwrap();
        symlink[central + 5] = 3;
        symlink[central + 38..central + 42].copy_from_slice(&(0o120777_u32 << 16).to_le_bytes());
        assert!(
            validate_source_bundle(
                &symlink,
                ArtifactConfig::default(),
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
        );

        let mut cursor = Cursor::new(Vec::new());
        cursor.write_all(b"SYSB").unwrap();
        cursor.write_all(&2_u32.to_le_bytes()).unwrap();
        let mut writer = zip::ZipWriter::new(cursor);
        let compressed =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("manifest.json", compressed).unwrap();
        writer
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        writer.start_file("files/app.js", compressed).unwrap();
        writer.write_all(&vec![b'a'; 1024 * 1024]).unwrap();
        let bomb = writer.finish().unwrap().into_inner();
        let config = ArtifactConfig {
            maximum_compression_ratio: 2,
            ..ArtifactConfig::default()
        };
        assert_eq!(
            validate_source_bundle(&bomb, config, Instant::now() + Duration::from_secs(1)).err(),
            Some(ArtifactError::ArchiveLimit)
        );
    }
}
