//! Capability-specific ports used by the Phase 1 Ingest application service.

use std::{future::Future, pin::Pin, time::Duration};

use metric_domain::{
    AcceptedEvent, DsnKey, EventKey, OrganizationIdentity, ProjectAcceptanceState, ProjectId,
    ProjectIdentity, ProjectKeyIdentity, ProjectKeyState, ProjectSnapshot, Timestamp,
    api::{
        ActivityPage, ApiTokenView, AuditLogView, EnvironmentPage, EventPage, EventView,
        IssueListQuery, IssuePage, IssueStatBucket, OrganizationMemberView, ProjectKeyView,
        ProjectPolicyUpdate, ProjectView, ReleasePage, SearchStorageQuery,
    },
    archive::{ArchiveBatch, ArchiveKind, ArchiveSegmentId, ArchiveSourceId},
    artifacts::{
        ArtifactBinding, ArtifactBundle, ArtifactBundleId, ArtifactCandidate, ArtifactGcClaim,
        ArtifactLookup, ArtifactUpload, ArtifactUploadRecord, ArtifactUploadState,
    },
    auth::{
        ApiToken, AuditRecord, BootstrapIdentity, CredentialId, EmailAddress, MembershipMutation,
        OrganizationMembership, PasswordHash, SecretDigest, SetupToken, UserAccount, UserId,
        WebSession,
    },
    blob::{BlobKey, BlobKind, BlobNamespace, BlobObject, BlobObjectId},
    debug_files::{
        CodeId, DebugFile, DebugFileId, DebugId, DebugUpload, DebugUploadRecord, DebugUploadState,
    },
    deletion::{
        ProjectDeletionChange, ProjectDeletionOperationId, ProjectDeletionRequest,
        ProjectDeletionStatus,
    },
    feedback::{FeedbackAnchor, FeedbackPage, FeedbackRecord, FeedbackStatus},
    finalization::{FinalizationPolicy, FinalizeBatch, FinalizeResult},
    grouping::IssueId,
    issue::{
        IssueCommand, IssueCommandResult, IssueMutationResult, IssueOccurrence, IssueSearchQuery,
        IssueSearchResult, IssueSnapshot,
    },
    notifications::{
        AlertRule, ClaimedNotificationDelivery, IssueNotificationTransition, NotificationDelivery,
        NotificationDeliveryId, NotificationDestination,
    },
    processing::{PendingEvent, ProcessingFailure, ProcessingProject, ProcessingStateChange},
    releases::{
        CreateDeploy, CreateRelease, DeployId, DeployRecord, FinalizeRelease, ReleaseIssueSummary,
        ReleaseRecord,
    },
    sessions::{ReleaseHealthBucket, SessionId, SessionRecord, SessionUpdate},
    signals::{
        LogId, LogRecord, LogSeverity, PerformanceBucket, SignalCursor, SignalPage, SpanRecord,
        TraceId, TraceView,
    },
    symbolication::{BackendSymbolicationResult, SymbolicationRequest},
};
use thiserror::Error;

pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BlobStoreError {
    #[error("blob source or object exceeds its configured limit")]
    TooLarge,
    #[error("blob storage capacity reserve is exhausted")]
    Capacity,
    #[error("blob object does not exist")]
    NotFound,
    #[error("blob object is corrupt")]
    Corrupt,
    #[error("blob storage request is invalid")]
    Invalid,
    #[error("blob storage is temporarily unavailable")]
    Unavailable,
}

pub trait BlobWriteSession: Send + 'static {
    fn write_chunk(&mut self, chunk: Box<[u8]>) -> PortFuture<'_, Result<(), BlobStoreError>>;

    fn commit(
        self: Box<Self>,
        key: BlobKey,
    ) -> PortFuture<'static, Result<BlobObject, BlobStoreError>>;

    fn abort(self: Box<Self>) -> PortFuture<'static, Result<(), BlobStoreError>>;
}

