//! Capability-specific ports used by the Phase 1 Ingest application service.

use std::{future::Future, pin::Pin};

use faultkeep_domain::{
    AcceptedEvent, DsnKey, EventKey, OrganizationIdentity, ProjectAcceptanceState, ProjectId,
    ProjectIdentity, ProjectKeyIdentity, ProjectKeyState, ProjectSnapshot, Timestamp,
    api::{
        ActivityPage, ApiTokenView, EnvironmentPage, EventPage, EventView, IssueListQuery,
        IssuePage, IssueStatBucket, ProjectKeyView, ProjectPolicyUpdate, ProjectView, ReleasePage,
        SearchStorageQuery,
    },
    auth::{
        ApiToken, AuditRecord, BootstrapIdentity, CredentialId, EmailAddress, MembershipMutation,
        OrganizationMembership, PasswordHash, SecretDigest, SetupToken, UserAccount, UserId,
        WebSession,
    },
    finalization::{FinalizationPolicy, FinalizeBatch, FinalizeResult},
    grouping::IssueId,
    issue::{
        IssueCommand, IssueCommandResult, IssueMutationResult, IssueOccurrence, IssueSearchQuery,
        IssueSearchResult, IssueSnapshot,
    },
    processing::{PendingEvent, ProcessingFailure, ProcessingProject, ProcessingStateChange},
    symbolication::{BackendSymbolicationResult, SymbolicationRequest},
};
use thiserror::Error;

pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProjectResolveError {
    #[error("project credential is unauthorized")]
    Unauthorized,
    #[error("project resolution is temporarily unavailable")]
    Unavailable,
}

pub trait ProjectResolver: Send + Sync + 'static {
    fn resolve(&self, key: DsnKey) -> PortFuture<'_, Result<ProjectSnapshot, ProjectResolveError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProjectStoreError {
    #[error("generated identity collides with an existing record")]
    IdentityCollision,
    #[error("generated DSN key collides with an existing record")]
    KeyCollision,
    #[error("organization slug already exists")]
    OrganizationSlugExists,
    #[error("project slug already exists in the organization")]
    ProjectSlugExists,
    #[error("project identity target does not exist")]
    NotFound,
    #[error("project policy revision does not match")]
    RevisionConflict,
    #[error("project has more keys than the bounded command supports")]
    TooManyKeys,
    #[error("stored project identity data is invalid")]
    InvalidData,
    #[error("project identity storage is temporarily unavailable")]
    Unavailable,
}

