//! Stable identities and bounded values for optional cold Event archives.

use std::fmt;

use crate::{
    EventKey, ProjectId, Timestamp,
    blob::BlobKey,
    grouping::IssueId,
    sessions::SessionId,
    signals::{LogId, SpanRecordId},
};

/// Version-one Parquet archive schema.
pub const EVENT_ARCHIVE_SCHEMA_VERSION: u16 = 1;
pub const LOG_ARCHIVE_SCHEMA_VERSION: u16 = 1;
pub const SPAN_ARCHIVE_SCHEMA_VERSION: u16 = 1;
pub const SESSION_ARCHIVE_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArchiveKind {
    Event,
    Log,
    Span,
    Session,
}

impl ArchiveKind {
    pub const ALL: [Self; 4] = [Self::Event, Self::Log, Self::Span, Self::Session];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Log => "log",
            Self::Span => "span",
            Self::Session => "session",
        }
    }

    #[must_use]
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Event => "events",
            Self::Log => "logs",
            Self::Span => "spans",
            Self::Session => "sessions",
        }
    }

    pub fn from_name(value: &str) -> Result<Self, ArchiveValueError> {
        match value {
            "event" => Ok(Self::Event),
            "log" => Ok(Self::Log),
            "span" => Ok(Self::Span),
            "session" => Ok(Self::Session),
            _ => Err(ArchiveValueError),
        }
    }

    pub fn from_directory(value: &str) -> Result<Self, ArchiveValueError> {
        match value {
            "events" => Ok(Self::Event),
            "logs" => Ok(Self::Log),
            "spans" => Ok(Self::Span),
            "sessions" => Ok(Self::Session),
            _ => Err(ArchiveValueError),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveValueError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArchiveSourceId {
    Event(EventKey),
    Log(LogId),
    Span(SpanRecordId),
    Session(SessionId),
}

impl ArchiveSourceId {
    #[must_use]
    pub const fn kind(self) -> ArchiveKind {
        match self {
            Self::Event(_) => ArchiveKind::Event,
            Self::Log(_) => ArchiveKind::Log,
            Self::Span(_) => ArchiveKind::Span,
            Self::Session(_) => ArchiveKind::Session,
        }
    }

    #[must_use]
    pub fn as_bytes(self) -> Vec<u8> {
        match self {
            Self::Event(value) => value.as_bytes().to_vec(),
            Self::Log(value) => value.as_bytes().to_vec(),
            Self::Span(value) => value.as_bytes().to_vec(),
            Self::Session(value) => value.as_bytes().to_vec(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArchiveSegmentId([u8; 16]);

impl ArchiveSegmentId {
    #[must_use]
    pub fn derive(project_id: ProjectId, event_keys: &[EventKey]) -> Self {
        let source_ids = event_keys
            .iter()
            .copied()
            .map(ArchiveSourceId::Event)
            .collect::<Vec<_>>();
        Self::derive_sources(ArchiveKind::Event, project_id, &source_ids)
    }

    #[must_use]
    pub fn derive_sources(
        kind: ArchiveKind,
        project_id: ProjectId,
        source_ids: &[ArchiveSourceId],
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"metric-signal-archive-segment/v1");
        hasher.update(kind.name().as_bytes());
        hasher.update(&[0]);
        hasher.update(&project_id.get().to_be_bytes());
        for source_id in source_ids {
            hasher.update(&source_id.as_bytes());
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSignal {
    pub id: [u8; 16],
    pub project_id: ProjectId,
    pub received_at: Timestamp,
    pub occurred_at_ns: i64,
    pub canonical_payload: Box<[u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveRecords {
    Events(Vec<ArchiveEvent>),
    Logs(Vec<ArchiveSignal>),
    Spans(Vec<ArchiveSignal>),
    Sessions(Vec<ArchiveSignal>),
}

impl ArchiveRecords {
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Events(records) => records.len(),
            Self::Logs(records) | Self::Spans(records) | Self::Sessions(records) => records.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveBatchState {
    Writing,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveBatch {
    pub kind: ArchiveKind,
    pub segment_id: ArchiveSegmentId,
    pub project_id: ProjectId,
    pub received_from: Timestamp,
    pub received_to: Timestamp,
    pub object_key: BlobKey,
    pub source_ids: Vec<ArchiveSourceId>,
    pub records: ArchiveRecords,
    pub state: ArchiveBatchState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventId, signals::LogId};

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
        let log_ids = [
            ArchiveSourceId::Log(LogId::from_bytes([1; 16])),
            ArchiveSourceId::Log(LogId::from_bytes([2; 16])),
        ];
        assert_ne!(
            ArchiveSegmentId::derive(project, &keys),
            ArchiveSegmentId::derive_sources(ArchiveKind::Log, project, &log_ids)
        );
    }
}
