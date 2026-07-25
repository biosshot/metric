//! Stable identities and bounded values for optional cold Event archives.

use std::fmt;

use crate::{EventKey, ProjectId, Timestamp, blob::BlobKey, grouping::IssueId};

/// Version-one Parquet archive schema.
pub const EVENT_ARCHIVE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArchiveSegmentId([u8; 16]);

impl ArchiveSegmentId {
    #[must_use]
    pub fn derive(project_id: ProjectId, event_keys: &[EventKey]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"metric-event-archive-segment/v1");
        hasher.update(&project_id.get().to_be_bytes());
        for key in event_keys {
            hasher.update(&key.as_bytes());
        }
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        Self(bytes)
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

impl fmt::Display for ArchiveSegmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for ArchiveSegmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEvent {
    pub key: EventKey,
    pub project_id: ProjectId,
    pub received_at: Timestamp,
    pub occurred_at: Timestamp,
    pub issue_id: Option<IssueId>,
    pub canonical_payload: Box<[u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveBatchState {
    Writing,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveBatch {
    pub segment_id: ArchiveSegmentId,
    pub project_id: ProjectId,
    pub received_from: Timestamp,
    pub received_to: Timestamp,
    pub object_key: BlobKey,
    pub event_keys: Vec<EventKey>,
    pub events: Vec<ArchiveEvent>,
    pub state: ArchiveBatchState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventId;

    #[test]
    fn segment_identity_is_ordered_and_deterministic() {
        let project = ProjectId::new(7).unwrap();
        let keys = [
            EventKey::new(project, EventId::from_bytes([1; 16])),
            EventKey::new(project, EventId::from_bytes([2; 16])),
        ];
        assert_eq!(
            ArchiveSegmentId::derive(project, &keys),
            ArchiveSegmentId::derive(project, &keys)
        );
        assert_ne!(
            ArchiveSegmentId::derive(project, &keys),
            ArchiveSegmentId::derive(project, &[keys[1], keys[0]])
        );
    }
}
