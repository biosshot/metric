//! Transport- and adapter-independent bounded primitives.

pub mod api;
pub mod archive;
pub mod artifacts;
pub mod auth;
pub mod blob;
pub mod debug_files;
pub mod deletion;
pub mod event;
pub mod finalization;
pub mod grouping;
pub mod inbound_filter;
pub mod issue;
pub mod notifications;
pub mod processing;
pub mod signals;
pub mod symbolication;

use std::{
    fmt,
    num::{NonZeroI32, NonZeroU32, NonZeroU64},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use thiserror::Error;

const MAX_ERROR_CODE_BYTES: usize = 64;
const MIN_TIMESTAMP_MILLIS: i64 = -62_135_596_800_000;
const MAX_TIMESTAMP_MILLIS: i64 = 253_402_300_799_999;

/// Validation failures shared by the foundation value types.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PrimitiveError {
    #[error("value must not be empty")]
    Empty,
    #[error("value contains a control character")]
    ControlCharacter,
    #[error("value is {actual} bytes; maximum is {maximum}")]
    TooLong { actual: usize, maximum: usize },
    #[error("value {actual} exceeds maximum {maximum}")]
    AboveMaximum { actual: u64, maximum: u64 },
    #[error("invalid byte size")]
    InvalidByteSize,
    #[error("invalid duration")]
    InvalidDuration,
    #[error("timestamp is outside the supported UTC range")]
    TimestampOutOfRange,
    #[error("invalid stable error code")]
    InvalidErrorCode,
    #[error("invalid project identifier")]
    InvalidProjectId,
    #[error("invalid DSN key")]
    InvalidDsnKey,
    #[error("invalid Event identifier")]
    InvalidEventId,
    #[error("invalid organization identifier")]
    InvalidOrganizationId,
    #[error("invalid slug")]
    InvalidSlug,
}

/// Non-empty UTF-8 identifier with a compile-time byte bound.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedId<const MAX_BYTES: usize>(Box<str>);

