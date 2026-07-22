//! Capability-specific ports used by the Phase 1 Ingest application service.

use std::{future::Future, pin::Pin};

use faultkeep_domain::{
    AcceptedEvent, DsnKey, EventKey, OrganizationIdentity, ProjectAcceptanceState, ProjectId,
    ProjectIdentity, ProjectKeyIdentity, ProjectKeyState, ProjectSnapshot, Timestamp,
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

    fn set_project_acceptance(
        &self,
        project_id: ProjectId,
        state: ProjectAcceptanceState,
    ) -> PortFuture<'_, Result<Vec<DsnKey>, ProjectStoreError>>;
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
    ) -> PortFuture<'a, Result<Vec<AcceptedEvent>, EventBacklogError>>;

    fn observe(&self) -> PortFuture<'_, Result<BacklogObservation, EventBacklogError>>;
}

/// Processing seam. Completion means durable Event eligibility was already changed.
pub trait WorkHandler: Send + Sync + 'static {
    fn handle(&self, event: AcceptedEvent) -> PortFuture<'_, ()>;
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
