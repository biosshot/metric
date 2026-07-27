//! Bounded reliability-monitor identities, schedules and Cron run lifecycle.

use std::fmt;

use thiserror::Error;
use time::{OffsetDateTime, Weekday};

use crate::{
    ProjectId, Timestamp,
    finalization::{EnvironmentId, ReleaseId},
};

pub const MAX_MONITOR_SLUG_BYTES: usize = 64;
pub const MAX_MONITOR_NAME_BYTES: usize = 128;
pub const MAX_MONITOR_ENVIRONMENT_BYTES: usize = 64;
pub const MAX_MONITOR_SCHEDULE_BYTES: usize = 128;
pub const MAX_MONITOR_INTERVAL_MINUTES: u32 = 366 * 24 * 60;
pub const MAX_MONITOR_MARGIN_SECONDS: u32 = 24 * 60 * 60;
pub const MAX_MONITOR_RUNTIME_SECONDS: u32 = 7 * 24 * 60 * 60;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorId([u8; 16]);

impl MonitorId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }

    #[must_use]
    pub fn derive(project_id: ProjectId, slug: &str, environment: &str) -> Self {
        Self(hash16(
            b"metric/cron-monitor/v1",
            &[
                &project_id.get().to_be_bytes(),
                slug.as_bytes(),
                environment.as_bytes(),
            ],
        ))
    }
}

impl fmt::Debug for MonitorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl fmt::Display for MonitorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorRunId([u8; 16]);

impl MonitorRunId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }

    #[must_use]
    pub fn sdk(monitor_id: MonitorId, check_in_id: [u8; 16]) -> Self {
        Self(hash16(
            b"metric/cron-run/sdk/v1",
            &[&monitor_id.as_bytes(), &check_in_id],
        ))
    }

    #[must_use]
    pub fn missed(monitor_id: MonitorId, scheduled_for: Timestamp) -> Self {
        Self(hash16(
            b"metric/cron-run/missed/v1",
            &[
                &monitor_id.as_bytes(),
                &scheduled_for.unix_millis().to_be_bytes(),
            ],
        ))
    }
}

impl fmt::Debug for MonitorRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl fmt::Display for MonitorRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorRunStatus {
    InProgress,
    Success,
    Error,
    Timeout,
    Missed,
}