/// Capability-specific control storage used by ProjectService and cache misses.
pub trait ProjectStore: Send + Sync + 'static {
    fn insert_organization(
        &self,
        organization: OrganizationIdentity,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>>;

    fn insert_project(
        &self,
        project: ProjectIdentity,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>>;

    fn insert_project_key(
        &self,
        key: ProjectKeyIdentity,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>>;

    fn load_project(
        &self,
        key: DsnKey,
    ) -> PortFuture<'_, Result<ProjectSnapshot, ProjectStoreError>>;

    fn set_key_state(
        &self,
        key: DsnKey,
        state: ProjectKeyState,
    ) -> PortFuture<'_, Result<ProjectId, ProjectStoreError>>;

    fn set_project_key_state(
        &self,
        _project_id: ProjectId,
        _key: DsnKey,
        _state: ProjectKeyState,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(async { Err(ProjectStoreError::Unavailable) })
    }

    fn set_project_acceptance(
        &self,
        project_id: ProjectId,
        state: ProjectAcceptanceState,
    ) -> PortFuture<'_, Result<Vec<DsnKey>, ProjectStoreError>>;

    fn list_projects(
        &self,
        _organization_id: faultkeep_domain::OrganizationId,
        _limit: usize,
    ) -> PortFuture<'_, Result<Vec<ProjectView>, ProjectStoreError>> {
        Box::pin(async { Err(ProjectStoreError::Unavailable) })
    }

    fn load_project_by_id(
        &self,
        _project_id: ProjectId,
    ) -> PortFuture<'_, Result<ProjectView, ProjectStoreError>> {
        Box::pin(async { Err(ProjectStoreError::Unavailable) })
    }

    fn list_project_keys(
        &self,
        _project_id: ProjectId,
    ) -> PortFuture<'_, Result<Vec<ProjectKeyView>, ProjectStoreError>> {
        Box::pin(async { Err(ProjectStoreError::Unavailable) })
    }

    fn update_project_policy(
        &self,
        _project_id: ProjectId,
        _update: ProjectPolicyUpdate,
    ) -> PortFuture<'_, Result<(ProjectView, Vec<DsnKey>), ProjectStoreError>> {
        Box::pin(async { Err(ProjectStoreError::Unavailable) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableOutcome {
    Accepted,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EventSinkError {
    #[error("durable storage is temporarily unavailable")]
    Unavailable,
    #[error("durable acknowledgement is ambiguous")]
    Ambiguous,
}

pub trait EventSink: Send + Sync + 'static {
    fn persist(
        &self,
        event: AcceptedEvent,
    ) -> PortFuture<'_, Result<DurableOutcome, EventSinkError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EventPrepareError {
    #[error("accepted Event cannot be encoded in the persistent format")]
    InvalidEvent,
    #[error("accepted Event exceeds the configured encoded size bound")]
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventWriteStatus {
    Inserted,
    Duplicate,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EventStoreError {
    #[error("Event storage is temporarily unavailable")]
    Unavailable,
    #[error("Event write acknowledgement is ambiguous")]
    Ambiguous,
}

/// Adapter-owned encoded Event that remains opaque outside the storage boundary.
pub trait PreparedEvent: Send + 'static {
    fn key(&self) -> EventKey;
    fn encoded_len(&self) -> usize;
    fn into_event(self) -> AcceptedEvent;
}

/// Capability-specific durable Event insertion port used only by MongoWriter.
pub trait EventStore: Send + Sync + 'static {
    type Prepared: PreparedEvent;

    fn prepare(&self, event: AcceptedEvent) -> Result<Self::Prepared, EventPrepareError>;

    fn insert_batch<'a>(
        &'a self,
        events: &'a [Self::Prepared],
    ) -> PortFuture<'a, Result<Vec<EventWriteStatus>, EventStoreError>>;
}

/// Fresh-payload acceleration seam. MongoDB remains authoritative if an offer fails.
pub trait AcceptedEventHandoff: Send + Sync + 'static {
    fn offer(&self, event: AcceptedEvent) -> Result<(), AcceptedEvent>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EventBacklogError {
    #[error("pending Event storage is temporarily unavailable")]
    Unavailable,
    #[error("pending Event storage contains invalid data")]
    InvalidData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BacklogObservation {
    pub pending_count: u64,
    pub oldest_pending_at: Option<Timestamp>,
}

/// Capability-specific pending Event discovery used only by Dispatcher.
pub trait EventBacklog: Send + Sync + 'static {
    fn load_due<'a>(
        &'a self,
        now: Timestamp,
        limit: usize,
        excluded: &'a [EventKey],
    ) -> PortFuture<'a, Result<Vec<PendingEvent>, EventBacklogError>>;

    fn observe(&self) -> PortFuture<'_, Result<BacklogObservation, EventBacklogError>>;
}

/// Processing seam. Completion means durable Event eligibility was already changed.
pub trait WorkHandler: Send + Sync + 'static {
    fn handle(&self, event: PendingEvent) -> PortFuture<'_, ()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProcessingProjectError {
    #[error("processing project does not exist")]
    NotFound,
    #[error("processing project data is invalid")]
    InvalidData,
    #[error("processing project storage is temporarily unavailable")]
    Unavailable,
}

pub trait ProcessingProjectStore: Send + Sync + 'static {
    fn load_processing_project(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<ProcessingProject, ProcessingProjectError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProcessingStateError {
    #[error("processing Event state is invalid")]
    InvalidData,
    #[error("processing Event state storage is temporarily unavailable")]
    Unavailable,
}

pub trait ProcessingStateStore: Send + Sync + 'static {
    fn record_processing_failure(
        &self,
        failure: ProcessingFailure,
    ) -> PortFuture<'_, Result<ProcessingStateChange, ProcessingStateError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SymbolicationBackendError {
    #[error("symbolication backend is temporarily unavailable")]
    Unavailable,
    #[error("symbolication backend timed out internally")]
    Timeout,
    #[error("symbolication backend returned an invalid response")]
    MalformedResponse,
}

