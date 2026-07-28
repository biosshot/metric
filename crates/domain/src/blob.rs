//! Bounded identities and metadata for non-BSON binary objects.

use std::fmt;

use thiserror::Error;

use crate::{
    EventId, OrganizationId, ProjectId, Timestamp,
    archive::{ArchiveKind, ArchiveSegmentId},
    artifacts::ArtifactBundleId,
    debug_files::DebugFileId,
};

pub const MAX_BLOB_KEY_BYTES: usize = 512;
pub const MAX_ATTACHMENT_FILENAME_BYTES: usize = 128;
pub const MAX_CONTENT_TYPE_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BlobValueError {
    #[error("blob key is invalid")]
    InvalidKey,
    #[error("attachment filename is invalid")]
    InvalidFilename,
    #[error("attachment content type is invalid")]
    InvalidContentType,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobKey(Box<str>);

impl BlobKey {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, BlobValueError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_BLOB_KEY_BYTES
            && !value.starts_with('/')
            && !value.ends_with('/')
            && !value.contains('\\')
            && value.split('/').all(valid_segment);
        valid
            .then_some(Self(value))
            .ok_or(BlobValueError::InvalidKey)
    }

    pub fn event_owned(project_id: ProjectId, event_id: EventId, object_id: BlobObjectId) -> Self {
        Self(
            format!(
                "projects/{}/events/{}/{}",
                project_id.get(),
                event_id,
                object_id
            )
            .into(),
        )
    }

    #[must_use]
    pub fn debug_chunk(organization_id: OrganizationId, sha1: [u8; 20]) -> Self {
        Self(
            format!(
                "debug-chunks/{}/{}",
                organization_id.get(),
                hex::encode(sha1)
            )
            .into(),
        )
    }

    #[must_use]
    pub fn debug_file(project_id: ProjectId, file_id: DebugFileId) -> Self {
        Self(format!("d/{}/{}", base36(project_id.get()), file_id).into())
    }

    #[must_use]
    pub fn artifact_bundle(
        organization_id: OrganizationId,
        bundle_id: ArtifactBundleId,
        generation: u32,
    ) -> Self {
        let suffix = if generation == 0 {
            String::new()
        } else {
            format!("/{}", base36_u32(generation))
        };
        Self(
            format!(
                "a/{}/{}{}",
                base36_u64(organization_id.get()),
                bundle_id,
                suffix
            )
            .into(),
        )
    }

    #[must_use]
    pub fn event_archive(
        project_id: ProjectId,
        year: i32,
        month: u8,
        day: u8,
        segment_id: ArchiveSegmentId,
    ) -> Self {
        Self::archive(ArchiveKind::Event, project_id, year, month, day, segment_id)
    }

    #[must_use]
    pub fn archive(
        kind: ArchiveKind,
        project_id: ProjectId,
        year: i32,
        month: u8,
        day: u8,
        segment_id: ArchiveSegmentId,
    ) -> Self {
        Self(
            format!(
                "projects/{}/archives/{}/{year:04}/{month:02}/{day:02}/{segment_id}.parquet",
                project_id.get(),
                kind.directory()
            )
            .into(),
        )
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn event_relation(&self) -> Result<(ProjectId, EventId, BlobObjectId), BlobValueError> {
        let segments = self.0.split('/').collect::<Vec<_>>();
        if segments.len() != 5 || segments[0] != "projects" || segments[2] != "events" {
            return Err(BlobValueError::InvalidKey);
        }
        let project_id = segments[1]
            .parse::<i32>()
            .ok()
            .and_then(|value| ProjectId::new(value).ok())
            .ok_or(BlobValueError::InvalidKey)?;
        let event_id = EventId::parse(segments[3]).map_err(|_| BlobValueError::InvalidKey)?;
        let object_id = BlobObjectId::parse(segments[4])?;
        Ok((project_id, event_id, object_id))
    }

    pub fn debug_file_relation(&self) -> Result<(ProjectId, DebugFileId), BlobValueError> {
        let segments = self.0.split('/').collect::<Vec<_>>();
        if segments.len() != 3 || segments[0] != "d" {
            return Err(BlobValueError::InvalidKey);
        }
        let project_value =
            i32::from_str_radix(segments[1], 36).map_err(|_| BlobValueError::InvalidKey)?;
        let project_id = ProjectId::new(project_value).map_err(|_| BlobValueError::InvalidKey)?;
        let file_id = DebugFileId::parse(segments[2]).map_err(|_| BlobValueError::InvalidKey)?;
        Ok((project_id, file_id))
    }

    pub fn archive_project(&self) -> Result<ProjectId, BlobValueError> {
        self.archive_relation().map(|(project_id, _)| project_id)
    }

    pub fn archive_relation(&self) -> Result<(ProjectId, ArchiveKind), BlobValueError> {
        let segments = self.0.split('/').collect::<Vec<_>>();
        if segments.len() != 8
            || segments[0] != "projects"
            || segments[2] != "archives"
            || !segments[7].ends_with(".parquet")
        {
            return Err(BlobValueError::InvalidKey);
        }
        let project_id = segments[1]
            .parse::<i32>()
            .ok()
            .and_then(|value| ProjectId::new(value).ok())
            .ok_or(BlobValueError::InvalidKey)?;
        let kind =
            ArchiveKind::from_directory(segments[3]).map_err(|_| BlobValueError::InvalidKey)?;
        Ok((project_id, kind))
    }
}

impl fmt::Debug for BlobKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("BlobKey").field(&self.0).finish()
    }
}

