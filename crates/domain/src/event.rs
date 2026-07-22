//! Stable normalized Error Event model independent of wire and storage codecs.

use std::collections::BTreeMap;

use crate::{EventId, ProjectId, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedEvent {
    pub project_id: ProjectId,
    pub event_id: EventId,
    pub received_at: Timestamp,
    pub policy_revision: u64,
    pub body: NormalizedEventBody,
    pub diagnostics: Vec<NormalizationDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedEventBody {
    pub occurred_at: Timestamp,
    pub platform: EventPlatform,
    pub level: EventLevel,
    pub logger: Option<Box<str>>,
    pub message: Option<Box<str>>,
    pub transaction: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub dist: Option<Box<str>>,
    pub environment: Option<Box<str>>,
    pub fingerprint: Vec<Box<str>>,
    pub exceptions: Vec<NormalizedException>,
    pub stacktrace: Vec<NormalizedFrame>,
    pub tags: Vec<NormalizedTag>,
    pub request: Option<CanonicalValue>,
    pub user: Option<CanonicalValue>,
    pub contexts: BTreeMap<Box<str>, CanonicalValue>,
    pub breadcrumbs: Vec<NormalizedBreadcrumb>,
    pub unknown: BTreeMap<Box<str>, CanonicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventPlatform {
    Other,
    JavaScript,
    Node,
    Python,
    Java,
    DotNet,
    Go,
    Rust,
    Php,
    Ruby,
    Cocoa,
    Native,
    Dart,
    Custom(Box<str>),
}

impl EventPlatform {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Other => "other",
            Self::JavaScript => "javascript",
            Self::Node => "node",
            Self::Python => "python",
            Self::Java => "java",
            Self::DotNet => "csharp",
            Self::Go => "go",
            Self::Rust => "rust",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Cocoa => "cocoa",
            Self::Native => "native",
            Self::Dart => "dart",
            Self::Custom(value) => value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLevel {
    Debug,
    Info,
    Warning,
    Error,
    Fatal,
}

impl EventLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Fatal => "fatal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedException {
    pub ty: Option<Box<str>>,
    pub value: Option<Box<str>>,
    pub module: Option<Box<str>>,
    pub thread_id: Option<Box<str>>,
    pub mechanism: Option<CanonicalValue>,
    pub stacktrace: Vec<NormalizedFrame>,
    pub raw_stacktrace: Vec<NormalizedFrame>,
    pub unknown: BTreeMap<Box<str>, CanonicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedFrame {
    pub filename: Option<Box<str>>,
    pub absolute_path: Option<Box<str>>,
    pub function: Option<Box<str>>,
    pub module: Option<Box<str>>,
    pub package: Option<Box<str>>,
    pub instruction_address: Option<Box<str>>,
    pub symbol_address: Option<Box<str>>,
    pub line: Option<u64>,
    pub column: Option<u64>,
    pub in_app: Option<bool>,
    pub context_line: Option<Box<str>>,
    pub pre_context: Vec<Box<str>>,
    pub post_context: Vec<Box<str>>,
    pub variables: BTreeMap<Box<str>, CanonicalValue>,
    pub unknown: BTreeMap<Box<str>, CanonicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedTag {
    pub key: Box<str>,
    pub value: Box<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedBreadcrumb {
    pub timestamp: Option<Timestamp>,
    pub ty: Option<Box<str>>,
    pub category: Option<Box<str>>,
    pub level: Option<EventLevel>,
    pub message: Option<Box<str>>,
    pub data: BTreeMap<Box<str>, CanonicalValue>,
    pub unknown: BTreeMap<Box<str>, CanonicalValue>,
}

/// Adapter-independent canonical JSON-compatible value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalValue {
    Null,
    Bool(bool),
    /// Canonical JSON number spelling produced by `serde_json::Number`.
    Number(Box<str>),
    String(Box<str>),
    Array(Vec<Self>),
    Object(BTreeMap<Box<str>, Self>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizationDiagnostic {
    pub code: NormalizationDiagnosticCode,
    pub path: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NormalizationDiagnosticCode {
    InvalidTimestamp,
    InvalidFieldType,
    InvalidLevel,
    CollectionTruncated,
    StringTruncated,
    DuplicateTag,
    UnknownFieldsTruncated,
}

impl NormalizationDiagnosticCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidTimestamp => "invalid_timestamp",
            Self::InvalidFieldType => "invalid_field_type",
            Self::InvalidLevel => "invalid_level",
            Self::CollectionTruncated => "collection_truncated",
            Self::StringTruncated => "string_truncated",
            Self::DuplicateTag => "duplicate_tag",
            Self::UnknownFieldsTruncated => "unknown_fields_truncated",
        }
    }
}