/// Replaceable backend capability. Adapter wire types remain behind this port.
pub trait SymbolicationBackend: Send + Sync + 'static {
    fn symbolicate(
        &self,
        request: SymbolicationRequest,
    ) -> PortFuture<'_, Result<BackendSymbolicationResult, SymbolicationBackendError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IssueStoreError {
    #[error("Issue identity collides with a different complete GroupingKey")]
    IdentityCollision,
    #[error("Issue does not exist in the project")]
    NotFound,
    #[error("Issue storage contains invalid data")]
    InvalidData,
    #[error("Issue storage is temporarily unavailable")]
    Unavailable,
}

/// Project-scoped Issue mutations and bounded title projection used by IssueService.
pub trait IssueStore: Send + Sync + 'static {
    fn apply_occurrence(
        &self,
        occurrence: IssueOccurrence,
    ) -> PortFuture<'_, Result<IssueMutationResult, IssueStoreError>>;

    fn apply_command(
        &self,
        command: IssueCommand,
    ) -> PortFuture<'_, Result<IssueCommandResult, IssueStoreError>>;

    fn load(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
    ) -> PortFuture<'_, Result<IssueSnapshot, IssueStoreError>>;

    fn search_titles(
        &self,
        project_id: ProjectId,
        query: IssueSearchQuery,
    ) -> PortFuture<'_, Result<Vec<IssueSearchResult>, IssueStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FinalizationStoreError {
    #[error("FinalizeBatch contains invalid or inconsistent data")]
    InvalidData,
    #[error("FinalizeBatch identity collides with existing durable data")]
    IdentityCollision,
    #[error("FinalizeBatch storage is temporarily unavailable")]
    Unavailable,
}

/// Durable successful-processing fence owned only by Finalizer.
pub trait FinalizationStore: Send + Sync + 'static {
    fn finalize(
        &self,
        batch: FinalizeBatch,
        policy: FinalizationPolicy,
    ) -> PortFuture<'_, Result<FinalizeResult, FinalizationStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcomeKind {
    Accepted,
    Duplicate,
    Invalid,
    TooLarge,
    RateLimited,
    Unsupported,
    StorageUnavailable,
    Filtered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOutcome {
    pub kind: IngestOutcomeKind,
    pub reason: &'static str,
    pub quantity: u64,
}

pub trait OutcomeSink: Send + Sync + 'static {
    fn record(&self, outcome: IngestOutcome);
}

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Timestamp;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("cryptographic randomness is unavailable")]
pub struct RandomError;

