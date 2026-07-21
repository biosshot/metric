//! Transport- and adapter-independent bounded primitives.

use std::{fmt, str::FromStr, time::Duration};

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
}
