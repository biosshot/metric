//! Capability-specific ports used by the Phase 1 Ingest application service.

use std::{future::Future, pin::Pin};

use faultkeep_domain::{AcceptedEvent, DsnKey, ProjectSnapshot, Timestamp};
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

pub trait RandomSource: Send + Sync + 'static {
    fn fill_bytes(&self, output: &mut [u8]);
}