pub trait BlobReadSession: Send + 'static {
    fn read_chunk(
        &mut self,
        maximum: usize,
    ) -> PortFuture<'_, Result<Option<Box<[u8]>>, BlobStoreError>>;
}

#[derive(Debug, Clone)]
pub struct BlobScanRequest {
    pub namespace: BlobNamespace,
    pub older_than: Timestamp,
    pub cursor: Option<Box<str>>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobScanPage {
    pub objects: Vec<BlobObject>,
    pub next_cursor: Option<Box<str>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobCapacity {
    pub used_bytes: u64,
    pub writable_bytes: u64,
    pub reserve_bytes: u64,
}

pub trait BlobStore: Send + Sync + 'static {
    fn begin(
        &self,
        kind: BlobKind,
        created_at: Timestamp,
    ) -> PortFuture<'_, Result<Box<dyn BlobWriteSession>, BlobStoreError>>;

    fn open(
        &self,
        key: &BlobKey,
    ) -> PortFuture<'_, Result<Box<dyn BlobReadSession>, BlobStoreError>>;

    fn delete(&self, key: &BlobKey) -> PortFuture<'_, Result<(), BlobStoreError>>;

    fn scan(
        &self,
        request: BlobScanRequest,
    ) -> PortFuture<'_, Result<BlobScanPage, BlobStoreError>>;

    fn capacity(&self) -> BlobCapacity;
}

#[derive(Debug, Clone, Copy)]
pub struct ArchiveClaimRequest {
    pub kind: ArchiveKind,
    pub now: Timestamp,
    pub maximum_events: usize,
    pub target_uncompressed_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct ArchiveCompleteRequest {
    pub segment_id: ArchiveSegmentId,
    pub object: BlobObject,
    pub completed_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct ArchiveSourceCommitRequest {
    pub kind: ArchiveKind,
    pub segment_id: ArchiveSegmentId,
    pub source_ids: Vec<ArchiveSourceId>,
    pub expire_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArchiveStoreError {
    #[error("archive request or stored data is invalid")]
    InvalidData,
    #[error("archive manifest conflicts with committed metadata")]
    Conflict,
    #[error("archive storage is temporarily unavailable")]
    Unavailable,
}

/// Durable archive-manifest ownership. Blob bytes are published through `BlobStore`;
/// this port owns only bounded source selection and the MongoDB commit sequence.
pub trait ArchiveStore: Send + Sync + 'static {
    fn claim(
        &self,
        request: ArchiveClaimRequest,
    ) -> PortFuture<'_, Result<Option<ArchiveBatch>, ArchiveStoreError>>;

    fn complete(
        &self,
        request: ArchiveCompleteRequest,
    ) -> PortFuture<'_, Result<(), ArchiveStoreError>>;

    fn commit_sources(
        &self,
        request: ArchiveSourceCommitRequest,
    ) -> PortFuture<'_, Result<usize, ArchiveStoreError>>;

    fn object_referenced(&self, key: &BlobKey) -> PortFuture<'_, Result<bool, ArchiveStoreError>>;
}

pub trait BlobChunkSource: Send + 'static {
    fn next_chunk(
        &mut self,
        maximum: usize,
    ) -> PortFuture<'_, Result<Option<Box<[u8]>>, BlobStoreError>>;
}

#[derive(Debug, Clone, Copy)]
pub struct BlobReference {
    pub project_id: ProjectId,
    pub event_id: metric_domain::EventId,
    pub object_id: BlobObjectId,
}

pub trait BlobReferenceStore: Send + Sync + 'static {
    fn is_referenced(
        &self,
        reference: BlobReference,
    ) -> PortFuture<'_, Result<bool, BlobStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DebugFileStoreError {
    #[error("debug file target does not exist")]
    NotFound,
    #[error("debug file already exists with conflicting metadata")]
    Conflict,
    #[error("debug file quota is exhausted")]
    Quota,
    #[error("stored debug file data is invalid")]
    InvalidData,
    #[error("debug file storage is temporarily unavailable")]
    Unavailable,
}

pub trait DebugFileStore: Send + Sync + 'static {
    fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<metric_domain::OrganizationId, DebugFileStoreError>>;

