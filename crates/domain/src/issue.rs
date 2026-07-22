//! Adapter-independent Issue state and mutation values.

use std::{fmt, num::NonZeroU64};

use thiserror::Error;

use crate::{
    EventId, ProjectId, Timestamp,
    grouping::{GroupingExplanation, GroupingKey, GroupingStrategy, IssueId},
};

pub const MAX_ISSUE_TITLE_BYTES: usize = 512;
pub const MAX_ISSUE_CULPRIT_BYTES: usize = 256;
pub const MAX_ISSUE_RELEASE_BYTES: usize = 200;
pub const MAX_ISSUE_SEARCH_BYTES: usize = 512;
pub const MAX_ISSUE_SEARCH_RESULTS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IssueValueError {
    #[error("Issue text value must not be empty")]
    Empty,
    #[error("Issue text value exceeds its encoded byte bound")]
    TooLong,
    #[error("Issue search limit must be between one and one hundred")]
    InvalidSearchLimit,
}

macro_rules! bounded_issue_text {
    ($name:ident, $maximum:expr) => {
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Box<str>);

        impl $name {
            pub fn new(value: impl Into<Box<str>>) -> Result<Self, IssueValueError> {
                let value = value.into();
                if value.is_empty() {
                    return Err(IssueValueError::Empty);
                }
                if value.len() > $maximum {
                    return Err(IssueValueError::TooLong);
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.0)
                    .finish()
            }
        }
    };
}

bounded_issue_text!(IssueTitle, MAX_ISSUE_TITLE_BYTES);
bounded_issue_text!(IssueCulprit, MAX_ISSUE_CULPRIT_BYTES);
bounded_issue_text!(IssueRelease, MAX_ISSUE_RELEASE_BYTES);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ActorKind {
    User = 1,
    ApiCredential = 2,
    System = 3,
}

impl ActorKind {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::User),
            2 => Some(Self::ApiCredential),
            3 => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorRef {
    kind: ActorKind,
    id: [u8; 16],
}

impl ActorRef {
    #[must_use]
    pub const fn new(kind: ActorKind, id: [u8; 16]) -> Self {
        Self { kind, id }
    }

    #[must_use]
    pub const fn system() -> Self {
        Self::new(ActorKind::System, [0; 16])
    }

    #[must_use]
    pub const fn kind(self) -> ActorKind {
        self.kind
    }

    #[must_use]
    pub const fn id(self) -> [u8; 16] {
        self.id
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; 17] {
        let mut bytes = [0_u8; 17];
        bytes[0] = self.kind as u8;
        bytes[1..].copy_from_slice(&self.id);
        bytes
    }

    pub fn from_bytes(bytes: [u8; 17]) -> Option<Self> {
        Some(Self::new(
            ActorKind::from_code(bytes[0])?,
            bytes[1..].try_into().ok()?,
        ))
    }
}

