//! Project-deletion lifecycle values shared by application ports and adapters.

use crate::{DsnKey, OrganizationId, ProjectId, Timestamp, auth::UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectDeletionOperationId([u8; 16]);

impl ProjectDeletionOperationId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectDeletionPhase {
    PendingGrace,
    Purging,
    Deleted,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDeletionStatus {
    pub operation_id: ProjectDeletionOperationId,
    pub project_id: ProjectId,
    pub organization_id: OrganizationId,
    pub phase: ProjectDeletionPhase,
    pub dataset_code: u16,
    pub reconciliation_pass: bool,
    pub requested_at: Timestamp,
    pub purge_after: Timestamp,
    pub completed_at: Option<Timestamp>,
    pub next_attempt_at: Timestamp,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectDeletionRequest {
    pub operation_id: ProjectDeletionOperationId,
    pub project_id: ProjectId,
    pub organization_id: OrganizationId,
    pub requested_by: UserId,
    pub requested_at: Timestamp,
    pub purge_after: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDeletionChange {
    pub status: ProjectDeletionStatus,
    pub affected_keys: Vec<DsnKey>,
}
