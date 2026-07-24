//! Compact backend-independent values for JavaScript Artifact Bundles.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use thiserror::Error;

use crate::{OrganizationId, ProjectId, Timestamp, debug_files::DebugId, finalization::ReleaseId};

pub const MAX_ARTIFACT_BINDINGS: usize = 512;
pub const MAX_ARTIFACT_DEBUG_IDS: usize = 20_000;
pub const MAX_ARTIFACT_CHUNKS: usize = 64;
pub const MAX_ARTIFACT_DIST_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactValueError {
    #[error("artifact bundle identifier is invalid")]
    InvalidBundleId,
    #[error("artifact binding is invalid")]
    InvalidBinding,
    #[error("artifact upload manifest is invalid")]
    InvalidManifest,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactBundleId([u8; 16]);

impl ArtifactBundleId {
    #[must_use]
    pub fn derive(organization_id: OrganizationId, checksum: [u8; 32]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"artifact-bundle-id-v1");
        hasher.update(&organization_id.get().to_be_bytes());
        hasher.update(&checksum);
        let mut id = [0_u8; 16];
        id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        Self(id)
    }

    pub fn parse(value: &str) -> Result<Self, ArtifactValueError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| ArtifactValueError::InvalidBundleId)?;
        Ok(Self(
            bytes
                .try_into()
                .map_err(|_| ArtifactValueError::InvalidBundleId)?,
        ))
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Display for ArtifactBundleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for ArtifactBundleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArtifactBinding {
    pub project_id: ProjectId,
    pub release_id: Option<ReleaseId>,
    pub dist: Option<Box<str>>,
}

impl ArtifactBinding {
    pub fn new(
        project_id: ProjectId,
        release_id: Option<ReleaseId>,
        dist: Option<Box<str>>,
    ) -> Result<Self, ArtifactValueError> {
        if dist.as_ref().is_some_and(|value| {
            release_id.is_none()
                || value.is_empty()
                || value.len() > MAX_ARTIFACT_DIST_BYTES
                || value.chars().any(char::is_control)
        }) {
            return Err(ArtifactValueError::InvalidBinding);
        }
        Ok(Self {
            project_id,
            release_id,
            dist,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArtifactDebugIdToken(i64);

impl ArtifactDebugIdToken {
    #[must_use]
    pub fn derive(organization_id: OrganizationId, debug_id: &DebugId) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"js-artifact-debug-id-v1");
        hasher.update(&organization_id.get().to_be_bytes());
        hasher.update(&debug_id.encode());
        Self(i64::from_be_bytes(
            hasher.finalize().as_bytes()[..8]
                .try_into()
                .expect("BLAKE3 token prefix"),
        ))
    }

    #[must_use]
    pub const fn from_stored(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn stored(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactBundle {
    pub id: ArtifactBundleId,
    pub organization_id: OrganizationId,
    pub bindings: Vec<ArtifactBinding>,
    pub bundle_debug_id: DebugId,
    pub debug_id_tokens: Vec<ArtifactDebugIdToken>,
    pub checksum: [u8; 32],
    pub sha1: [u8; 20],
    pub size: u64,
    pub uploaded_at: Timestamp,
    pub generation: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactUpload {
    pub id: [u8; 16],
    pub organization_id: OrganizationId,
    pub sha1: [u8; 20],
    pub chunks: Vec<[u8; 20]>,
    pub bindings: Vec<ArtifactBinding>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactUploadState {
    Pending,
    Assembling,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactUploadRecord {
    pub upload: ArtifactUpload,
    pub state: ArtifactUploadState,
    pub attempts: u32,
    pub final_id: Option<ArtifactBundleId>,
    pub error_code: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactLookup {
    pub project_id: ProjectId,
    pub organization_id: OrganizationId,
    pub debug_ids: Vec<DebugId>,
    pub release_id: Option<ReleaseId>,
    pub dist: Option<Box<str>>,
    pub limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactResolution {
    DebugId,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactCandidate {
    pub bundle: ArtifactBundle,
    pub resolved_with: ArtifactResolution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactGcClaim {
    pub bundle: ArtifactBundle,
    pub claim: [u8; 16],
    pub lease_until: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_bind_organization_and_canonicalize() {
        let organization = OrganizationId::new(7).unwrap();
        let checksum = [9; 32];
        let id = ArtifactBundleId::derive(organization, checksum);
        assert_eq!(ArtifactBundleId::parse(&id.to_string()).unwrap(), id);
        assert_ne!(
            ArtifactBundleId::derive(OrganizationId::new(8).unwrap(), checksum),
            id
        );
        let debug_id = DebugId::parse("67e9247c-814e-392b-a027-dbde6748fcbf").unwrap();
        assert_ne!(
            ArtifactDebugIdToken::derive(organization, &debug_id),
            ArtifactDebugIdToken::derive(OrganizationId::new(8).unwrap(), &debug_id)
        );
    }

    #[test]
    fn dist_requires_release_and_is_bounded() {
        let project = ProjectId::new(1).unwrap();
        assert!(ArtifactBinding::new(project, None, Some("web".into())).is_err());
        assert!(ArtifactBundleId::parse("bad").is_err());
    }
}