    fn resolve_project_slugs(
        &self,
        organization_slug: Box<str>,
        project_slug: Box<str>,
    ) -> PortFuture<'_, Result<(metric_domain::OrganizationId, ProjectView), DebugFileStoreError>>;

    fn load_by_sha1(
        &self,
        project_id: ProjectId,
        sha1: [u8; 20],
    ) -> PortFuture<'_, Result<Option<DebugFile>, DebugFileStoreError>>;

    fn upsert_upload(
        &self,
        upload: DebugUpload,
    ) -> PortFuture<'_, Result<DebugUploadRecord, DebugFileStoreError>>;

    fn set_upload_state(
        &self,
        upload_id: [u8; 16],
        state: DebugUploadState,
        now: Timestamp,
        error_code: Option<Box<str>>,
    ) -> PortFuture<'_, Result<(), DebugFileStoreError>>;

    fn publish_debug_file(
        &self,
        upload_id: [u8; 16],
        file: DebugFile,
    ) -> PortFuture<'_, Result<u64, DebugFileStoreError>>;

    fn find_debug_files(
        &self,
        project_id: ProjectId,
        debug_id: Option<DebugId>,
        code_id: Option<CodeId>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<DebugFile>, DebugFileStoreError>>;

    fn load_debug_file(
        &self,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> PortFuture<'_, Result<DebugFile, DebugFileStoreError>>;

    fn delete_debug_file(
        &self,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> PortFuture<'_, Result<Option<(DebugFile, u64)>, DebugFileStoreError>>;

    fn recoverable_uploads(
        &self,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<DebugUploadRecord>, DebugFileStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactStoreError {
    #[error("artifact target does not exist")]
    NotFound,
    #[error("artifact identity conflicts with existing content")]
    Conflict,
    #[error("artifact quota is exhausted")]
    Quota,
    #[error("artifact is busy with another state transition")]
    Busy,
    #[error("stored artifact data is invalid")]
    InvalidData,
    #[error("artifact storage is temporarily unavailable")]
    Unavailable,
}

/// Organization-deduplicated Artifact Bundle metadata and GC state boundary.
pub trait ArtifactStore: Send + Sync + 'static {
    fn resolve_projects(
        &self,
        organization_slug: Box<str>,
        project_slugs: Vec<Box<str>>,
    ) -> PortFuture<'_, Result<(metric_domain::OrganizationId, Vec<ProjectView>), ArtifactStoreError>>;

    fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<metric_domain::OrganizationId, ArtifactStoreError>>;

    fn load_by_sha1(
        &self,
        organization_id: metric_domain::OrganizationId,
        sha1: [u8; 20],
    ) -> PortFuture<'_, Result<Option<ArtifactBundle>, ArtifactStoreError>>;

    fn upsert_upload(
        &self,
        upload: ArtifactUpload,
    ) -> PortFuture<'_, Result<ArtifactUploadRecord, ArtifactStoreError>>;

    fn set_upload_state(
        &self,
        upload_id: [u8; 16],
        state: ArtifactUploadState,
        now: Timestamp,
        final_id: Option<ArtifactBundleId>,
        error_code: Option<u16>,
    ) -> PortFuture<'_, Result<(), ArtifactStoreError>>;

    /// Returns generation zero for new content or reserves one post-tombstone generation.
    fn publication_generation(
        &self,
        organization_id: metric_domain::OrganizationId,
        sha1: [u8; 20],
        upload_id: [u8; 16],
        reservation_until: Timestamp,
    ) -> PortFuture<'_, Result<u32, ArtifactStoreError>>;

    /// Publishes or rescues content and returns affected project revisions.
    fn publish_bundle(
        &self,
        upload_id: [u8; 16],
        bundle: ArtifactBundle,
    ) -> PortFuture<'_, Result<Vec<(ProjectId, u64)>, ArtifactStoreError>>;

    fn lookup(
        &self,
        request: ArtifactLookup,
    ) -> PortFuture<'_, Result<Vec<ArtifactCandidate>, ArtifactStoreError>>;

    fn load_for_project(
        &self,
        project_id: ProjectId,
        bundle_id: ArtifactBundleId,
    ) -> PortFuture<'_, Result<ArtifactBundle, ArtifactStoreError>>;

    /// Removes one exact binding and returns the new project revision.
    fn remove_binding(
        &self,
        organization_id: metric_domain::OrganizationId,
        bundle_id: ArtifactBundleId,
        binding: ArtifactBinding,
        orphan_at: Timestamp,
    ) -> PortFuture<'_, Result<Option<u64>, ArtifactStoreError>>;

    fn recoverable_uploads(
        &self,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ArtifactUploadRecord>, ArtifactStoreError>>;

    fn claim_gc(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        claim: [u8; 16],
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ArtifactGcClaim>, ArtifactStoreError>>;

    fn validate_gc_claim(
        &self,
        bundle_id: ArtifactBundleId,
        generation: u32,
        claim: [u8; 16],
        minimum_lease_until: metric_domain::Timestamp,
    ) -> PortFuture<'_, Result<bool, ArtifactStoreError>>;

    fn finish_gc(
        &self,
        bundle_id: ArtifactBundleId,
        generation: u32,
        claim: [u8; 16],
        tombstone_until: Timestamp,
    ) -> PortFuture<'_, Result<bool, ArtifactStoreError>>;
}

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
        _organization_id: metric_domain::OrganizationId,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProjectDeletionStoreError {
    #[error("project deletion conflicts with an existing operation")]
    Conflict,
    #[error("project deletion target does not exist")]
    NotFound,
    #[error("project deletion can no longer be cancelled")]
    NotCancellable,
    #[error("stored project deletion data is invalid")]
    InvalidData,
    #[error("project deletion storage is temporarily unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Copy)]
pub struct ProjectPurgeRequest {
    pub now: Timestamp,
    pub batch_size: usize,
    pub retry_base: Duration,
    pub retry_max: Duration,
    pub completed_retention: Duration,
    pub slug_reservation: Duration,
}

/// Durable control-plane boundary for project deletion and bounded purge work.
pub trait ProjectDeletionStore: Send + Sync + 'static {
    fn request_deletion(
        &self,
        request: ProjectDeletionRequest,
    ) -> PortFuture<'_, Result<ProjectDeletionChange, ProjectDeletionStoreError>>;

    fn cancel_deletion(
        &self,
        project_id: ProjectId,
        operation_id: ProjectDeletionOperationId,
        now: Timestamp,
        completed_retention: Duration,
    ) -> PortFuture<'_, Result<ProjectDeletionChange, ProjectDeletionStoreError>>;

    fn deletion_status(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<ProjectDeletionStatus, ProjectDeletionStoreError>>;

    /// Executes at most one bounded dataset batch for one due operation.
    fn purge_next(
        &self,
        request: ProjectPurgeRequest,
    ) -> PortFuture<'_, Result<Option<ProjectDeletionStatus>, ProjectDeletionStoreError>>;
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

    /// Returns a saturating pending count no larger than `count_limit`.
    fn observe(
        &self,
        count_limit: u64,
    ) -> PortFuture<'_, Result<BacklogObservation, EventBacklogError>>;
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
pub enum NotificationStoreError {
    #[error("notification storage contains invalid data")]
    InvalidData,
    #[error("notification storage is temporarily unavailable")]
    Unavailable,
}

/// Durable Issue-transition expansion and delivery-attempt boundary.
///
/// Implementations must upsert every deterministic delivery before removing the
/// embedded Issue transition. Claiming increments `attempts` and moves
/// `next_attempt_at` beyond the attempt lease in one atomic update.
pub trait NotificationStore: Send + Sync + 'static {
    fn pending_transitions(
        &self,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<IssueNotificationTransition>, NotificationStoreError>>;

    fn matching_rules(
        &self,
        project_id: ProjectId,
        kind: metric_domain::issue::IssueNotificationKind,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<AlertRule>, NotificationStoreError>>;

    fn expand_transition(
        &self,
        transition: IssueNotificationTransition,
        deliveries: Vec<NotificationDelivery>,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>>;

    fn claim_due(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        scan_limit: usize,
    ) -> PortFuture<'_, Result<Option<ClaimedNotificationDelivery>, NotificationStoreError>>;

    fn mark_delivered(
        &self,
        delivery_id: NotificationDeliveryId,
        delivered_at: Timestamp,
        delete_at: Timestamp,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>>;

    fn schedule_retry(
        &self,
        delivery_id: NotificationDeliveryId,
        next_attempt_at: Timestamp,
        error_code: &'static str,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>>;

    fn mark_dead(
        &self,
        delivery_id: NotificationDeliveryId,
        dead_at: Timestamp,
        delete_at: Timestamp,
        error_code: &'static str,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>>;

    fn upsert_destination(
        &self,
        destination: NotificationDestination,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>>;

    fn upsert_rule(&self, rule: AlertRule) -> PortFuture<'_, Result<(), NotificationStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WebhookDeliveryError {
    #[error("webhook destination is permanently invalid or forbidden")]
    Rejected,
    #[error("webhook delivery failed temporarily")]
    Retryable,
    #[error("webhook delivery timed out")]
    Timeout,
    #[error("webhook response exceeds its configured bound")]
    ResponseTooLarge,
    #[error("webhook destination secret is invalid")]
    InvalidSecret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebhookDeliveryReceipt {
    pub status: u16,
    pub retry_after: Option<Duration>,
}

pub trait WebhookDeliveryAdapter: Send + Sync + 'static {
    fn deliver(
        &self,
        claim: ClaimedNotificationDelivery,
    ) -> PortFuture<'_, Result<WebhookDeliveryReceipt, WebhookDeliveryError>>;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReleaseStoreError {
    #[error("release or deploy does not exist")]
    NotFound,
    #[error("release or deploy conflicts with existing data")]
    Conflict,
    #[error("release or deploy request is invalid")]
    InvalidData,
    #[error("release or deploy storage is temporarily unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseIssueKind {
    New,
    Regressed,
}

/// Bounded control-plane persistence for explicit Releases and Deploys.
pub trait ReleaseStore: Send + Sync + 'static {
    fn resolve_projects(
        &self,
        organization_slug: Box<str>,
        project_slugs: Vec<Box<str>>,
    ) -> PortFuture<'_, Result<(metric_domain::OrganizationId, Vec<ProjectId>), ReleaseStoreError>>;

    fn create_release(
        &self,
        command: CreateRelease,
    ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>>;

    fn finalize_release(
        &self,
        command: FinalizeRelease,
    ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>>;

    fn load_release(
        &self,
        organization_id: metric_domain::OrganizationId,
        release_id: metric_domain::finalization::ReleaseId,
    ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>>;

    fn create_deploy(
        &self,
        command: CreateDeploy,
    ) -> PortFuture<'_, Result<DeployRecord, ReleaseStoreError>>;

    fn finish_deploy(
        &self,
        organization_id: metric_domain::OrganizationId,
        deploy_id: DeployId,
        finished_at: Timestamp,
    ) -> PortFuture<'_, Result<DeployRecord, ReleaseStoreError>>;

    fn list_deploys(
        &self,
        organization_id: metric_domain::OrganizationId,
        project_id: ProjectId,
        release_id: metric_domain::finalization::ReleaseId,
        before: Option<(Timestamp, DeployId)>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<DeployRecord>, ReleaseStoreError>>;

    fn list_release_issues(
        &self,
        project_id: ProjectId,
        release: Box<str>,
        kind: ReleaseIssueKind,
        before: Option<(Timestamp, IssueId)>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ReleaseIssueSummary>, ReleaseStoreError>>;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaintenanceTask {
    RetryBacklog,
    EventRetention,
    HourlyRetention,
    CounterReconciliation,
    UploadExpiry,
    BlobOrphanRegistration,
}

impl MaintenanceTask {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RetryBacklog => "retry_backlog",
            Self::EventRetention => "event_retention",
            Self::HourlyRetention => "hourly_retention",
            Self::CounterReconciliation => "counter_reconciliation",
            Self::UploadExpiry => "upload_expiry",
            Self::BlobOrphanRegistration => "blob_orphan_registration",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceCursor(Box<[u8]>);

impl MaintenanceCursor {
    pub const MAX_BYTES: usize = 32;

    #[must_use]
    pub fn new(bytes: impl Into<Box<[u8]>>) -> Option<Self> {
        let bytes = bytes.into();
        (!bytes.is_empty() && bytes.len() <= Self::MAX_BYTES).then_some(Self(bytes))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct MaintenanceRequest {
    pub task: MaintenanceTask,
    pub now: Timestamp,
    pub cursor: Option<MaintenanceCursor>,
    pub batch_size: usize,
    pub event_retention: Duration,
    pub hourly_retention: Duration,
    pub archive_events: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceDisposition {
    Completed,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceResult {
    pub scanned: usize,
    pub changed: usize,
    pub next_cursor: Option<MaintenanceCursor>,
    pub disposition: MaintenanceDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MaintenanceStoreError {
    #[error("maintenance request or stored data is invalid")]
    InvalidData,
    #[error("maintenance storage is temporarily unavailable")]
    Unavailable,
}

/// Capability-specific, bounded maintenance operations. Implementations cannot
/// expose a raw backend query surface to Scheduler.
pub trait MaintenanceStore: Send + Sync + 'static {
    fn run(
        &self,
        request: MaintenanceRequest,
    ) -> PortFuture<'_, Result<MaintenanceResult, MaintenanceStoreError>>;
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
        organization_id: metric_domain::OrganizationId,
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
        organization_id: metric_domain::OrganizationId,
        revoked_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<metric_domain::OrganizationId, AuthStoreError>>;

    fn append_audit(&self, record: AuditRecord) -> PortFuture<'_, Result<(), AuthStoreError>>;

    fn list_api_tokens(
        &self,
        _user_id: UserId,
        _organization_id: metric_domain::OrganizationId,
        _limit: usize,
    ) -> PortFuture<'_, Result<Vec<ApiTokenView>, AuthStoreError>> {
        Box::pin(async { Err(AuthStoreError::Unavailable) })
    }

    fn load_organization(
        &self,
        _organization_id: metric_domain::OrganizationId,
    ) -> PortFuture<'_, Result<OrganizationIdentity, AuthStoreError>> {
        Box::pin(async { Err(AuthStoreError::Unavailable) })
    }

    fn list_organization_members(
        &self,
        _organization_id: metric_domain::OrganizationId,
        _limit: usize,
    ) -> PortFuture<'_, Result<Vec<OrganizationMemberView>, AuthStoreError>> {
        Box::pin(async { Err(AuthStoreError::Unavailable) })
    }

    fn list_audit_log(
        &self,
        _organization_id: metric_domain::OrganizationId,
        _limit: usize,
    ) -> PortFuture<'_, Result<Vec<AuditLogView>, AuthStoreError>> {
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
        before: Option<metric_domain::api::EventAnchor>,
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
        before: Option<metric_domain::api::ActivityAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<ActivityPage, InvestigationStoreError>>;

    fn list_releases(
        &self,
        organization_id: metric_domain::OrganizationId,
        project_id: ProjectId,
        before: Option<metric_domain::api::ReleaseAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<ReleasePage, InvestigationStoreError>>;

    fn list_environments(
        &self,
        project_id: ProjectId,
        before: Option<metric_domain::api::EnvironmentAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<EnvironmentPage, InvestigationStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ExploreStoreError {
    #[error("Explore query data is invalid")]
    InvalidData,
    #[error("Explore storage is temporarily unavailable")]
    Unavailable,
}

/// Executes only an already validated, project-scoped Explore plan.
///
/// Raw backend expressions and collection names never cross this boundary.
pub trait ExploreStore: Send + Sync + 'static {
    fn execute(
        &self,
        plan: metric_domain::explore::ExplorePlan,
    ) -> PortFuture<'_, Result<metric_domain::explore::ExploreResult, ExploreStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DashboardStoreError {
    #[error("dashboard resource does not exist")]
    NotFound,
    #[error("dashboard resource conflicts with current state")]
    Conflict,
    #[error("dashboard data is invalid")]
    InvalidData,
    #[error("dashboard storage is temporarily unavailable")]
    Unavailable,
}

/// Durable project-scoped configuration for saved queries and dashboards.
///
/// Signal rows and derived query results never cross this storage boundary.
pub trait DashboardStore: Send + Sync + 'static {
    fn list_saved_queries(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<metric_domain::dashboards::SavedQuery>, DashboardStoreError>>;

    fn load_saved_query(
        &self,
        project_id: ProjectId,
        id: metric_domain::dashboards::SavedQueryId,
    ) -> PortFuture<'_, Result<metric_domain::dashboards::SavedQuery, DashboardStoreError>>;

    fn insert_saved_query(
        &self,
        saved_query: metric_domain::dashboards::SavedQuery,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>>;

    fn replace_saved_query(
        &self,
        saved_query: metric_domain::dashboards::SavedQuery,
        expected_revision: u64,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>>;

    fn delete_saved_query(
        &self,
        project_id: ProjectId,
        id: metric_domain::dashboards::SavedQueryId,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>>;

    fn list_dashboards(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<metric_domain::dashboards::Dashboard>, DashboardStoreError>>;

    fn load_dashboard(
        &self,
        project_id: ProjectId,
        id: metric_domain::dashboards::DashboardId,
    ) -> PortFuture<'_, Result<metric_domain::dashboards::Dashboard, DashboardStoreError>>;

    fn insert_dashboard(
        &self,
        dashboard: metric_domain::dashboards::Dashboard,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>>;

    fn replace_dashboard(
        &self,
        dashboard: metric_domain::dashboards::Dashboard,
        expected_revision: u64,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>>;

    fn delete_dashboard(
        &self,
        project_id: ProjectId,
        id: metric_domain::dashboards::DashboardId,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogQuery {
    pub from_ns: i64,
    pub until_ns: i64,
    pub severity: Option<LogSeverity>,
    pub message: Option<Box<str>>,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub service: Option<Box<str>>,
    pub trace_id: Option<TraceId>,
    pub before: Option<SignalCursor>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentQuery {
    pub from_ns: i64,
    pub until_ns: i64,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub service: Option<Box<str>>,
    pub before: Option<SignalCursor>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceQuery {
    pub from: Timestamp,
    pub until: Timestamp,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub service: Option<Box<str>>,
    pub limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SignalStoreError {
    #[error("signal target does not exist")]
    NotFound,
    #[error("signal identity conflicts with existing data")]
    Conflict,
    #[error("signal data is invalid")]
    InvalidData,
    #[error("signal lane capacity is exhausted")]
    Capacity,
    #[error("signal storage is temporarily unavailable")]
    Unavailable,
}

/// Dedicated durable Log admission boundary.
///
/// Implementations may buffer and micro-batch records, but a successful response
/// means that every returned record is already durable.
pub trait LogSink: Send + Sync + 'static {
    fn persist_logs(
        &self,
        records: Vec<LogRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>>;
}

/// Dedicated durable Span admission boundary.
///
/// Transactions are normalized into one or more terminal Span records before this
/// boundary. Implementations may buffer and micro-batch records, but a successful
/// response means that every returned record is already durable.
pub trait SpanSink: Send + Sync + 'static {
    fn persist_spans(
        &self,
        records: Vec<SpanRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>>;
}

/// Dedicated durable Session admission boundary.
pub trait SessionSink: Send + Sync + 'static {
    fn persist_sessions(
        &self,
        updates: Vec<SessionUpdate>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>>;
}

/// Session source-of-truth boundary. It is intentionally separate from SignalStore
/// because one SDK Session receives deterministic lifecycle upserts.
pub trait SessionStore: Send + Sync + 'static {
    fn persist_sessions(
        &self,
        updates: Vec<SessionUpdate>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>>;

    fn load_session(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
    ) -> PortFuture<'_, Result<SessionRecord, SignalStoreError>>;

    fn terminalize_stale_sessions(
        &self,
        now: Timestamp,
        maximum_active_age: Duration,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>>;

    fn release_health(
        &self,
        project_id: ProjectId,
        release_id: metric_domain::finalization::ReleaseId,
        from: Timestamp,
        until: Timestamp,
    ) -> PortFuture<'_, Result<Vec<ReleaseHealthBucket>, SignalStoreError>>;

    fn rebuild_session_stats(
        &self,
        project_id: ProjectId,
        from: Timestamp,
        until: Timestamp,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FeedbackStoreError {
    #[error("Feedback target does not exist")]
    NotFound,
    #[error("Feedback identity conflicts with existing data")]
    Conflict,
    #[error("Feedback data is invalid")]
    InvalidData,
    #[error("Feedback submission is rate limited")]
    Capacity,
    #[error("Feedback storage is temporarily unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedbackQuery {
    pub status: Option<FeedbackStatus>,
    pub before: Option<FeedbackAnchor>,
    pub limit: usize,
}

/// Low-volume Feedback submission boundary. A successful outcome means that the
/// metadata and every referenced Blob are already durable.
pub trait FeedbackSink: Send + Sync + 'static {
    fn persist_feedback(
        &self,
        feedback: FeedbackRecord,
    ) -> PortFuture<'_, Result<DurableOutcome, FeedbackStoreError>>;
}

/// Project-scoped Feedback investigation and workflow boundary.
pub trait FeedbackStore: Send + Sync + 'static {
    fn list_feedback(
        &self,
        project_id: ProjectId,
        query: FeedbackQuery,
    ) -> PortFuture<'_, Result<FeedbackPage, FeedbackStoreError>>;

    fn load_feedback(
        &self,
        project_id: ProjectId,
        feedback_id: metric_domain::EventId,
    ) -> PortFuture<'_, Result<FeedbackRecord, FeedbackStoreError>>;

    fn update_feedback_status(
        &self,
        project_id: ProjectId,
        feedback_id: metric_domain::EventId,
        status: FeedbackStatus,
        changed_at: Timestamp,
    ) -> PortFuture<'_, Result<FeedbackRecord, FeedbackStoreError>>;
}

/// Signal-specific durable and query boundary. Raw MongoDB filters never cross it.
pub trait SignalStore: Send + Sync + 'static {
    fn persist_logs(
        &self,
        records: Vec<LogRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>>;

    fn persist_spans(
        &self,
        records: Vec<SpanRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>>;

    fn list_logs(
        &self,
        project_id: ProjectId,
        query: LogQuery,
    ) -> PortFuture<'_, Result<SignalPage<LogRecord>, SignalStoreError>>;

    fn load_log(
        &self,
        project_id: ProjectId,
        log_id: LogId,
    ) -> PortFuture<'_, Result<LogRecord, SignalStoreError>>;

    fn list_segments(
        &self,
        project_id: ProjectId,
        query: SegmentQuery,
    ) -> PortFuture<'_, Result<SignalPage<SpanRecord>, SignalStoreError>>;

    fn trace(
        &self,
        project_ids: Vec<ProjectId>,
        trace_id: TraceId,
        maximum_spans: usize,
        maximum_logs: usize,
    ) -> PortFuture<'_, Result<TraceView, SignalStoreError>>;

    fn performance(
        &self,
        project_id: ProjectId,
        query: PerformanceQuery,
    ) -> PortFuture<'_, Result<Vec<PerformanceBucket>, SignalStoreError>>;

    fn rebuild_span_stats(
        &self,
        project_id: ProjectId,
        from: Timestamp,
        until: Timestamp,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>>;
}
