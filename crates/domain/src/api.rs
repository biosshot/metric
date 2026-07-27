//! Descriptive, adapter-independent projections for the native API.

use std::num::NonZeroU64;

use crate::{
    DisplayName, DsnKey, EventKey, IpScrubPolicy, ItemCapabilities, OrganizationId,
    ProjectAcceptanceState, ProjectId, ProjectIngestLimits, ProjectKeyLabel, ProjectKeyState, Slug,
    Timestamp,
    auth::CredentialId,
    auth::{OrganizationRole, UserId},
    event::{EventLevel, EventPlatform},
    finalization::{EnvironmentId, ProcessedEventPayload, ReleaseId, SearchToken},
    grouping::IssueId,
    inbound_filter::InboundFilterPolicy,
    issue::{ActorRef, IssueActivityId, IssueSnapshot, IssueStatus},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectView {
    pub id: ProjectId,
    pub organization_id: OrganizationId,
    pub slug: Slug,
    pub display_name: DisplayName,
    pub state: ProjectAcceptanceState,
    pub policy_revision: u64,
    pub ip_policy: IpScrubPolicy,
    pub items: ItemCapabilities,
    pub limits: ProjectIngestLimits,
    pub inbound_filters: InboundFilterPolicy,
    pub grouping_revision: u64,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectKeyView {
    pub key: DsnKey,
    pub project_id: ProjectId,
    pub state: ProjectKeyState,
    pub label: ProjectKeyLabel,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPolicyUpdate {
    pub expected_revision: u64,
    pub ip_policy: IpScrubPolicy,
    pub items: ItemCapabilities,
    pub limits: ProjectIngestLimits,
    pub inbound_filters: InboundFilterPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueAnchor {
    pub last_seen: Timestamp,
    pub issue_id: IssueId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuePage {
    pub items: Vec<IssueSnapshot>,
    pub next: Option<IssueAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventAnchor {
    pub occurred_at: Timestamp,
    pub event_key: EventKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventView {
    pub key: EventKey,
    pub issue_id: IssueId,
    pub received_at: Timestamp,
    pub occurred_at: Timestamp,
    pub level: EventLevel,
    pub platform: EventPlatform,
    pub payload: ProcessedEventPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    pub items: Vec<EventView>,
    pub next: Option<EventAnchor>,
    pub candidates_examined: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueActivityKind {
    Resolved,
    Ignored,
    Reopened,
    Assigned,
    Unassigned,
    Regressed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityAnchor {
    pub at: Timestamp,
    pub id: IssueActivityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueActivityView {
    pub id: IssueActivityId,
    pub issue_id: IssueId,
    pub kind: IssueActivityKind,
    pub actor: ActorRef,
    pub event_key: Option<EventKey>,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityPage {
    pub items: Vec<IssueActivityView>,
    pub next: Option<ActivityAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueStatBucket {
    pub bucket_start: Timestamp,
    pub occurrence_count: NonZeroU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseAnchor {
    pub activity_at: Timestamp,
    pub id: ReleaseId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseView {
    pub id: ReleaseId,
    pub version: Box<str>,
    pub activity_at: Timestamp,
    pub first_seen: Option<Timestamp>,
    pub last_seen: Option<Timestamp>,
    pub released_at: Option<Timestamp>,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePage {
    pub items: Vec<ReleaseView>,
    pub next: Option<ReleaseAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentAnchor {
    pub last_seen: Timestamp,
    pub id: EnvironmentId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentView {
    pub id: EnvironmentId,
    pub name: Box<str>,
    pub first_seen: Timestamp,
    pub last_seen: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentPage {
    pub items: Vec<EnvironmentView>,
    pub next: Option<EnvironmentAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiTokenView {
    pub id: CredentialId,
    pub name: Box<str>,
    pub scopes: Vec<Box<str>>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationMemberView {
    pub user_id: UserId,
    pub email: Box<str>,
    pub display_name: Box<str>,
    pub role: OrganizationRole,
    pub disabled_at: Option<Timestamp>,
    pub joined_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditLogView {
    pub request_id: Box<str>,
    pub actor: Box<str>,
    pub actor_user_id: UserId,
    pub action: Box<str>,
    pub target_kind: Box<str>,
    pub target_id: Box<str>,
    pub timestamp: Timestamp,
    pub metadata: Vec<(Box<str>, Box<str>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStorageAnchor {
    ProjectTimeline,
    Event(EventKey),
    Issue(IssueId),
    Token(SearchToken),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchStorageBranch {
    pub anchor: SearchStorageAnchor,
    pub from: Timestamp,
    pub until: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchStorageQuery {
    pub branches: Vec<SearchStorageBranch>,
    pub before: Option<EventAnchor>,
    pub candidate_limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueListQuery {
    pub status: Option<IssueStatus>,
    pub before: Option<IssueAnchor>,
    pub limit: usize,
}