pub trait RandomSource: Send + Sync + 'static {
    fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapTokenInstall {
    Created,
    AlreadyInstalled,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AuthStoreError {
    #[error("authentication record does not exist")]
    NotFound,
    #[error("identity or credential already exists")]
    AlreadyExists,
    #[error("generated identity or credential collides with an existing record")]
    IdentityCollision,
    #[error("bootstrap is no longer available")]
    BootstrapClosed,
    #[error("the operation would remove the final organization owner")]
    FinalOwner,
    #[error("credential is invalid, expired, consumed, or revoked")]
    InvalidCredential,
    #[error("authentication storage contains invalid data")]
    InvalidData,
    #[error("authentication storage is temporarily unavailable")]
    Unavailable,
}

/// Authoritative identity and credential persistence. No caller receives a raw
/// collection or an unscoped filter surface.
pub trait AuthStore: Send + Sync + 'static {
    fn install_bootstrap_token(
        &self,
        token: SetupToken,
    ) -> PortFuture<'_, Result<BootstrapTokenInstall, AuthStoreError>>;

    fn consume_bootstrap(
        &self,
        identity: BootstrapIdentity,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn create_invited_user(
        &self,
        user: UserAccount,
        membership: OrganizationMembership,
        setup_token: SetupToken,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn create_password_setup_token(
        &self,
        token: SetupToken,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn consume_password_setup(
        &self,
        digest: SecretDigest,
        now: Timestamp,
        password_hash: PasswordHash,
    ) -> PortFuture<'_, Result<UserId, AuthStoreError>>;

    fn load_user_by_email<'a>(
        &'a self,
        email: &'a EmailAddress,
    ) -> PortFuture<'a, Result<UserAccount, AuthStoreError>>;

    fn load_user(&self, user_id: UserId) -> PortFuture<'_, Result<UserAccount, AuthStoreError>>;

    fn update_password_hash(
        &self,
        user_id: UserId,
        password_hash: PasswordHash,
        changed_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn load_membership(
        &self,
        user_id: UserId,
        organization_id: faultkeep_domain::OrganizationId,
    ) -> PortFuture<'_, Result<OrganizationMembership, AuthStoreError>>;

    fn mutate_membership(
        &self,
        mutation: MembershipMutation,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn set_user_disabled(
        &self,
        user_id: UserId,
        disabled_at: Option<Timestamp>,
        operation_id: CredentialId,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn create_session(&self, session: WebSession) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn load_session(
        &self,
        digest: SecretDigest,
    ) -> PortFuture<'_, Result<WebSession, AuthStoreError>>;

    fn touch_session(
        &self,
        session_id: CredentialId,
        last_seen_at: Timestamp,
        idle_expires_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn revoke_session(
        &self,
        digest: SecretDigest,
        revoked_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn revoke_user_sessions(
        &self,
        user_id: UserId,
        revoked_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn create_api_token(&self, token: ApiToken) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn load_api_token(
        &self,
        digest: SecretDigest,
    ) -> PortFuture<'_, Result<ApiToken, AuthStoreError>>;

    fn touch_api_token(
        &self,
        token_id: CredentialId,
        last_used_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn revoke_api_token(
        &self,
        token_id: CredentialId,
        user_id: UserId,
        organization_id: faultkeep_domain::OrganizationId,
        revoked_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<faultkeep_domain::OrganizationId, AuthStoreError>>;

    fn append_audit(&self, record: AuditRecord) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn list_api_tokens(
        &self,
        _user_id: UserId,
        _organization_id: faultkeep_domain::OrganizationId,
        _limit: usize,
    ) -> PortFuture<'_, Result<Vec<ApiTokenView>, AuthStoreError>> {
        Box::pin(async { Err(AuthStoreError::Unavailable) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InvestigationStoreError {
    #[error("query target does not exist")]
    NotFound,
    #[error("query data is invalid")]
    InvalidData,
    #[error("query storage is temporarily unavailable")]
    Unavailable,
}

/// Project-scoped query capability for the native API and SearchService.
///
/// It deliberately exposes no raw backend filter, projection, sort, or collection.
pub trait InvestigationStore: Send + Sync + 'static {
    fn list_issues(
        &self,
        project_id: ProjectId,
        query: IssueListQuery,
    ) -> PortFuture<'_, Result<IssuePage, InvestigationStoreError>>;

    fn list_events(
        &self,
        project_id: ProjectId,
        issue_id: Option<IssueId>,
        from: Timestamp,
        until: Timestamp,
        before: Option<faultkeep_domain::api::EventAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<EventPage, InvestigationStoreError>>;

    fn load_event(
        &self,
        project_id: ProjectId,
        event_key: EventKey,
    ) -> PortFuture<'_, Result<EventView, InvestigationStoreError>>;

    fn search_candidates(
        &self,
        project_id: ProjectId,
        query: SearchStorageQuery,
    ) -> PortFuture<'_, Result<EventPage, InvestigationStoreError>>;

    fn issue_statistics(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
        from: Timestamp,
        until: Timestamp,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<IssueStatBucket>, InvestigationStoreError>>;

    fn issue_activity(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
        before: Option<faultkeep_domain::api::ActivityAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<ActivityPage, InvestigationStoreError>>;

    fn list_releases(
        &self,
        organization_id: faultkeep_domain::OrganizationId,
        project_id: ProjectId,
        before: Option<faultkeep_domain::api::ReleaseAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<ReleasePage, InvestigationStoreError>>;

    fn list_environments(
        &self,
        project_id: ProjectId,
        before: Option<faultkeep_domain::api::EnvironmentAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<EnvironmentPage, InvestigationStoreError>>;
}
