//! Durable finalization values and deterministic derived identities.

use std::{fmt, time::Duration};

use crate::{
    EventId, EventKey, OrganizationId, ProjectId, Timestamp,
    event::{EventLevel, EventPlatform},
    grouping::IssueId,
    issue::IssueOccurrence,
};

pub const MAX_SEARCH_TOKENS_PER_EVENT: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct ProcessedEventPayload(Box<[u8]>);

impl ProcessedEventPayload {
    #[must_use]
    pub fn new(bytes: impl Into<Box<[u8]>>) -> Self {
        Self(bytes.into())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ProcessedEventPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessedEventPayload")
            .field("bytes", &self.0.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SearchToken(i64);

impl SearchToken {
    #[must_use]
    pub const fn from_stored(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn stored(self) -> i64 {
        self.0
    }

    #[must_use]
    pub fn environment(value: &str) -> Self {
        token(b"search/environment/v1", &[value.as_bytes()])
    }

    #[must_use]
    pub fn release(value: &str) -> Self {
        token(b"search/release/v1", &[value.as_bytes()])
    }

    #[must_use]
    pub fn user_id(value: &str) -> Self {
        token(b"search/user-id/v1", &[value.as_bytes()])
    }

    #[must_use]
    pub fn tag_pair(key: &str, value: &str) -> Self {
        token(b"search/tag-pair/v1", &[key.as_bytes(), value.as_bytes()])
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseId([u8; 16]);

impl ReleaseId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for ReleaseId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ReleaseId({})", hex::encode(self.0))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnvironmentId([u8; 16]);

impl EnvironmentId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for EnvironmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "EnvironmentId({})", hex::encode(self.0))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HourBucketId([u8; 16]);

impl HourBucketId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for HourBucketId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HourBucketId({})", hex::encode(self.0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeEvent {
    pub project_id: ProjectId,
    pub event_id: EventId,
    pub received_at: Timestamp,
    pub occurred_at: Timestamp,
    pub level: EventLevel,
    pub platform: EventPlatform,
    pub issue: IssueOccurrence,
    pub environment: Option<Box<str>>,
    pub search_tokens: Vec<SearchToken>,
    pub payload: ProcessedEventPayload,
}

impl FinalizeEvent {
    #[must_use]
    pub fn key(&self) -> EventKey {
        EventKey::new(self.project_id, self.event_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeBatch {
    pub events: Vec<FinalizeEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizationPolicy {
    pub event_retention: Duration,
    pub hourly_retention: Duration,
    pub archive_events: bool,
    pub max_implicit_releases_per_project_day: u32,
    pub max_implicit_environments_per_project: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizeResult {
    pub requested: usize,
    pub pending: usize,
    pub finalized: usize,
    pub skipped_completed: usize,
}

#[must_use]
pub fn derive_release_id(organization_id: OrganizationId, exact_version: &str) -> ReleaseId {
    ReleaseId(derive_id(
        b"release-id/v1",
        &[
            &organization_id.get().to_be_bytes(),
            exact_version.as_bytes(),
        ],
    ))
}

#[must_use]
pub fn derive_environment_id(project_id: ProjectId, exact_name: &str) -> EnvironmentId {
    EnvironmentId(derive_id(
        b"environment-id/v1",
        &[&project_id.get().to_be_bytes(), exact_name.as_bytes()],
    ))
}

#[must_use]
pub fn hour_start(timestamp: Timestamp) -> Timestamp {
    const HOUR_MILLIS: i64 = 60 * 60 * 1_000;
    let millis = timestamp.unix_millis();
    let start = millis.div_euclid(HOUR_MILLIS) * HOUR_MILLIS;
    Timestamp::from_unix_millis(start).expect("an in-range timestamp has an in-range hour start")
}

#[must_use]
pub fn derive_hour_bucket_id(
    project_id: ProjectId,
    issue_id: IssueId,
    bucket_start: Timestamp,
) -> HourBucketId {
    HourBucketId(derive_id(
        b"issue-hour/v1",
        &[
            &project_id.get().to_be_bytes(),
            &issue_id.as_bytes(),
            &bucket_start.unix_millis().to_be_bytes(),
        ],
    ))
}

fn token(domain: &[u8], values: &[&[u8]]) -> SearchToken {
    let digest = derive_digest(domain, values);
    SearchToken(i64::from_be_bytes(
        digest[..8].try_into().expect("eight-byte token prefix"),
    ))
}

fn derive_id(domain: &[u8], values: &[&[u8]]) -> [u8; 16] {
    let digest = derive_digest(domain, values);
    digest[..16]
        .try_into()
        .expect("sixteen-byte identity prefix")
}

fn derive_digest(domain: &[u8], values: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, domain);
    for value in values {
        hash_part(&mut hasher, value);
    }
    *hasher.finalize().as_bytes()
}

fn hash_part(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_identity_and_search_token_goldens_are_pinned() {
        let organization = OrganizationId::new(42).unwrap();
        let project = ProjectId::new(7).unwrap();
        let issue = IssueId::from_bytes([9; 16]);
        let hour = Timestamp::from_unix_millis(1_700_000_123_456).unwrap();
        assert_eq!(
            hex::encode(derive_release_id(organization, "backend@1.4.2").as_bytes()),
            "163bd55b89d766145955c0d157182803"
        );
        assert_eq!(
            hex::encode(derive_environment_id(project, "production").as_bytes()),
            "c82ef02b6e811f1b855ace099977eada"
        );
        assert_eq!(hour_start(hour).unix_millis(), 1_699_999_200_000);
        assert_eq!(
            hex::encode(derive_hour_bucket_id(project, issue, hour_start(hour)).as_bytes()),
            "348f7cb4e12281dee8800896a7ffd7f0"
        );
        assert_eq!(
            SearchToken::release("backend@1.4.2").stored(),
            6280831570156352842
        );
        assert_ne!(
            SearchToken::environment("production"),
            SearchToken::release("production")
        );
    }

    #[test]
    fn hour_floor_handles_pre_epoch_timestamps() {
        assert_eq!(
            hour_start(Timestamp::from_unix_millis(-1).unwrap()).unix_millis(),
            -3_600_000
        );
    }
}
