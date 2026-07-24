//! Signal-specific domain values shared by Structured Logs, Traces and Performance.

use std::{fmt, str::FromStr};

use thiserror::Error;

use crate::{EventId, PrimitiveError, ProjectId, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SignalPrimitiveError {
    #[error("invalid Trace identifier")]
    InvalidTraceId,
    #[error("invalid Span identifier")]
    InvalidSpanId,
    #[error("signal timestamp or duration is invalid")]
    InvalidTime,
    #[error("signal text is invalid")]
    InvalidText,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraceId([u8; 16]);

impl TraceId {
    pub fn parse(value: &str) -> Result<Self, SignalPrimitiveError> {
        let mut bytes = [0_u8; 16];
        if value.len() != 32
            || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            || hex::decode_to_slice(value, &mut bytes).is_err()
            || bytes == [0; 16]
        {
            return Err(SignalPrimitiveError::InvalidTraceId);
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for TraceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl FromStr for TraceId {
    type Err = SignalPrimitiveError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanId([u8; 8]);

impl SpanId {
    pub fn parse(value: &str) -> Result<Self, SignalPrimitiveError> {
        let mut bytes = [0_u8; 8];
        if value.len() != 16
            || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            || hex::decode_to_slice(value, &mut bytes).is_err()
            || bytes == [0; 8]
        {
            return Err(SignalPrimitiveError::InvalidSpanId);
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 8] {
        self.0
    }
}

impl fmt::Debug for SpanId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl FromStr for SpanId {
    type Err = SignalPrimitiveError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogId([u8; 16]);

impl LogId {
    #[must_use]
    pub fn deterministic(
        project_id: ProjectId,
        received_at: Timestamp,
        occurred_at_ns: i64,
        payload: &[u8],
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"structured-log/v1");
        hasher.update(&project_id.get().to_be_bytes());
        hasher.update(&occurred_at_ns.to_be_bytes());
        hasher.update(payload);
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&received_at.unix_millis().to_be_bytes());
        bytes[8..].copy_from_slice(&digest.as_bytes()[..8]);
        Self(bytes)
    }

    pub fn parse(value: &str) -> Result<Self, PrimitiveError> {
        let mut bytes = [0_u8; 16];
        if value.len() != 32
            || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            || hex::decode_to_slice(value, &mut bytes).is_err()
        {
            return Err(PrimitiveError::InvalidEventId);
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for LogId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for LogId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanRecordId([u8; 16]);

impl SpanRecordId {
    #[must_use]
    pub fn deterministic(project_id: ProjectId, trace_id: TraceId, span_id: SpanId) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"span-record/v1");
        hasher.update(&project_id.get().to_be_bytes());
        hasher.update(&trace_id.as_bytes());
        hasher.update(&span_id.as_bytes());
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        Self(bytes)
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for SpanRecordId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogSeverity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl LogSeverity {
    #[must_use]
    pub fn from_wire(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "trace" => Self::Trace,
            "debug" => Self::Debug,
            "warn" | "warning" => Self::Warn,
            "error" => Self::Error,
            "fatal" => Self::Fatal,
            _ => Self::Info,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fatal => "fatal",
        }
    }

    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Trace => 1,
            Self::Debug => 2,
            Self::Info => 3,
            Self::Warn => 4,
            Self::Error => 5,
            Self::Fatal => 6,
        }
    }

    pub fn from_code(value: i32) -> Result<Self, SignalPrimitiveError> {
        match value {
            1 => Ok(Self::Trace),
            2 => Ok(Self::Debug),
            3 => Ok(Self::Info),
            4 => Ok(Self::Warn),
            5 => Ok(Self::Error),
            6 => Ok(Self::Fatal),
            _ => Err(SignalPrimitiveError::InvalidText),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpanOperationClass {
    Other,
    HttpServer,
    HttpClient,
    Database,
    Cache,
    Queue,
    File,
    Rpc,
    Function,
    Task,
    Ui,
    Resource,
}

impl SpanOperationClass {
    #[must_use]
    pub fn from_operation(value: &str) -> Self {
        let value = value.to_ascii_lowercase();
        if value.starts_with("http.server") || value.starts_with("server") {
            Self::HttpServer
        } else if value.starts_with("http") {
            Self::HttpClient
        } else if value.starts_with("db") || value.contains("database") {
            Self::Database
        } else if value.starts_with("cache") {
            Self::Cache
        } else if value.starts_with("queue") || value.starts_with("messaging") {
            Self::Queue
        } else if value.starts_with("file") {
            Self::File
        } else if value.starts_with("rpc") {
            Self::Rpc
        } else if value.starts_with("function") {
            Self::Function
        } else if value.starts_with("task") {
            Self::Task
        } else if value.starts_with("ui") || value.starts_with("navigation") {
            Self::Ui
        } else if value.starts_with("resource") {
            Self::Resource
        } else {
            Self::Other
        }
    }

    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }

    pub fn from_code(value: i32) -> Result<Self, SignalPrimitiveError> {
        match value {
            0 => Ok(Self::Other),
            1 => Ok(Self::HttpServer),
            2 => Ok(Self::HttpClient),
            3 => Ok(Self::Database),
            4 => Ok(Self::Cache),
            5 => Ok(Self::Queue),
            6 => Ok(Self::File),
            7 => Ok(Self::Rpc),
            8 => Ok(Self::Function),
            9 => Ok(Self::Task),
            10 => Ok(Self::Ui),
            11 => Ok(Self::Resource),
            _ => Err(SignalPrimitiveError::InvalidText),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Other => "other",
            Self::HttpServer => "http.server",
            Self::HttpClient => "http.client",
            Self::Database => "database",
            Self::Cache => "cache",
            Self::Queue => "queue",
            Self::File => "file",
            Self::Rpc => "rpc",
            Self::Function => "function",
            Self::Task => "task",
            Self::Ui => "ui",
            Self::Resource => "resource",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalBody(Box<[u8]>);

impl SignalBody {
    #[must_use]
    pub fn new(bytes: impl Into<Box<[u8]>>) -> Self {
        Self(bytes.into())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    pub id: LogId,
    pub project_id: ProjectId,
    pub received_at: Timestamp,
    pub occurred_at_ns: i64,
    pub severity: LogSeverity,
    pub message: Box<str>,
    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub service: Option<Box<str>>,
    pub body: SignalBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanRecord {
    pub id: SpanRecordId,
    pub project_id: ProjectId,
    pub received_at: Timestamp,
    pub started_at_ns: i64,
    pub duration_ns: i64,
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub parent_span_id: Option<SpanId>,
    pub is_segment: bool,
    pub operation_class: SpanOperationClass,
    pub operation: Box<str>,
    pub status: Box<str>,
    pub name: Box<str>,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub service: Option<Box<str>>,
    pub insight_flags: u32,
    pub body: SignalBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalCursor {
    pub time_ns: i64,
    pub id: [u8; 16],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalPage<T> {
    pub items: Vec<T>,
    pub next: Option<SignalCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceView {
    pub trace_id: TraceId,
    pub spans: Vec<SpanRecord>,
    pub logs: Vec<LogRecord>,
    pub errors: Vec<EventId>,
    pub partial: bool,
    pub omitted_spans: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PerformanceBucket {
    pub hour: Timestamp,
    pub name: Box<str>,
    pub service: Option<Box<str>>,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub representative_trace_id: TraceId,
    pub operation: SpanOperationClass,
    pub count: u64,
    pub failure_count: u64,
    pub average_duration_ms: f64,
    pub p50_ms: f64,
    pub p75_ms: f64,
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_have_fixed_wire_width_and_stable_span_identity() {
        let trace = TraceId::parse("0123456789abcdef0123456789abcdef").unwrap();
        let span = SpanId::parse("0123456789abcdef").unwrap();
        let project = ProjectId::new(42).unwrap();
        assert_eq!(trace.to_string(), "0123456789abcdef0123456789abcdef");
        assert_eq!(span.to_string(), "0123456789abcdef");
        assert_eq!(
            SpanRecordId::deterministic(project, trace, span).as_bytes(),
            SpanRecordId::deterministic(project, trace, span).as_bytes()
        );
        assert!(TraceId::parse("00000000000000000000000000000000").is_err());
        assert!(SpanId::parse("0000000000000000").is_err());
    }

    #[test]
    fn operation_class_is_bounded() {
        assert_eq!(
            SpanOperationClass::from_operation("db.sql.query"),
            SpanOperationClass::Database
        );
        assert_eq!(
            SpanOperationClass::from_operation("custom.unbounded"),
            SpanOperationClass::Other
        );
    }
}
