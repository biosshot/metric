//! Backend-independent symbolication request and result model.

use crate::{ProjectId, event::NormalizedFrame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolicationKind {
    NotRequired,
    Native,
    JavaScript,
}

impl SymbolicationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Native => "native",
            Self::JavaScript => "javascript",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawTraceOrigin {
    Event,
    Exception { index: usize },
    ExceptionRaw { index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawStacktrace {
    pub origin: RawTraceOrigin,
    pub frames: Vec<NormalizedFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicationModule {
    pub kind: Option<Box<str>>,
    pub debug_id: Option<Box<str>>,
    pub code_id: Option<Box<str>>,
    pub code_file: Option<Box<str>>,
    pub image_address: Option<Box<str>>,
    pub image_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicationRequest {
    pub project_id: ProjectId,
    pub kind: SymbolicationKind,
    pub traces: Vec<RawStacktrace>,
    pub modules: Vec<SymbolicationModule>,
    pub release: Option<Box<str>>,
    pub dist: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicatedFrame {
    pub original_index: usize,
    pub function: Option<Box<str>>,
    pub filename: Option<Box<str>>,
    pub module: Option<Box<str>>,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicatedStacktrace {
    pub origin: RawTraceOrigin,
    pub frames: Vec<SymbolicatedFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendSymbolicationStatus {
    Complete,
    Partial,
    Missing,
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSymbolicationResult {
    pub status: BackendSymbolicationStatus,
    pub derived: Vec<SymbolicatedStacktrace>,
    pub missing_debug_ids: Vec<Box<str>>,
    pub diagnostics: Vec<SymbolicationDiagnosticCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolicationStatus {
    NotRequired,
    Complete,
    Partial,
    Missing,
    Malformed,
    Timeout,
    Unavailable,
    Cancelled,
}

impl SymbolicationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Missing => "missing",
            Self::Malformed => "malformed",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolicationDisposition {
    Continue,
    Retryable,
    FinalizeRaw,
}

impl SymbolicationDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Retryable => "retryable",
            Self::FinalizeRaw => "finalize_raw",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SymbolicationDiagnosticCode {
    BackendPartial,
    MissingDebugFile,
    MalformedDebugFile,
    MalformedBackendResponse,
    RequestLimitExceeded,
    BackendTimeout,
    BackendUnavailable,
    Cancelled,
    BaselineBackendDisabled,
}

impl SymbolicationDiagnosticCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BackendPartial => "backend_partial",
            Self::MissingDebugFile => "missing_debug_file",
            Self::MalformedDebugFile => "malformed_debug_file",
            Self::MalformedBackendResponse => "malformed_backend_response",
            Self::RequestLimitExceeded => "request_limit_exceeded",
            Self::BackendTimeout => "backend_timeout",
            Self::BackendUnavailable => "backend_unavailable",
            Self::Cancelled => "cancelled",
            Self::BaselineBackendDisabled => "baseline_backend_disabled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicationResult {
    pub kind: SymbolicationKind,
    pub status: SymbolicationStatus,
    pub disposition: SymbolicationDisposition,
    pub raw: Vec<RawStacktrace>,
    pub derived: Vec<SymbolicatedStacktrace>,
    pub missing_debug_ids: Vec<Box<str>>,
    pub diagnostics: Vec<SymbolicationDiagnosticCode>,
}