impl fmt::Display for BlobKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobObjectId([u8; 16]);

impl BlobObjectId {
    pub fn parse(value: &str) -> Result<Self, BlobValueError> {
        if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BlobValueError::InvalidKey);
        }
        let mut bytes = [0_u8; 16];
        hex::decode_to_slice(value, &mut bytes).map_err(|_| BlobValueError::InvalidKey)?;
        Ok(Self(bytes))
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

impl fmt::Display for BlobObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", hex::encode(self.0))
    }
}

impl fmt::Debug for BlobObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlobChecksum([u8; 32]);

impl BlobChecksum {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for BlobChecksum {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", hex::encode(self.0))
    }
}

impl fmt::Debug for BlobChecksum {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BlobChecksum(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobKind {
    EventAttachment,
    Minidump,
    DebugChunk,
    DebugFile,
    ArtifactBundle,
    EventArchive,
    LogArchive,
    SpanArchive,
    SessionArchive,
    MetricArchive,
}

impl BlobKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::EventAttachment => "event_attachment",
            Self::Minidump => "minidump",
            Self::DebugChunk => "debug_chunk",
            Self::DebugFile => "debug_file",
            Self::ArtifactBundle => "artifact_bundle",
            Self::EventArchive => "event_archive",
            Self::LogArchive => "log_archive",
            Self::SpanArchive => "span_archive",
            Self::SessionArchive => "session_archive",
            Self::MetricArchive => "metric_archive",
        }
    }

    #[must_use]
    pub const fn archive(kind: ArchiveKind) -> Self {
        match kind {
            ArchiveKind::Event => Self::EventArchive,
            ArchiveKind::Log => Self::LogArchive,
            ArchiveKind::Span => Self::SpanArchive,
            ArchiveKind::Session => Self::SessionArchive,
            ArchiveKind::Metric => Self::MetricArchive,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobNamespace {
    EventOwned,
    DebugChunks,
    DebugFiles,
    ArtifactBundles,
    EventArchives,
    LogArchives,
    SpanArchives,
    SessionArchives,
    MetricArchives,
}

impl BlobNamespace {
    #[must_use]
    pub const fn prefix(&self) -> &'static str {
        match self {
            Self::EventOwned => "projects/",
            Self::DebugChunks => "debug-chunks/",
            Self::DebugFiles => "d/",
            Self::ArtifactBundles => "a/",
            Self::EventArchives => "projects/",
            Self::LogArchives => "projects/",
            Self::SpanArchives => "projects/",
            Self::SessionArchives => "projects/",
            Self::MetricArchives => "projects/",
        }
    }

    #[must_use]
    pub const fn archive(kind: ArchiveKind) -> Self {
        match kind {
            ArchiveKind::Event => Self::EventArchives,
            ArchiveKind::Log => Self::LogArchives,
            ArchiveKind::Span => Self::SpanArchives,
            ArchiveKind::Session => Self::SessionArchives,
            ArchiveKind::Metric => Self::MetricArchives,
        }
    }

    pub fn kind_for_key(&self, key: &BlobKey) -> Result<BlobKind, BlobValueError> {
        if !key.as_str().starts_with(self.prefix()) {
            return Err(BlobValueError::InvalidKey);
        }
        Ok(match self {
            Self::EventOwned => {
                key.event_relation()?;
                BlobKind::EventAttachment
            }
            Self::DebugChunks => BlobKind::DebugChunk,
            Self::DebugFiles => BlobKind::DebugFile,
            Self::ArtifactBundles => BlobKind::ArtifactBundle,
            Self::EventArchives => {
                archive_kind(key, ArchiveKind::Event)?;
                BlobKind::EventArchive
            }
            Self::LogArchives => {
                archive_kind(key, ArchiveKind::Log)?;
                BlobKind::LogArchive
            }
            Self::SpanArchives => {
                archive_kind(key, ArchiveKind::Span)?;
                BlobKind::SpanArchive
            }
            Self::SessionArchives => {
                archive_kind(key, ArchiveKind::Session)?;
                BlobKind::SessionArchive
            }
            Self::MetricArchives => {
                archive_kind(key, ArchiveKind::Metric)?;
                BlobKind::MetricArchive
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobObject {
    pub key: BlobKey,
    pub kind: BlobKind,
    pub size: u64,
    pub checksum: BlobChecksum,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentFilename(Box<str>);

impl AttachmentFilename {
    pub fn sanitized(value: &str) -> Result<Self, BlobValueError> {
        let leaf = value
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        let valid = !leaf.is_empty() && leaf.len() <= MAX_ATTACHMENT_FILENAME_BYTES;
        valid
            .then(|| Self(leaf.into()))
            .ok_or(BlobValueError::InvalidFilename)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobContentType(Box<str>);

impl BlobContentType {
    pub fn new(value: &str) -> Result<Self, BlobValueError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_CONTENT_TYPE_BYTES
            && value.is_ascii()
            && !value.chars().any(char::is_control);
        valid
            .then(|| Self(value.into()))
            .ok_or(BlobValueError::InvalidContentType)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventAttachment {
    pub attachment_id: BlobObjectId,
    pub blob: BlobObject,
    pub filename: AttachmentFilename,
    pub content_type: BlobContentType,
    pub attachment_type: Box<str>,
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment.len() <= 128
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
}

fn archive_kind(key: &BlobKey, expected: ArchiveKind) -> Result<(), BlobValueError> {
    let (_, actual) = key.archive_relation()?;
    (actual == expected)
        .then_some(())
        .ok_or(BlobValueError::InvalidKey)
}

fn base36(value: i32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut value = u32::try_from(value).expect("ProjectId is positive");
    let mut output = Vec::new();
    while value > 0 {
        output.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    output.reverse();
    String::from_utf8(output).expect("base36 is ASCII")
}

fn base36_u64(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut output = Vec::new();
    while value > 0 {
        output.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    output.reverse();
    String::from_utf8(output).expect("base36 is ASCII")
}

fn base36_u32(value: u32) -> String {
    base36_u64(u64::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_keys_and_display_names_reject_traversal() {
        for invalid in [
            "../secret",
            "projects/1/../../secret",
            "/absolute",
            r"projects\1\secret",
            "projects//secret",
        ] {
            assert_eq!(BlobKey::new(invalid), Err(BlobValueError::InvalidKey));
        }
        assert_eq!(
            AttachmentFilename::sanitized("../../safe.txt")
                .unwrap()
                .as_str(),
            "safe.txt"
        );
        let key = BlobKey::event_owned(
            ProjectId::new(7).unwrap(),
            EventId::from_bytes([1; 16]),
            BlobObjectId::from_bytes([2; 16]),
        );
        assert_eq!(key.event_relation().unwrap().0, ProjectId::new(7).unwrap());

        let segment = ArchiveSegmentId::derive(
            ProjectId::new(7).unwrap(),
            &[crate::EventKey::new(
                ProjectId::new(7).unwrap(),
                EventId::from_bytes([3; 16]),
            )],
        );
        for kind in ArchiveKind::ALL {
            let key = BlobKey::archive(kind, ProjectId::new(7).unwrap(), 2026, 7, 26, segment);
            assert_eq!(
                key.archive_relation().unwrap(),
                (ProjectId::new(7).unwrap(), kind)
            );
            assert_eq!(
                BlobNamespace::archive(kind).kind_for_key(&key).unwrap(),
                BlobKind::archive(kind)
            );
        }
    }
}