impl fmt::Debug for ActorRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActorRef")
            .field("kind", &self.kind)
            .field("id", &hex::encode(self.id))
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueStatus {
    Open,
    Resolved,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueWorkflow {
    pub at: Timestamp,
    pub actor: ActorRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegressionSummary {
    pub at: Timestamp,
    pub event_id: EventId,
    pub count: NonZeroU64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueGroupingDetail {
    pub strategy: GroupingStrategy,
    pub explanation: GroupingExplanation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueOccurrence {
    pub project_id: ProjectId,
    pub issue_id: IssueId,
    pub grouping_key: GroupingKey,
    pub event_id: EventId,
    pub occurred_at: Timestamp,
    pub received_at: Timestamp,
    pub release: Option<IssueRelease>,
    pub title: IssueTitle,
    pub culprit: Option<IssueCulprit>,
    pub grouping: IssueGroupingDetail,
    pub increment: NonZeroU64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSnapshot {
    pub project_id: ProjectId,
    pub issue_id: IssueId,
    pub grouping_key: GroupingKey,
    pub title: IssueTitle,
    pub culprit: Option<IssueCulprit>,
    pub first_seen: Timestamp,
    pub last_seen: Timestamp,
    pub first_event_id: EventId,
    pub latest_event_id: EventId,
    pub representative_event_id: EventId,
    pub occurrence_count: NonZeroU64,
    pub status: IssueStatus,
    pub assignee: Option<ActorRef>,
    pub workflow: Option<IssueWorkflow>,
    pub regression: Option<RegressionSummary>,
    pub first_release: Option<IssueRelease>,
    pub last_release: Option<IssueRelease>,
    pub grouping: IssueGroupingDetail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueMutationKind {
    Created,
    Updated,
    Regressed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueMutationResult {
    pub kind: IssueMutationKind,
    pub issue: IssueSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueCommandAction {
    Resolve,
    Ignore,
    Reopen,
    Assign(Option<ActorRef>),
}

impl IssueCommandAction {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Resolve => 1,
            Self::Ignore => 2,
            Self::Reopen => 3,
            Self::Assign(Some(_)) => 4,
            Self::Assign(None) => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueCommand {
    pub project_id: ProjectId,
    pub issue_id: IssueId,
    pub idempotency_key: [u8; 16],
    pub actor: ActorRef,
    pub at: Timestamp,
    pub action: IssueCommandAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCommandResult {
    pub applied: bool,
    pub issue: IssueSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSearchQuery {
    text: Box<str>,
    limit: usize,
}

impl IssueSearchQuery {
    pub fn new(text: impl Into<Box<str>>, limit: usize) -> Result<Self, IssueValueError> {
        let text = text.into();
        if text.is_empty() {
            return Err(IssueValueError::Empty);
        }
        if text.len() > MAX_ISSUE_SEARCH_BYTES {
            return Err(IssueValueError::TooLong);
        }
        if !(1..=MAX_ISSUE_SEARCH_RESULTS).contains(&limit) {
            return Err(IssueValueError::InvalidSearchLimit);
        }
        Ok(Self { text, limit })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSearchResult {
    pub issue_id: IssueId,
    pub title: IssueTitle,
    pub status: IssueStatus,
    pub last_seen: Timestamp,
    pub occurrence_count: NonZeroU64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct IssueActivityId([u8; 16]);

impl IssueActivityId {
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for IssueActivityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "IssueActivityId({})", hex::encode(self.0))
    }
}

#[must_use]
pub fn command_activity_id(command: IssueCommand) -> IssueActivityId {
    activity_id(
        command.project_id,
        command.issue_id,
        command.action.code(),
        &command.idempotency_key,
    )
}

#[must_use]
pub fn regression_activity_id(
    project_id: ProjectId,
    issue_id: IssueId,
    event_id: EventId,
) -> IssueActivityId {
    activity_id(project_id, issue_id, 6, &event_id.as_bytes())
}

fn activity_id(
    project_id: ProjectId,
    issue_id: IssueId,
    kind: u8,
    idempotency_material: &[u8; 16],
) -> IssueActivityId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"issue-activity/v1");
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(&issue_id.as_bytes());
    hasher.update(&[kind]);
    hasher.update(idempotency_material);
    let mut id = [0_u8; 16];
    id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    IssueActivityId(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_actor_and_bounded_search_are_canonical() {
        let actor = ActorRef::new(ActorKind::User, [7; 16]);
        assert_eq!(ActorRef::from_bytes(actor.to_bytes()), Some(actor));
        assert!(ActorRef::from_bytes([0; 17]).is_none());
        assert!(IssueSearchQuery::new("panic", 100).is_ok());
        assert!(IssueSearchQuery::new("panic", 101).is_err());
        assert!(IssueTitle::new("x".repeat(MAX_ISSUE_TITLE_BYTES + 1)).is_err());
    }

    #[test]
    fn activity_ids_are_deterministic_and_action_scoped() {
        let project_id = ProjectId::new(7).unwrap();
        let issue_id = IssueId::from_bytes([3; 16]);
        let base = IssueCommand {
            project_id,
            issue_id,
            idempotency_key: [9; 16],
            actor: ActorRef::system(),
            at: Timestamp::from_unix_millis(1).unwrap(),
            action: IssueCommandAction::Resolve,
        };
        assert_eq!(command_activity_id(base), command_activity_id(base));
        assert_ne!(
            command_activity_id(base),
            command_activity_id(IssueCommand {
                action: IssueCommandAction::Ignore,
                ..base
            })
        );
    }
}