impl MonitorRunStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Success => "success",
            Self::Error => "error",
            Self::Timeout => "timeout",
            Self::Missed => "missed",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::InProgress)
    }

    pub fn parse(value: &str) -> Result<Self, MonitorValueError> {
        match value {
            "in_progress" => Ok(Self::InProgress),
            "ok" | "success" => Ok(Self::Success),
            "error" => Ok(Self::Error),
            "timeout" => Ok(Self::Timeout),
            "missed" => Ok(Self::Missed),
            _ => Err(MonitorValueError::InvalidStatus),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorRunSource {
    Sdk,
    Scheduler,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorSchedule {
    Interval {
        minutes: u32,
    },
    Crontab {
        expression: Box<str>,
        compiled: Box<CompiledCrontab>,
    },
}

impl MonitorSchedule {
    pub fn interval(minutes: u32) -> Result<Self, MonitorValueError> {
        if !(1..=MAX_MONITOR_INTERVAL_MINUTES).contains(&minutes) {
            return Err(MonitorValueError::InvalidSchedule);
        }
        Ok(Self::Interval { minutes })
    }

    pub fn crontab(expression: &str) -> Result<Self, MonitorValueError> {
        if expression.is_empty() || expression.len() > MAX_MONITOR_SCHEDULE_BYTES {
            return Err(MonitorValueError::InvalidSchedule);
        }
        let parts = expression.split_ascii_whitespace().collect::<Vec<_>>();
        if parts.len() != 5 {
            return Err(MonitorValueError::InvalidSchedule);
        }
        let normalized = parts.join(" ");
        let compiled = CompiledCrontab {
            minute: parse_field(parts[0], 0, 59)?,
            hour: parse_field(parts[1], 0, 23)?,
            day: parse_field(parts[2], 1, 31)?,
            month: parse_field(parts[3], 1, 12)?,
            weekday: parse_field(parts[4], 0, 6)?,
            day_wildcard: parts[2] == "*",
            weekday_wildcard: parts[4] == "*",
        };
        Ok(Self::Crontab {
            expression: normalized.into_boxed_str(),
            compiled: Box::new(compiled),
        })
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Interval { .. } => "interval",
            Self::Crontab { .. } => "crontab",
        }
    }

    #[must_use]
    pub fn value(&self) -> Box<str> {
        match self {
            Self::Interval { minutes } => minutes.to_string().into_boxed_str(),
            Self::Crontab { expression, .. } => expression.clone(),
        }
    }

    pub fn next_after(&self, after: Timestamp) -> Result<Timestamp, MonitorValueError> {
        match self {
            Self::Interval { minutes } => add_millis(
                after,
                i64::from(*minutes)
                    .checked_mul(60_000)
                    .ok_or(MonitorValueError::InvalidTime)?,
            ),
            Self::Crontab { compiled, .. } => compiled.next_after(after),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorConfig {
    pub schedule: MonitorSchedule,
    pub checkin_margin_seconds: u32,
    pub max_runtime_seconds: u32,
}

impl MonitorConfig {
    pub fn validate(&self) -> Result<(), MonitorValueError> {
        if self.checkin_margin_seconds > MAX_MONITOR_MARGIN_SECONDS
            || self.max_runtime_seconds == 0
            || self.max_runtime_seconds > MAX_MONITOR_RUNTIME_SECONDS
        {
            return Err(MonitorValueError::InvalidConfig);
        }
        Ok(())
    }

    pub fn missed_at(&self, scheduled_for: Timestamp) -> Result<Timestamp, MonitorValueError> {
        add_millis(
            scheduled_for,
            i64::from(self.checkin_margin_seconds) * 1_000,
        )
    }

    pub fn timeout_at(&self, started_at: Timestamp) -> Result<Timestamp, MonitorValueError> {
        add_millis(started_at, i64::from(self.max_runtime_seconds) * 1_000)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorDefinition {
    pub id: MonitorId,
    pub project_id: ProjectId,
    pub slug: Box<str>,
    pub name: Box<str>,
    pub environment_id: EnvironmentId,
    pub environment: Box<str>,
    pub enabled: bool,
    pub managed_by_web: bool,
    pub revision: u64,
    pub config: MonitorConfig,
    pub next_expected_at: Timestamp,
    pub last_run_id: Option<MonitorRunId>,
    pub last_status: Option<MonitorRunStatus>,
    pub last_check_in_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl MonitorDefinition {
    pub fn validate(&self) -> Result<(), MonitorValueError> {
        validate_text(&self.slug, MAX_MONITOR_SLUG_BYTES)?;
        validate_text(&self.name, MAX_MONITOR_NAME_BYTES)?;
        validate_text(&self.environment, MAX_MONITOR_ENVIRONMENT_BYTES)?;
        self.config.validate()?;
        if self.revision == 0 || self.updated_at < self.created_at {
            return Err(MonitorValueError::InvalidConfig);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorRun {
    pub id: MonitorRunId,
    pub project_id: ProjectId,
    pub monitor_id: MonitorId,
    pub check_in_id: Option<[u8; 16]>,
    pub status: MonitorRunStatus,
    pub source: MonitorRunSource,
    pub scheduled_for: Option<Timestamp>,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub duration_ms: Option<u64>,
    pub received_at: Timestamp,
    pub release_id: Option<ReleaseId>,
    pub timeout_at: Option<Timestamp>,
    pub delete_at: Option<Timestamp>,
}

impl MonitorRun {
    pub fn validate(&self) -> Result<(), MonitorValueError> {
        if self.finished_at.is_some() != self.status.is_terminal()
            || self
                .finished_at
                .is_some_and(|value| value < self.started_at)
            || self
                .duration_ms
                .is_some_and(|value| value > i64::MAX as u64)
            || matches!(self.source, MonitorRunSource::Sdk) != self.check_in_id.is_some()
        {
            return Err(MonitorValueError::InvalidRun);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorUpdate {
    pub definition: Option<MonitorDefinition>,
    pub run: MonitorRun,
}

impl MonitorUpdate {
    pub fn validate(&self) -> Result<(), MonitorValueError> {
        self.run.validate()?;
        if let Some(definition) = &self.definition {
            definition.validate()?;
            if definition.id != self.run.monitor_id || definition.project_id != self.run.project_id
            {
                return Err(MonitorValueError::InvalidRun);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorAnchor {
    pub updated_at: Timestamp,
    pub monitor_id: MonitorId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorPage {
    pub items: Vec<MonitorDefinition>,
    pub next: Option<MonitorAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRunAnchor {
    pub started_at: Timestamp,
    pub run_id: MonitorRunId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorRunPage {
    pub items: Vec<MonitorRun>,
    pub next: Option<MonitorRunAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MonitorValueError {
    #[error("monitor text value is invalid")]
    InvalidText,
    #[error("monitor schedule is invalid or unsupported")]
    InvalidSchedule,
    #[error("monitor configuration is invalid")]
    InvalidConfig,
    #[error("monitor status is invalid")]
    InvalidStatus,
    #[error("monitor run is invalid")]
    InvalidRun,
    #[error("monitor timestamp is invalid")]
    InvalidTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCrontab {
    minute: u64,
    hour: u64,
    day: u64,
    month: u64,
    weekday: u64,
    day_wildcard: bool,
    weekday_wildcard: bool,
}

impl CompiledCrontab {
    fn next_after(&self, after: Timestamp) -> Result<Timestamp, MonitorValueError> {
        let seconds = after.unix_millis().div_euclid(1_000);
        let next_minute = seconds
            .div_euclid(60)
            .checked_add(1)
            .and_then(|value| value.checked_mul(60))
            .ok_or(MonitorValueError::InvalidTime)?;
        // Two years admits every supported numeric schedule except an isolated
        // leap-day expression, which is deliberately outside this bounded subset.
        for offset in 0_i64..=(2 * 366 * 24 * 60) {
            let candidate_seconds = next_minute
                .checked_add(
                    offset
                        .checked_mul(60)
                        .ok_or(MonitorValueError::InvalidTime)?,
                )
                .ok_or(MonitorValueError::InvalidTime)?;
            let value = OffsetDateTime::from_unix_timestamp(candidate_seconds)
                .map_err(|_| MonitorValueError::InvalidTime)?;
            let weekday = match value.weekday() {
                Weekday::Sunday => 0,
                Weekday::Monday => 1,
                Weekday::Tuesday => 2,
                Weekday::Wednesday => 3,
                Weekday::Thursday => 4,
                Weekday::Friday => 5,
                Weekday::Saturday => 6,
            };
            let day_matches = contains(self.day, value.day()) && contains(self.weekday, weekday);
            let cron_day_matches = if self.day_wildcard || self.weekday_wildcard {
                day_matches
            } else {
                contains(self.day, value.day()) || contains(self.weekday, weekday)
            };
            if contains(self.minute, value.minute())
                && contains(self.hour, value.hour())
                && contains(self.month, u8::from(value.month()))
                && cron_day_matches
            {
                return Timestamp::from_unix_millis(
                    candidate_seconds
                        .checked_mul(1_000)
                        .ok_or(MonitorValueError::InvalidTime)?,
                )
                .map_err(|_| MonitorValueError::InvalidTime);
            }
        }
        Err(MonitorValueError::InvalidSchedule)
    }
}

fn parse_field(value: &str, minimum: u8, maximum: u8) -> Result<u64, MonitorValueError> {
    let mut bits = 0_u64;
    for item in value.split(',') {
        if item.is_empty() {
            return Err(MonitorValueError::InvalidSchedule);
        }
        let (base, step) = item.split_once('/').map_or((item, 1_u8), |(base, step)| {
            let parsed = step.parse::<u8>().unwrap_or(0);
            (base, parsed)
        });
        if step == 0 {
            return Err(MonitorValueError::InvalidSchedule);
        }
        let (start, end) = if base == "*" {
            (minimum, maximum)
        } else if let Some((start, end)) = base.split_once('-') {
            (
                start
                    .parse::<u8>()
                    .map_err(|_| MonitorValueError::InvalidSchedule)?,
                end.parse::<u8>()
                    .map_err(|_| MonitorValueError::InvalidSchedule)?,
            )
        } else {
            let exact = base
                .parse::<u8>()
                .map_err(|_| MonitorValueError::InvalidSchedule)?;
            (exact, exact)
        };
        if start < minimum || end > maximum || start > end {
            return Err(MonitorValueError::InvalidSchedule);
        }
        for number in (start..=end).step_by(usize::from(step)) {
            bits |= 1_u64 << number;
        }
    }
    (bits != 0)
        .then_some(bits)
        .ok_or(MonitorValueError::InvalidSchedule)
}

fn contains(bits: u64, value: u8) -> bool {
    bits & (1_u64 << value) != 0
}

fn validate_text(value: &str, maximum: usize) -> Result<(), MonitorValueError> {
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(|character| character.is_control())
    {
        return Err(MonitorValueError::InvalidText);
    }
    Ok(())
}

fn add_millis(value: Timestamp, delta: i64) -> Result<Timestamp, MonitorValueError> {
    Timestamp::from_unix_millis(
        value
            .unix_millis()
            .checked_add(delta)
            .ok_or(MonitorValueError::InvalidTime)?,
    )
    .map_err(|_| MonitorValueError::InvalidTime)
}

fn hash16(domain: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    for part in parts {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    let mut output = [0_u8; 16];
    output.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_scoped_and_stable() {
        let project = ProjectId::new(7).unwrap();
        let monitor = MonitorId::derive(project, "nightly", "production");
        assert_eq!(monitor, MonitorId::derive(project, "nightly", "production"));
        assert_ne!(monitor, MonitorId::derive(project, "nightly", "staging"));
        assert_eq!(
            MonitorRunId::missed(monitor, Timestamp::from_unix_millis(60_000).unwrap()),
            MonitorRunId::missed(monitor, Timestamp::from_unix_millis(60_000).unwrap())
        );
    }

    #[test]
    fn interval_and_crontab_advance_strictly() {
        let now = Timestamp::from_unix_millis(1_700_000_000_000).unwrap();
        assert_eq!(
            MonitorSchedule::interval(5)
                .unwrap()
                .next_after(now)
                .unwrap()
                .unix_millis(),
            now.unix_millis() + 300_000
        );
        let hourly = MonitorSchedule::crontab("0 * * * *").unwrap();
        let next = hourly.next_after(now).unwrap();
        assert!(next > now);
        let parsed = OffsetDateTime::from_unix_timestamp(next.unix_millis() / 1_000).unwrap();
        assert_eq!(parsed.minute(), 0);
    }

    #[test]
    fn unsupported_or_too_frequent_syntax_fails() {
        assert!(MonitorSchedule::interval(0).is_err());
        assert!(MonitorSchedule::crontab("@hourly").is_err());
        assert!(MonitorSchedule::crontab("* * * * * *").is_err());
        assert!(MonitorSchedule::crontab("* * * JAN *").is_err());
    }

    #[test]
    fn grace_and_runtime_deadlines_are_server_time_exact() {
        let scheduled = Timestamp::from_unix_millis(1_700_000_000_000).unwrap();
        let config = MonitorConfig {
            schedule: MonitorSchedule::interval(5).unwrap(),
            checkin_margin_seconds: 90,
            max_runtime_seconds: 600,
        };
        assert_eq!(
            config.missed_at(scheduled).unwrap().unix_millis(),
            scheduled.unix_millis() + 90_000
        );
        assert_eq!(
            config.timeout_at(scheduled).unwrap().unix_millis(),
            scheduled.unix_millis() + 600_000
        );
    }
}
