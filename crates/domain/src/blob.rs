//! Bounded identities and metadata for non-BSON binary objects.

use std::fmt;

use thiserror::Error;

use crate::{EventId, ProjectId, Timestamp};

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
}

impl BlobKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::EventAttachment => "event_attachment",
            Self::Minidump => "minidump",
        }
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
    }
}
