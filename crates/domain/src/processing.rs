use crate::{AcceptedEvent, EventKey, ProjectAcceptanceState, ProjectId, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEvent {
    pub event: AcceptedEvent,
    pub attempts: u32,
}

impl PendingEvent {
    #[must_use]
    pub fn fresh(event: AcceptedEvent) -> Self {
        Self { event, attempts: 0 }
    }

    #[must_use]
    pub fn key(&self) -> EventKey {
        EventKey::new(self.event.project_id, self.event.event_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessingProject {
    pub project_id: ProjectId,
    pub state: ProjectAcceptanceState,
    pub error_events_enabled: bool,
    pub grouping_revision: u64,
    pub debug_file_revision: u64,
    pub artifact_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ProcessingErrorCode {
    ProjectUnavailable = 1,
    ProjectNotFound = 2,
    ProjectFenced = 3,
    ErrorCapabilityDisabled = 4,
    ProjectInvalidData = 5,
    Cancelled = 6,
    TotalDeadline = 7,
    StageDeadline = 8,
    NormalizationInvalidJson = 10,
    NormalizationInvalidRoot = 11,
    NormalizationTooComplex = 12,
    NormalizationIdentityTooLarge = 13,
    SymbolicationRetryable = 20,
    GroupingUnsupportedRevision = 30,
    GroupingInputLimit = 31,
    IssueInvalidIdentity = 40,
    IssueInvalidSummary = 41,
    FinalizerInvalidData = 50,
    FinalizerIdentityCollision = 51,
    FinalizerUnavailable = 52,
    RetryExhausted = 60,
}

impl ProcessingErrorCode {
    #[must_use]
    pub const fn stored(self) -> i32 {
        self as i32
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectUnavailable => "project_unavailable",
            Self::ProjectNotFound => "project_not_found",
            Self::ProjectFenced => "project_fenced",
            Self::ErrorCapabilityDisabled => "error_capability_disabled",
            Self::ProjectInvalidData => "project_invalid_data",
            Self::Cancelled => "cancelled",
            Self::TotalDeadline => "total_deadline",
            Self::StageDeadline => "stage_deadline",
            Self::NormalizationInvalidJson => "normalization_invalid_json",
            Self::NormalizationInvalidRoot => "normalization_invalid_root",
            Self::NormalizationTooComplex => "normalization_too_complex",
            Self::NormalizationIdentityTooLarge => "normalization_identity_too_large",
            Self::SymbolicationRetryable => "symbolication_retryable",
            Self::GroupingUnsupportedRevision => "grouping_unsupported_revision",
            Self::GroupingInputLimit => "grouping_input_limit",
            Self::IssueInvalidIdentity => "issue_invalid_identity",
            Self::IssueInvalidSummary => "issue_invalid_summary",
            Self::FinalizerInvalidData => "finalizer_invalid_data",
            Self::FinalizerIdentityCollision => "finalizer_identity_collision",
            Self::FinalizerUnavailable => "finalizer_unavailable",
            Self::RetryExhausted => "retry_exhausted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingFailureDisposition {
    RetryAt(Timestamp),
    PermanentlyFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessingFailure {
    pub key: EventKey,
    pub expected_attempts: u32,
    pub new_attempts: u32,
    pub code: ProcessingErrorCode,
    pub disposition: ProcessingFailureDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingStateChange {
    Updated,
    StaleOrCompleted,
}
