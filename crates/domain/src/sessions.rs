//! Compact Sentry application Session lifecycle and Release Health values.

use std::fmt;

use crate::{
    ProjectId, Timestamp,
    finalization::{EnvironmentId, ReleaseId},
};

pub const USER_SKETCH_BYTES: usize = 128;
pub const USER_SKETCH_STANDARD_ERROR_PERCENT: f64 = 3.25;
pub const USER_SKETCH_SATURATION_ESTIMATE: u64 = 7_098;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId([u8; 16]);

impl SessionId {
    #[must_use]
    pub fn derive(project_id: ProjectId, sdk_session_id: [u8; 16]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"metric/session-id/v1");
        hasher.update(&project_id.get().to_be_bytes());
        hasher.update(&sdk_session_id);
        let mut bytes = [0; 16];
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

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Ok,
    Exited,
    Crashed,
    Abnormal,
}

impl SessionState {
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::Exited => 1,
            Self::Crashed => 2,
            Self::Abnormal => 3,
        }
    }

    pub fn from_code(code: i32) -> Result<Self, SessionValueError> {
        match code {
            0 => Ok(Self::Ok),
            1 => Ok(Self::Exited),
            2 => Ok(Self::Crashed),
            3 => Ok(Self::Abnormal),
            _ => Err(SessionValueError::InvalidState),
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Ok)
    }

    const fn precedence(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Exited => 1,
            Self::Abnormal => 2,
            Self::Crashed => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUpdate {
    pub id: SessionId,
    pub project_id: ProjectId,
    pub release_id: ReleaseId,
    pub environment_id: EnvironmentId,
    pub started_at: Timestamp,
    pub updated_at: Timestamp,
    pub state: SessionState,
    pub sequence: Option<u64>,
    pub duration_ms: Option<u64>,
    pub user_digest: Option<[u8; 16]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub project_id: ProjectId,
    pub release_id: ReleaseId,
    pub environment_id: EnvironmentId,
    pub started_at: Timestamp,
    pub last_update: Timestamp,
    pub state: SessionState,
    pub sequence: Option<u64>,
    pub finished_at: Option<Timestamp>,
    pub duration_ms: Option<u64>,
    pub user_digest: Option<[u8; 16]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionValueError {
    InvalidIdentity,
    InvalidTime,
    InvalidState,
    Conflict,
}

impl SessionUpdate {
    pub fn validate(&self) -> Result<(), SessionValueError> {
        if self.updated_at < self.started_at
            || self
                .duration_ms
                .is_some_and(|value| value > i64::MAX as u64)
        {
            return Err(SessionValueError::InvalidTime);
        }
        Ok(())
    }
}

impl SessionRecord {
    pub fn from_update(update: SessionUpdate) -> Result<Self, SessionValueError> {
        update.validate()?;
        let finished_at = update.state.is_terminal().then_some(update.updated_at);
        Ok(Self {
            id: update.id,
            project_id: update.project_id,
            release_id: update.release_id,
            environment_id: update.environment_id,
            started_at: update.started_at,
            last_update: update.updated_at,
            state: update.state,
            sequence: update.sequence,
            finished_at,
            duration_ms: finished_at.and(update.duration_ms),
            user_digest: update.user_digest,
        })
    }

    pub fn merge(&mut self, update: SessionUpdate) -> Result<bool, SessionValueError> {
        update.validate()?;
        if self.id != update.id
            || self.project_id != update.project_id
            || self.release_id != update.release_id
            || self.environment_id != update.environment_id
            || matches!((self.user_digest, update.user_digest), (Some(left), Some(right)) if left != right)
        {
            return Err(SessionValueError::Conflict);
        }

        let before = self.clone();
        let previous_last_update = self.last_update;
        self.started_at = self.started_at.min(update.started_at);
        self.last_update = self.last_update.max(update.updated_at);
        self.user_digest = self.user_digest.or(update.user_digest);

        let existing_sequence = self.sequence.unwrap_or(0);
        let incoming_sequence = update.sequence.unwrap_or(0);
        self.sequence = match (self.sequence, update.sequence) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (left, right) => left.or(right),
        };

        let stronger = update.state.precedence() > self.state.precedence();
        let same_newer = update.state == self.state
            && (incoming_sequence > existing_sequence
                || (incoming_sequence == existing_sequence
                    && update.updated_at > self.finished_at.unwrap_or(previous_last_update)));
        if stronger || same_newer {
            self.state = update.state;
            self.finished_at = update.state.is_terminal().then_some(update.updated_at);
            self.duration_ms = self.finished_at.and(update.duration_ms);
        }

        Ok(*self != before)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserSketch([u8; USER_SKETCH_BYTES]);

impl Default for UserSketch {
    fn default() -> Self {
        Self([0; USER_SKETCH_BYTES])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseHealthBucket {
    pub hour: Timestamp,
    pub environment_id: EnvironmentId,
    pub environment: Box<str>,
    pub sessions: u64,
    pub crashed: u64,
    pub abnormal: u64,
    pub exited: u64,
    pub approximate_users: u64,
    pub approximate_crashed_users: u64,
    pub user_sketch: UserSketch,
    pub crashed_user_sketch: UserSketch,
}

impl UserSketch {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; USER_SKETCH_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; USER_SKETCH_BYTES] {
        self.0
    }

    pub fn insert(&mut self, digest: [u8; 16]) {
        let index =
            usize::from(u16::from_be_bytes([digest[0], digest[1]])) % (USER_SKETCH_BYTES * 8);
        self.0[index / 8] |= 1 << (index % 8);
    }

    pub fn merge(&mut self, other: Self) {
        for (left, right) in self.0.iter_mut().zip(other.0) {
            *left |= right;
        }
    }

    #[must_use]
    pub fn estimate(self) -> u64 {
        let bits = (USER_SKETCH_BYTES * 8) as f64;
        let zeroes = self.0.iter().map(|byte| byte.count_zeros()).sum::<u32>() as f64;
        if zeroes == 0.0 {
            return USER_SKETCH_SATURATION_ESTIMATE;
        }
        (-(bits * (zeroes / bits).ln())).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(state: SessionState, sequence: u64, updated_at: i64) -> SessionUpdate {
        SessionUpdate {
            id: SessionId::derive(ProjectId::new(42).unwrap(), [7; 16]),
            project_id: ProjectId::new(42).unwrap(),
            release_id: ReleaseId::from_bytes([3; 16]),
            environment_id: EnvironmentId::from_bytes([4; 16]),
            started_at: Timestamp::from_unix_millis(1_000).unwrap(),
            updated_at: Timestamp::from_unix_millis(updated_at).unwrap(),
            state,
            sequence: Some(sequence),
            duration_ms: Some(500),
            user_digest: Some([9; 16]),
        }
    }

    #[test]
    fn duplicate_and_out_of_order_updates_converge() {
        let updates = [
            update(SessionState::Ok, 1, 1_100),
            update(SessionState::Exited, 3, 1_300),
            update(SessionState::Crashed, 2, 1_200),
            update(SessionState::Exited, 3, 1_300),
        ];
        let mut forward = SessionRecord::from_update(updates[0].clone()).unwrap();
        for item in &updates[1..] {
            forward.merge(item.clone()).unwrap();
        }
        let mut reordered = SessionRecord::from_update(updates[1].clone()).unwrap();
        for item in [updates[3].clone(), updates[0].clone(), updates[2].clone()] {
            reordered.merge(item).unwrap();
        }
        assert_eq!(forward, reordered);
        assert_eq!(forward.state, SessionState::Crashed);
    }

    #[test]
    fn user_sketch_is_fixed_and_mergeable() {
        let mut left = UserSketch::default();
        left.insert([1; 16]);
        let mut right = UserSketch::default();
        right.insert([2; 16]);
        let individual = left.estimate() + right.estimate();
        left.merge(right);
        assert_eq!(left.as_bytes().len(), USER_SKETCH_BYTES);
        assert!(left.estimate() <= individual);
    }

    #[test]
    #[ignore = "retained release-mode Phase 30 Session merge RPS baseline"]
    fn performance_session_merge_rps() {
        use std::{hint::black_box, time::Instant};
        const OPERATIONS: usize = 100_000;
        let base = update(SessionState::Ok, 1, 1_100);
        let started = Instant::now();
        for index in 0..OPERATIONS {
            let mut record = SessionRecord::from_update(base.clone()).unwrap();
            let mut next = update(SessionState::Exited, index as u64 + 2, 1_200);
            next.id = SessionId::derive(
                next.project_id,
                (index as u128).saturating_add(1).to_be_bytes(),
            );
            record.id = next.id;
            black_box(record.merge(next).unwrap());
        }
        let elapsed = started.elapsed();
        let rps = OPERATIONS as f64 / elapsed.as_secs_f64();
        eprintln!(
            "Phase 30 Session merge: rps={rps:.0},operations={OPERATIONS},elapsed_ms={}",
            elapsed.as_millis()
        );
        assert!(rps >= 100_000.0, "{rps:.0} RPS below local gate");
    }
}