impl<const MAX_BYTES: usize> BoundedId<MAX_BYTES> {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, PrimitiveError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PrimitiveError::Empty);
        }
        if value.len() > MAX_BYTES {
            return Err(PrimitiveError::TooLong {
                actual: value.len(),
                maximum: MAX_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(PrimitiveError::ControlCharacter);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const MAX_BYTES: usize> fmt::Debug for BoundedId<MAX_BYTES> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("BoundedId").field(&self.0).finish()
    }
}

impl<const MAX_BYTES: usize> fmt::Display for BoundedId<MAX_BYTES> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<const MAX_BYTES: usize> FromStr for BoundedId<MAX_BYTES> {
    type Err = PrimitiveError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// Byte count with an owner-selected compile-time upper bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize<const MAX_BYTES: u64>(u64);

impl<const MAX_BYTES: u64> ByteSize<MAX_BYTES> {
    pub fn new(bytes: u64) -> Result<Self, PrimitiveError> {
        if bytes > MAX_BYTES {
            return Err(PrimitiveError::AboveMaximum {
                actual: bytes,
                maximum: MAX_BYTES,
            });
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<const MAX_BYTES: u64> FromStr for ByteSize<MAX_BYTES> {
    type Err = PrimitiveError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let split = value
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(value.len());
        let (number, unit) = value.split_at(split);
        let number = number
            .parse::<u64>()
            .map_err(|_| PrimitiveError::InvalidByteSize)?;
        let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
            "" | "b" => 1,
            "kb" => 1_000,
            "mb" => 1_000_000,
            "gb" => 1_000_000_000,
            "kib" => 1_024,
            "mib" => 1_048_576,
            "gib" => 1_073_741_824,
            _ => return Err(PrimitiveError::InvalidByteSize),
        };
        let bytes = number
            .checked_mul(multiplier)
            .ok_or(PrimitiveError::InvalidByteSize)?;
        Self::new(bytes)
    }
}

/// Duration with an owner-selected compile-time upper bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedDuration<const MAX_MILLIS: u64>(Duration);

impl<const MAX_MILLIS: u64> BoundedDuration<MAX_MILLIS> {
    pub fn new(duration: Duration) -> Result<Self, PrimitiveError> {
        let millis =
            u64::try_from(duration.as_millis()).map_err(|_| PrimitiveError::InvalidDuration)?;
        if millis > MAX_MILLIS {
            return Err(PrimitiveError::AboveMaximum {
                actual: millis,
                maximum: MAX_MILLIS,
            });
        }
        Ok(Self(duration))
    }

    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl<const MAX_MILLIS: u64> FromStr for BoundedDuration<MAX_MILLIS> {
    type Err = PrimitiveError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let duration =
            humantime::parse_duration(value).map_err(|_| PrimitiveError::InvalidDuration)?;
        Self::new(duration)
    }
}

/// UTC timestamp represented as Unix epoch milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    pub fn from_unix_millis(value: i64) -> Result<Self, PrimitiveError> {
        if !(MIN_TIMESTAMP_MILLIS..=MAX_TIMESTAMP_MILLIS).contains(&value) {
            return Err(PrimitiveError::TimestampOutOfRange);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn unix_millis(self) -> i64 {
        self.0
    }
}

/// Bounded machine-readable error code suitable for public contracts.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ErrorCode(Box<str>);

impl ErrorCode {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, PrimitiveError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_ERROR_CODE_BYTES
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            });
        if !valid {
            return Err(PrimitiveError::InvalidErrorCode);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ErrorCode").field(&self.0).finish()
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable project identifier shared by Sentry paths and persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectId(NonZeroI32);

impl ProjectId {
    pub fn new(value: i32) -> Result<Self, PrimitiveError> {
        NonZeroI32::new(value)
            .filter(|value| value.get().is_positive())
            .map(Self)
            .ok_or(PrimitiveError::InvalidProjectId)
    }

    #[must_use]
    pub const fn get(self) -> i32 {
        self.0.get()
    }
}

/// Random positive 63-bit organization identifier stored as BSON `int64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrganizationId(NonZeroU64);

impl OrganizationId {
    pub fn new(value: u64) -> Result<Self, PrimitiveError> {
        NonZeroU64::new(value)
            .filter(|value| value.get() <= i64::MAX as u64)
            .map(Self)
            .ok_or(PrimitiveError::InvalidOrganizationId)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Immutable Sentry-compatible organization or project route slug.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slug(Box<str>);

impl Slug {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, PrimitiveError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= 63
            && bytes[0].is_ascii_alphanumeric()
            && bytes[bytes.len() - 1].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && !bytes.windows(2).any(|pair| pair == b"--");
        if !valid {
            return Err(PrimitiveError::InvalidSlug);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Slug {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Slug").field(&self.0).finish()
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for Slug {
    type Err = PrimitiveError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

pub type DisplayName = BoundedId<128>;
pub type ProjectKeyLabel = BoundedId<64>;

/// Sentry-compatible 16-byte ingest credential.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DsnKey([u8; 16]);

impl DsnKey {
    pub fn parse(value: &str) -> Result<Self, PrimitiveError> {
        parse_hex_identifier(value)
            .map(Self)
            .ok_or(PrimitiveError::InvalidDsnKey)
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

impl fmt::Debug for DsnKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DsnKey(<redacted>)")
    }
}

impl fmt::Display for DsnKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

/// Sentry Event identifier, encoded as 32 hexadecimal characters on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventId([u8; 16]);

impl EventId {
    pub fn parse(value: &str) -> Result<Self, PrimitiveError> {
        parse_hex_identifier(value)
            .map(Self)
            .ok_or(PrimitiveError::InvalidEventId)
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

impl fmt::Debug for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretBytes([u8; 32]);

impl SecretBytes {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpScrubPolicy {
    Hmac,
    Keep,
    Remove,
    Truncate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectAcceptanceState {
    Active,
    Disabled,
    PendingDelete,
    Purging,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKeyState {
    Active,
    Disabled,
    SuspendedByDeletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectIngestLimits {
    pub max_event_bytes: NonZeroU32,
    pub max_events_per_second: Option<NonZeroU32>,
    pub burst: Option<NonZeroU32>,
}

impl Default for ProjectIngestLimits {
    fn default() -> Self {
        Self {
            max_event_bytes: NonZeroU32::new(1024 * 1024).expect("one MiB is nonzero"),
            max_events_per_second: None,
            burst: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubPolicy {
    pub revision: u64,
    pub ip_policy: IpScrubPolicy,
    pub hmac_key: SecretBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemCapabilities {
    pub error: bool,
    pub client_report: bool,
    pub log: bool,
    pub transaction: bool,
    pub span: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSnapshot {
    pub project_id: ProjectId,
    pub state: ProjectAcceptanceState,
    pub key_state: ProjectKeyState,
    pub scrub_policy: ScrubPolicy,
    pub items: ItemCapabilities,
    pub limits: ProjectIngestLimits,
    pub inbound_filters: Arc<inbound_filter::CompiledInboundFilterPolicy>,
    pub grouping_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationIdentity {
    pub id: OrganizationId,
    pub slug: Slug,
    pub display_name: DisplayName,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    pub id: ProjectId,
    pub organization_id: OrganizationId,
    pub slug: Slug,
    pub display_name: DisplayName,
    pub state: ProjectAcceptanceState,
    pub policy_revision: u64,
    pub ip_policy: IpScrubPolicy,
    pub items: ItemCapabilities,
    pub limits: ProjectIngestLimits,
    pub grouping_revision: u64,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectKeyIdentity {
    pub key: DsnKey,
    pub project_id: ProjectId,
    pub state: ProjectKeyState,
    pub label: ProjectKeyLabel,
    pub created_at: Timestamp,
}

/// Sanitized acceptance payload. The unsanitized wire body cannot inhabit this type.
#[derive(Clone, PartialEq, Eq)]
pub struct ScrubbedEventPayload(Box<[u8]>);

impl ScrubbedEventPayload {
    #[must_use]
    pub fn new(bytes: impl Into<Box<[u8]>>) -> Self {
        Self(bytes.into())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ScrubbedEventPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScrubbedEventPayload")
            .field("bytes", &self.0.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedEvent {
    pub project_id: ProjectId,
    pub event_id: EventId,
    pub received_at: Timestamp,
    pub policy_revision: u64,
    pub payload: ScrubbedEventPayload,
}

/// Canonical 20-byte MongoDB Event identity from ADR-0022.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventKey([u8; 20]);

impl EventKey {
    #[must_use]
    pub fn new(project_id: ProjectId, event_id: EventId) -> Self {
        let mut bytes = [0_u8; 20];
        bytes[..4].copy_from_slice(&project_id.get().to_be_bytes());
        bytes[4..].copy_from_slice(&event_id.as_bytes());
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 20]) -> Result<Self, PrimitiveError> {
        ProjectId::new(i32::from_be_bytes(
            bytes[..4].try_into().expect("four-byte Event key prefix"),
        ))?;
        Ok(Self(bytes))
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 20] {
        self.0
    }

    #[must_use]
    pub fn project_id(self) -> ProjectId {
        ProjectId::new(i32::from_be_bytes(
            self.0[..4].try_into().expect("four-byte Event key prefix"),
        ))
        .expect("validated Event key project ID")
    }

    #[must_use]
    pub fn event_id(self) -> EventId {
        EventId::from_bytes(
            self.0[4..]
                .try_into()
                .expect("sixteen-byte Event key suffix"),
        )
    }
}

impl fmt::Debug for EventKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "EventKey({}:{})",
            self.project_id().get(),
            self.event_id()
        )
    }
}

fn parse_hex_identifier(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(value, &mut bytes).ok()?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_values_enforce_owner_limits() {
        assert!(BoundedId::<4>::new("rust").is_ok());
        assert!(BoundedId::<3>::new("rust").is_err());
        assert_eq!(ByteSize::<2048>::from_str("2KiB").unwrap().get(), 2048);
        assert!(ByteSize::<2047>::from_str("2KiB").is_err());
        assert_eq!(
            BoundedDuration::<1_000>::from_str("1s").unwrap().get(),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn fuzz_regression_primitives_rejects_control_and_overflow() {
        assert!(BoundedId::<16>::new("bad\nvalue").is_err());
        assert!(ByteSize::<{ u64::MAX }>::from_str("18446744073709551615GiB").is_err());
        assert!(ErrorCode::new("Not Stable").is_err());
    }

    #[test]
    fn timestamps_have_an_explicit_range() {
        assert!(Timestamp::from_unix_millis(MIN_TIMESTAMP_MILLIS).is_ok());
        assert!(Timestamp::from_unix_millis(MAX_TIMESTAMP_MILLIS).is_ok());
        assert!(Timestamp::from_unix_millis(i64::MIN).is_err());
    }

    #[test]
    fn sentry_identifiers_have_canonical_fixed_width() {
        let event = EventId::parse("0123456789abcdef0123456789ABCDEF").unwrap();
        assert_eq!(event.to_string(), "0123456789abcdef0123456789abcdef");
        assert!(DsnKey::parse("short").is_err());
        assert!(ProjectId::new(0).is_err());
        let key = EventKey::new(ProjectId::new(0x0102_0304).unwrap(), event);
        assert_eq!(&key.as_bytes()[..4], &[1, 2, 3, 4]);
        assert_eq!(key.project_id().get(), 0x0102_0304);
        assert_eq!(key.event_id(), event);
        assert!(EventKey::from_bytes([0; 20]).is_err());
    }

    #[test]
    fn identity_values_enforce_storage_and_route_bounds() {
        assert!(OrganizationId::new(1).is_ok());
        assert!(OrganizationId::new(i64::MAX as u64).is_ok());
        assert!(OrganizationId::new(0).is_err());
        assert!(OrganizationId::new(i64::MAX as u64 + 1).is_err());
        assert_eq!(
            Slug::new("error-service").unwrap().as_str(),
            "error-service"
        );
        for invalid in [
            "",
            "UPPER",
            "-leading",
            "trailing-",
            "two--hyphens",
            "under_score",
        ] {
            assert!(Slug::new(invalid).is_err(), "{invalid}");
        }
        assert!(Slug::new("a".repeat(63)).is_ok());
        assert!(Slug::new("a".repeat(64)).is_err());
    }
}
