//! Bounded Session Replay metadata. Recording bytes remain opaque BlobStore objects.

use thiserror::Error;

use crate::{EventId, ProjectId, Timestamp, blob::BlobObject, signals::TraceId};

pub const MAX_REPLAY_SEGMENTS: u32 = 100;
pub const MAX_REPLAY_CORRELATIONS: usize = 100;
pub const MAX_REPLAY_TEXT_BYTES: usize = 256;
pub const MAX_REPLAY_DURATION_MILLIS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReplayValueError {
    #[error("Replay value is invalid or exceeds a bound")]
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayMetadata {
    pub project_id: ProjectId,
    pub replay_id: EventId,
    pub segment_id: u32,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub received_at: Timestamp,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub url: Option<Box<str>>,
    pub error_ids: Vec<EventId>,
    pub trace_ids: Vec<TraceId>,
}

impl ReplayMetadata {
    pub fn validate(&self) -> Result<(), ReplayValueError> {
        let duration = self
            .ended_at
            .unix_millis()
            .checked_sub(self.started_at.unix_millis())
            .ok_or(ReplayValueError::Invalid)?;
        if self.segment_id >= MAX_REPLAY_SEGMENTS
            || !(0..=MAX_REPLAY_DURATION_MILLIS).contains(&duration)
            || self.error_ids.len() > MAX_REPLAY_CORRELATIONS
            || self.trace_ids.len() > MAX_REPLAY_CORRELATIONS
        {
            return Err(ReplayValueError::Invalid);
        }
        validate_optional(&self.environment)?;
        validate_optional(&self.release)?;
        validate_optional(&self.url)?;
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ReplaySubmission {
    pub metadata: ReplayMetadata,
    pub recording: Box<[u8]>,
    pub decompressed_bytes: u64,
    pub event_count: u32,
}

impl std::fmt::Debug for ReplaySubmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplaySubmission")
            .field("metadata", &self.metadata)
            .field("recording_bytes", &self.recording.len())
            .field("decompressed_bytes", &self.decompressed_bytes)
            .field("event_count", &self.event_count)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySegment {
    pub segment_id: u32,
    pub object: BlobObject,
    pub decompressed_bytes: u64,
    pub event_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySegmentCommit {
    pub metadata: ReplayMetadata,
    pub segment: ReplaySegment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRecord {
    pub project_id: ProjectId,
    pub replay_id: EventId,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub received_at: Timestamp,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub url: Option<Box<str>>,
    pub error_ids: Vec<EventId>,
    pub trace_ids: Vec<TraceId>,
    pub segments: Vec<ReplaySegment>,
    pub expires_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayCursor {
    pub received_at: Timestamp,
    pub replay_id: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayPage {
    pub items: Vec<ReplayRecord>,
    pub next: Option<ReplayCursor>,
}

fn validate_optional(value: &Option<Box<str>>) -> Result<(), ReplayValueError> {
    if value.as_ref().is_some_and(|value| {
        value.is_empty()
            || value.len() > MAX_REPLAY_TEXT_BYTES
            || value.chars().any(char::is_control)
    }) {
        return Err(ReplayValueError::Invalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_rejects_unbounded_segment_and_duration() {
        let mut metadata = ReplayMetadata {
            project_id: ProjectId::new(7).unwrap(),
            replay_id: EventId::from_bytes([1; 16]),
            segment_id: 0,
            started_at: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
            ended_at: Timestamp::from_unix_millis(1_700_000_001_000).unwrap(),
            received_at: Timestamp::from_unix_millis(1_700_000_001_001).unwrap(),
            environment: Some("production".into()),
            release: None,
            url: Some("https://example.test/orders".into()),
            error_ids: Vec::new(),
            trace_ids: Vec::new(),
        };
        assert_eq!(metadata.validate(), Ok(()));
        metadata.segment_id = MAX_REPLAY_SEGMENTS;
        assert_eq!(metadata.validate(), Err(ReplayValueError::Invalid));
    }
}
