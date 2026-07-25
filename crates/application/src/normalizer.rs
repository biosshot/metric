//! Pure deterministic normalization of scrubbed Sentry Error Events.

use std::collections::{BTreeMap, BTreeSet};

use metric_domain::{
    AcceptedEvent, PrimitiveError, Timestamp,
    event::{
        CanonicalValue, EventLevel, EventPlatform, NormalizationDiagnostic,
        NormalizationDiagnosticCode, NormalizedBreadcrumb, NormalizedEvent, NormalizedEventBody,
        NormalizedException, NormalizedFrame, NormalizedTag,
    },
};
use serde_json::{Map, Value, json};
use thiserror::Error;

const HARD_MAX_DEPTH: usize = 32;
const HARD_MAX_NODES: usize = 65_536;
const HARD_MAX_COLLECTION: usize = 4_096;
const HARD_MAX_DIAGNOSTICS: usize = 256;
const HARD_MAX_STRING_BYTES: usize = 1024 * 1024;
const MAX_RELEASE_BYTES: usize = 200;
const MAX_DIST_BYTES: usize = 64;
const MAX_ENVIRONMENT_BYTES: usize = 64;
const MAX_PLATFORM_BYTES: usize = 64;
const MAX_LOGGER_BYTES: usize = 256;
const MAX_MESSAGE_BYTES: usize = 8 * 1024;
const MAX_FIELD_BYTES: usize = 2 * 1024;
const MAX_CONTEXT_LINE_BYTES: usize = 4 * 1024;
const MAX_TAG_KEY_BYTES: usize = 200;
const MAX_TAG_VALUE_BYTES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizerLimits {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_diagnostics: usize,
    pub max_exceptions: usize,
    pub max_frames: usize,
    pub max_tags: usize,
    pub max_breadcrumbs: usize,
    pub max_fingerprint: usize,
    pub max_unknown_fields: usize,
    pub max_object_fields: usize,
    pub max_array_items: usize,
    pub max_string_bytes: usize,
}

impl Default for NormalizerLimits {
    fn default() -> Self {
        Self {
            max_depth: 16,
            max_nodes: 16_384,
            max_diagnostics: 64,
            max_exceptions: 64,
            max_frames: 512,
            max_tags: 200,
            max_breadcrumbs: 100,
            max_fingerprint: 32,
            max_unknown_fields: 128,
            max_object_fields: 256,
            max_array_items: 512,
            max_string_bytes: 16 * 1024,
        }
    }
}

impl NormalizerLimits {
    pub fn validate(self) -> Result<Self, NormalizerConfigError> {
        let values = [
            self.max_depth,
            self.max_nodes,
            self.max_diagnostics,
            self.max_exceptions,
            self.max_frames,
            self.max_tags,
            self.max_breadcrumbs,
            self.max_fingerprint,
            self.max_unknown_fields,
            self.max_object_fields,
            self.max_array_items,
            self.max_string_bytes,
        ];
        if values.contains(&0)
            || self.max_depth > HARD_MAX_DEPTH
            || self.max_nodes > HARD_MAX_NODES
            || self.max_diagnostics > HARD_MAX_DIAGNOSTICS
            || values[3..11]
                .iter()
                .any(|value| *value > HARD_MAX_COLLECTION)
            || self.max_string_bytes > HARD_MAX_STRING_BYTES
        {
            return Err(NormalizerConfigError::OutOfRange);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NormalizerConfigError {
    #[error("normalizer limit is zero or above its hard ceiling")]
    OutOfRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NormalizationError {
    #[error("accepted Event payload is not valid JSON")]
    InvalidJson,
    #[error("accepted Event root is not an object")]
    InvalidRoot,
    #[error("accepted Event exceeds normalization complexity limits")]
    TooComplex,
    #[error("identity-bearing Event field exceeds its protocol bound")]
    IdentityFieldTooLarge,
}

#[derive(Debug, Clone)]
pub struct Normalizer {
    limits: NormalizerLimits,
}

impl Normalizer {
    pub fn new(limits: NormalizerLimits) -> Result<Self, NormalizerConfigError> {
        Ok(Self {
            limits: limits.validate()?,
        })
    }

    #[must_use]
    pub fn limits(&self) -> NormalizerLimits {
        self.limits
    }

    pub fn normalize(&self, event: &AcceptedEvent) -> Result<NormalizedEvent, NormalizationError> {
        let value: Value = serde_json::from_slice(event.payload.as_bytes())
            .map_err(|_| NormalizationError::InvalidJson)?;
        let root = value.as_object().ok_or(NormalizationError::InvalidRoot)?;
        let mut context = Context::new(self.limits);
        let occurred_at = match root.get("timestamp").and_then(parse_timestamp).transpose() {
            Ok(Some(timestamp)) => timestamp,
            Ok(None) => {
                if root.contains_key("timestamp") {
                    context.diagnostic(NormalizationDiagnosticCode::InvalidTimestamp, "timestamp");
                }
                event.received_at
            }
            Err(()) => {
                context.diagnostic(NormalizationDiagnosticCode::InvalidTimestamp, "timestamp");
                event.received_at
            }
        };
        let release = identity_string(root, "release", MAX_RELEASE_BYTES)?;
        let dist = identity_string(root, "dist", MAX_DIST_BYTES)?;
        let environment = identity_string(root, "environment", MAX_ENVIRONMENT_BYTES)?;
        let platform = normalize_platform(root.get("platform"), &mut context);
        let level =
            normalize_level(root.get("level"), "level", &mut context).unwrap_or(EventLevel::Error);
        let logger = optional_string(root, "logger", MAX_LOGGER_BYTES, "logger", &mut context);
        let message = normalize_message(root, &mut context);
        let transaction = optional_string(
            root,
            "transaction",
            MAX_FIELD_BYTES,
            "transaction",
            &mut context,
        );
        let fingerprint = normalize_string_array(
            root.get("fingerprint"),
            self.limits.max_fingerprint,
            MAX_FIELD_BYTES,
            "fingerprint",
            &mut context,
        );
        let exceptions = normalize_exceptions(root.get("exception"), &mut context)?;
        let stacktrace = normalize_stacktrace(root.get("stacktrace"), "stacktrace", &mut context)?;
        let tags = normalize_tags(root.get("tags"), &mut context);
        let request = normalize_optional_object(root.get("request"), "request", &mut context)?;
        let user = normalize_optional_object(root.get("user"), "user", &mut context)?;
        let contexts = normalize_named_object(root.get("contexts"), "contexts", &mut context)?;
        let breadcrumbs = normalize_breadcrumbs(root.get("breadcrumbs"), &mut context)?;
        let unknown = normalize_unknown(
            root,
            &[
                "event_id",
                "timestamp",
                "platform",
                "level",
                "logger",
                "message",
                "logentry",
                "transaction",
                "release",
                "dist",
                "environment",
                "fingerprint",
                "exception",
                "stacktrace",
                "tags",
                "request",
                "user",
                "contexts",
                "breadcrumbs",
            ],
            "unknown",
            &mut context,
        )?;

        Ok(NormalizedEvent {
            project_id: event.project_id,
            event_id: event.event_id,
            received_at: event.received_at,
            policy_revision: event.policy_revision,
            body: NormalizedEventBody {
                occurred_at,
                platform,
                level,
                logger,
                message,
                transaction,
                release,
                dist,
                environment,
                fingerprint,
                exceptions,
                stacktrace,
                tags,
                request,
                user,
                contexts,
                breadcrumbs,
                unknown,
            },
            diagnostics: context.diagnostics,
        })
    }

    /// Stable JSON projection used by the later body codec and idempotence tests.
    pub fn canonical_json(&self, body: &NormalizedEventBody) -> Vec<u8> {
        serde_json::to_vec(&canonical_body_value(body)).expect("domain Event always maps to JSON")
    }
}

pub(crate) fn canonical_body_value(body: &NormalizedEventBody) -> Value {
    body_to_value(body)
}

struct Context {
    limits: NormalizerLimits,
    nodes: usize,
    diagnostics: Vec<NormalizationDiagnostic>,
}

impl Context {
    fn new(limits: NormalizerLimits) -> Self {
        Self {
            limits,
            nodes: 0,
            diagnostics: Vec::with_capacity(limits.max_diagnostics.min(16)),
        }
    }

    fn visit(&mut self, depth: usize) -> Result<(), NormalizationError> {
        self.nodes = self.nodes.saturating_add(1);
        if depth > self.limits.max_depth || self.nodes > self.limits.max_nodes {
            return Err(NormalizationError::TooComplex);
        }
        Ok(())
    }

    fn diagnostic(&mut self, code: NormalizationDiagnosticCode, path: &'static str) {
        if self.diagnostics.len() < self.limits.max_diagnostics
            && !self
                .diagnostics
                .iter()
                .any(|item| item.code == code && item.path.as_ref() == path)
        {
            self.diagnostics.push(NormalizationDiagnostic {
                code,
                path: path.into(),
            });
        }
    }
}

fn identity_string(
    root: &Map<String, Value>,
    key: &str,
    maximum: usize,
) -> Result<Option<Box<str>>, NormalizationError> {
    match root.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) if value.len() <= maximum => Ok(Some(value.clone().into())),
        Some(Value::String(_)) => Err(NormalizationError::IdentityFieldTooLarge),
        Some(_) => Ok(None),
    }
}

fn optional_string(
    root: &Map<String, Value>,
    key: &str,
    maximum: usize,
    path: &'static str,
    context: &mut Context,
) -> Option<Box<str>> {
    root.get(key)
        .and_then(|value| value_string(value, maximum, path, context))
}

fn value_string(
    value: &Value,
    maximum: usize,
    path: &'static str,
    context: &mut Context,
) -> Option<Box<str>> {
    match value {
        Value::Null => None,
        Value::String(value) if value.is_empty() => None,
        Value::String(value) => {
            let maximum = maximum.min(context.limits.max_string_bytes);
            if value.len() <= maximum {
                Some(value.clone().into())
            } else {
                context.diagnostic(NormalizationDiagnosticCode::StringTruncated, path);
                Some(truncate_utf8(value, maximum).into())
            }
        }
        _ => {
            context.diagnostic(NormalizationDiagnosticCode::InvalidFieldType, path);
            None
        }
    }
}

fn truncate_utf8(value: &str, maximum: usize) -> String {
    let mut end = maximum.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn normalize_message(root: &Map<String, Value>, context: &mut Context) -> Option<Box<str>> {
    if let Some(message) = root.get("message") {
        if let Some(message) = value_string(message, MAX_MESSAGE_BYTES, "message", context) {
            return Some(message);
        }
    }
    root.get("logentry")
        .and_then(Value::as_object)
        .and_then(|entry| entry.get("formatted").or_else(|| entry.get("message")))
        .and_then(|value| value_string(value, MAX_MESSAGE_BYTES, "logentry", context))
}

fn normalize_platform(value: Option<&Value>, context: &mut Context) -> EventPlatform {
    let Some(value) = value else {
        return EventPlatform::Other;
    };
    let Some(value) = value_string(value, MAX_PLATFORM_BYTES, "platform", context) else {
        return EventPlatform::Other;
    };
    match value.as_ref() {
        "other" => EventPlatform::Other,
        "javascript" => EventPlatform::JavaScript,
        "node" => EventPlatform::Node,
        "python" => EventPlatform::Python,
        "java" | "android" => EventPlatform::Java,
        "csharp" | "dotnet" => EventPlatform::DotNet,
        "go" => EventPlatform::Go,
        "rust" => EventPlatform::Rust,
        "php" => EventPlatform::Php,
        "ruby" => EventPlatform::Ruby,
        "cocoa" | "objc" | "swift" => EventPlatform::Cocoa,
        "native" | "c" | "cpp" => EventPlatform::Native,
        "dart" | "flutter" => EventPlatform::Dart,
        _ => EventPlatform::Custom(value),
    }
}

fn normalize_level(
    value: Option<&Value>,
    path: &'static str,
    context: &mut Context,
) -> Option<EventLevel> {
    let value = value?;
    let Some(value) = value.as_str() else {
        context.diagnostic(NormalizationDiagnosticCode::InvalidFieldType, path);
        return None;
    };
    match value {
        "debug" => Some(EventLevel::Debug),
        "info" | "log" => Some(EventLevel::Info),
        "warning" | "warn" => Some(EventLevel::Warning),
        "error" => Some(EventLevel::Error),
        "fatal" => Some(EventLevel::Fatal),
        _ => {
            context.diagnostic(NormalizationDiagnosticCode::InvalidLevel, path);
            None
        }
    }
}

fn normalize_string_array(
    value: Option<&Value>,
    maximum: usize,
    string_maximum: usize,
    path: &'static str,
    context: &mut Context,
) -> Vec<Box<str>> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(values) = value.as_array() else {
        context.diagnostic(NormalizationDiagnosticCode::InvalidFieldType, path);
        return Vec::new();
    };
    if values.len() > maximum {
        context.diagnostic(NormalizationDiagnosticCode::CollectionTruncated, path);
    }
    values
        .iter()
        .take(maximum)
        .filter_map(|value| value_string(value, string_maximum, path, context))
        .collect()
}

fn normalize_exceptions(
    value: Option<&Value>,
    context: &mut Context,
) -> Result<Vec<NormalizedException>, NormalizationError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_object()
        .and_then(|object| object.get("values"))
        .unwrap_or(value);
    let Some(values) = values.as_array() else {
        context.diagnostic(NormalizationDiagnosticCode::InvalidFieldType, "exception");
        return Ok(Vec::new());
    };
    if values.len() > context.limits.max_exceptions {
        context.diagnostic(
            NormalizationDiagnosticCode::CollectionTruncated,
            "exception.values",
        );
    }
    values
        .iter()
        .take(context.limits.max_exceptions)
        .filter_map(|value| value.as_object())
        .map(|exception| {
            let thread_id = exception.get("thread_id").and_then(|value| {
                bounded_scalar_string(value, MAX_FIELD_BYTES, "exception.thread_id", context)
            });
            Ok(NormalizedException {
                ty: optional_string(
                    exception,
                    "type",
                    MAX_FIELD_BYTES,
                    "exception.type",
                    context,
                ),
                value: optional_string(
                    exception,
                    "value",
                    MAX_MESSAGE_BYTES,
                    "exception.value",
                    context,
                ),
                module: optional_string(
                    exception,
                    "module",
                    MAX_FIELD_BYTES,
                    "exception.module",
                    context,
                ),
                thread_id,
                mechanism: exception
                    .get("mechanism")
                    .map(|value| canonicalize(value, 1, "exception.mechanism", context))
                    .transpose()?,
                stacktrace: normalize_stacktrace(
                    exception.get("stacktrace"),
                    "exception.stacktrace",
                    context,
                )?,
                raw_stacktrace: normalize_stacktrace(
                    exception.get("raw_stacktrace"),
                    "exception.raw_stacktrace",
                    context,
                )?,
                unknown: normalize_unknown(
                    exception,
                    &[
                        "type",
                        "value",
                        "module",
                        "thread_id",
                        "mechanism",
                        "stacktrace",
                        "raw_stacktrace",
                    ],
                    "exception.unknown",
                    context,
                )?,
            })
        })
        .collect()
}

fn normalize_stacktrace(
    value: Option<&Value>,
    path: &'static str,
    context: &mut Context,
) -> Result<Vec<NormalizedFrame>, NormalizationError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_object()
        .and_then(|object| object.get("frames"))
        .unwrap_or(value);
    let Some(values) = values.as_array() else {
        context.diagnostic(NormalizationDiagnosticCode::InvalidFieldType, path);
        return Ok(Vec::new());
    };
    if values.len() > context.limits.max_frames {
        context.diagnostic(NormalizationDiagnosticCode::CollectionTruncated, path);
    }
    values
        .iter()
        .take(context.limits.max_frames)
        .filter_map(Value::as_object)
        .map(|frame| normalize_frame(frame, context))
        .collect()
}

fn normalize_frame(
    frame: &Map<String, Value>,
    context: &mut Context,
) -> Result<NormalizedFrame, NormalizationError> {
    Ok(NormalizedFrame {
        filename: optional_string(
            frame,
            "filename",
            MAX_FIELD_BYTES,
            "frame.filename",
            context,
        ),
        absolute_path: optional_string(
            frame,
            "abs_path",
            MAX_FIELD_BYTES,
            "frame.abs_path",
            context,
        ),
        function: optional_string(
            frame,
            "function",
            MAX_FIELD_BYTES,
            "frame.function",
            context,
        ),
        module: optional_string(frame, "module", MAX_FIELD_BYTES, "frame.module", context),
        package: optional_string(frame, "package", MAX_FIELD_BYTES, "frame.package", context),
        instruction_address: optional_string(
            frame,
            "instruction_addr",
            MAX_FIELD_BYTES,
            "frame.instruction_addr",
            context,
        ),
        symbol_address: optional_string(
            frame,
            "symbol_addr",
            MAX_FIELD_BYTES,
            "frame.symbol_addr",
            context,
        ),
        line: frame.get("lineno").and_then(nonnegative_integer),
        column: frame.get("colno").and_then(nonnegative_integer),
        in_app: frame.get("in_app").and_then(Value::as_bool),
        context_line: optional_string(
            frame,
            "context_line",
            MAX_CONTEXT_LINE_BYTES,
            "frame.context_line",
            context,
        ),
        pre_context: normalize_string_array(
            frame.get("pre_context"),
            32,
            MAX_CONTEXT_LINE_BYTES,
            "frame.pre_context",
            context,
        ),
        post_context: normalize_string_array(
            frame.get("post_context"),
            32,
            MAX_CONTEXT_LINE_BYTES,
            "frame.post_context",
            context,
        ),
        variables: normalize_named_object(frame.get("vars"), "frame.vars", context)?,
        unknown: normalize_unknown(
            frame,
            &[
                "filename",
                "abs_path",
                "function",
                "module",
                "package",
                "instruction_addr",
                "symbol_addr",
                "lineno",
                "colno",
                "in_app",
                "context_line",
                "pre_context",
                "post_context",
                "vars",
            ],
            "frame.unknown",
            context,
        )?,
    })
}

fn normalize_tags(value: Option<&Value>, context: &mut Context) -> Vec<NormalizedTag> {
    let Some(value) = value else {
        return Vec::new();
    };
    let mut tags = BTreeMap::<String, String>::new();
    let mut duplicate = false;
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if let Some(value) =
                    bounded_scalar_string(value, MAX_TAG_VALUE_BYTES, "tags", context)
                {
                    tags.insert(key.clone(), value.into());
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                let pair = value.as_array().and_then(|pair| {
                    Some((pair.first()?.as_str()?, pair.get(1).and_then(scalar_str)?))
                });
                if let Some((key, value)) = pair {
                    if key.is_empty()
                        || key.len() > MAX_TAG_KEY_BYTES
                        || value.len() > MAX_TAG_VALUE_BYTES
                    {
                        context.diagnostic(NormalizationDiagnosticCode::StringTruncated, "tags");
                        continue;
                    }
                    duplicate |= tags.insert(key.to_owned(), value.to_owned()).is_some();
                }
            }
        }
        _ => context.diagnostic(NormalizationDiagnosticCode::InvalidFieldType, "tags"),
    }
    if duplicate {
        context.diagnostic(NormalizationDiagnosticCode::DuplicateTag, "tags");
    }
    if tags.len() > context.limits.max_tags {
        context.diagnostic(NormalizationDiagnosticCode::CollectionTruncated, "tags");
    }
    tags.into_iter()
        .take(context.limits.max_tags)
        .filter_map(|(key, value)| {
            if key.is_empty() || key.len() > MAX_TAG_KEY_BYTES || value.len() > MAX_TAG_VALUE_BYTES
            {
                context.diagnostic(NormalizationDiagnosticCode::StringTruncated, "tags");
                return None;
            }
            Some(NormalizedTag {
                key: key.into(),
                value: value.into(),
            })
        })
        .collect()
}

fn normalize_optional_object(
    value: Option<&Value>,
    path: &'static str,
    context: &mut Context,
) -> Result<Option<CanonicalValue>, NormalizationError> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    if !value.is_object() {
        context.diagnostic(NormalizationDiagnosticCode::InvalidFieldType, path);
        return Ok(None);
    }
    canonicalize(value, 0, path, context).map(Some)
}

fn normalize_named_object(
    value: Option<&Value>,
    path: &'static str,
    context: &mut Context,
) -> Result<BTreeMap<Box<str>, CanonicalValue>, NormalizationError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let Some(object) = value.as_object() else {
        if !value.is_null() {
            context.diagnostic(NormalizationDiagnosticCode::InvalidFieldType, path);
        }
        return Ok(BTreeMap::new());
    };
    if object.len() > context.limits.max_object_fields {
        context.diagnostic(NormalizationDiagnosticCode::CollectionTruncated, path);
    }
    object
        .iter()
        .take(context.limits.max_object_fields)
        .map(|(key, value)| Ok((key.clone().into(), canonicalize(value, 1, path, context)?)))
        .collect()
}

fn normalize_breadcrumbs(
    value: Option<&Value>,
    context: &mut Context,
) -> Result<Vec<NormalizedBreadcrumb>, NormalizationError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_object()
        .and_then(|object| object.get("values"))
        .unwrap_or(value);
    let Some(values) = values.as_array() else {
        context.diagnostic(NormalizationDiagnosticCode::InvalidFieldType, "breadcrumbs");
        return Ok(Vec::new());
    };
    if values.len() > context.limits.max_breadcrumbs {
        context.diagnostic(
            NormalizationDiagnosticCode::CollectionTruncated,
            "breadcrumbs",
        );
    }
    values
        .iter()
        .take(context.limits.max_breadcrumbs)
        .filter_map(Value::as_object)
        .map(|breadcrumb| {
            let timestamp = match breadcrumb
                .get("timestamp")
                .and_then(parse_timestamp)
                .transpose()
            {
                Ok(timestamp) => timestamp,
                Err(()) => {
                    context.diagnostic(
                        NormalizationDiagnosticCode::InvalidTimestamp,
                        "breadcrumb.timestamp",
                    );
                    None
                }
            };
            Ok(NormalizedBreadcrumb {
                timestamp,
                ty: optional_string(
                    breadcrumb,
                    "type",
                    MAX_FIELD_BYTES,
                    "breadcrumb.type",
                    context,
                ),
                category: optional_string(
                    breadcrumb,
                    "category",
                    MAX_FIELD_BYTES,
                    "breadcrumb.category",
                    context,
                ),
                level: normalize_level(breadcrumb.get("level"), "breadcrumb.level", context),
                message: optional_string(
                    breadcrumb,
                    "message",
                    MAX_MESSAGE_BYTES,
                    "breadcrumb.message",
                    context,
                ),
                data: normalize_named_object(breadcrumb.get("data"), "breadcrumb.data", context)?,
                unknown: normalize_unknown(
                    breadcrumb,
                    &["timestamp", "type", "category", "level", "message", "data"],
                    "breadcrumb.unknown",
                    context,
                )?,
            })
        })
        .collect()
}

fn normalize_unknown(
    object: &Map<String, Value>,
    known: &[&str],
    path: &'static str,
    context: &mut Context,
) -> Result<BTreeMap<Box<str>, CanonicalValue>, NormalizationError> {
    let known = known.iter().copied().collect::<BTreeSet<_>>();
    let unknown_count = object
        .keys()
        .filter(|key| !known.contains(key.as_str()))
        .count();
    if unknown_count > context.limits.max_unknown_fields {
        context.diagnostic(NormalizationDiagnosticCode::UnknownFieldsTruncated, path);
    }
    object
        .iter()
        .filter(|(key, _)| !known.contains(key.as_str()))
        .take(context.limits.max_unknown_fields)
        .map(|(key, value)| Ok((key.clone().into(), canonicalize(value, 1, path, context)?)))
        .collect()
}

fn canonicalize(
    value: &Value,
    depth: usize,
    path: &'static str,
    context: &mut Context,
) -> Result<CanonicalValue, NormalizationError> {
    context.visit(depth)?;
    Ok(match value {
        Value::Null => CanonicalValue::Null,
        Value::Bool(value) => CanonicalValue::Bool(*value),
        Value::Number(value) => CanonicalValue::Number(value.to_string().into()),
        Value::String(value) => {
            if value.len() > context.limits.max_string_bytes {
                context.diagnostic(NormalizationDiagnosticCode::StringTruncated, path);
            }
            CanonicalValue::String(truncate_utf8(value, context.limits.max_string_bytes).into())
        }
        Value::Array(values) => {
            if values.len() > context.limits.max_array_items {
                context.diagnostic(NormalizationDiagnosticCode::CollectionTruncated, path);
            }
            CanonicalValue::Array(
                values
                    .iter()
                    .take(context.limits.max_array_items)
                    .map(|value| canonicalize(value, depth + 1, path, context))
                    .collect::<Result<_, _>>()?,
            )
        }
        Value::Object(values) => {
            if values.len() > context.limits.max_object_fields {
                context.diagnostic(NormalizationDiagnosticCode::CollectionTruncated, path);
            }
            CanonicalValue::Object(
                values
                    .iter()
                    .take(context.limits.max_object_fields)
                    .map(|(key, value)| {
                        Ok((
                            key.clone().into(),
                            canonicalize(value, depth + 1, path, context)?,
                        ))
                    })
                    .collect::<Result<_, NormalizationError>>()?,
            )
        }
    })
}

fn parse_timestamp(value: &Value) -> Option<Result<Timestamp, ()>> {
    match value {
        Value::Number(number) => {
            let seconds = number.as_f64()?;
            if !seconds.is_finite() {
                return Some(Err(()));
            }
            let millis = seconds * 1_000.0;
            if millis < i64::MIN as f64 || millis > i64::MAX as f64 {
                return Some(Err(()));
            }
            Some(Timestamp::from_unix_millis(millis.round() as i64).map_err(|_| ()))
        }
        Value::String(value) => Some(
            parse_rfc3339_millis(value)
                .and_then(Timestamp::from_unix_millis)
                .map_err(|_| ()),
        ),
        _ => Some(Err(())),
    }
}

fn parse_rfc3339_millis(value: &str) -> Result<i64, PrimitiveError> {
    if value.len() < 20
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
        || !matches!(value.as_bytes().get(10), Some(b'T' | b't' | b' '))
        || value.as_bytes().get(13) != Some(&b':')
        || value.as_bytes().get(16) != Some(&b':')
    {
        return Err(PrimitiveError::TimestampOutOfRange);
    }
    let year = parse_digits(value, 0, 4)? as i64;
    let month = parse_digits(value, 5, 2)? as u32;
    let day = parse_digits(value, 8, 2)? as u32;
    let hour = parse_digits(value, 11, 2)? as i64;
    let minute = parse_digits(value, 14, 2)? as i64;
    let second = parse_digits(value, 17, 2)? as i64;
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(PrimitiveError::TimestampOutOfRange);
    }
    let bytes = value.as_bytes();
    let mut cursor = 19;
    let mut millis = 0_i64;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == start {
            return Err(PrimitiveError::TimestampOutOfRange);
        }
        let fraction = &value[start..cursor];
        let first_three = &fraction[..fraction.len().min(3)];
        millis = first_three
            .parse::<i64>()
            .map_err(|_| PrimitiveError::TimestampOutOfRange)?;
        millis *= 10_i64.pow((3 - first_three.len()) as u32);
    }
    let offset_seconds = match bytes.get(cursor) {
        Some(b'Z' | b'z') if cursor + 1 == bytes.len() => 0_i64,
        Some(sign @ (b'+' | b'-'))
            if cursor + 6 == bytes.len() && bytes.get(cursor + 3) == Some(&b':') =>
        {
            let hours = parse_digits(value, cursor + 1, 2)? as i64;
            let minutes = parse_digits(value, cursor + 4, 2)? as i64;
            if hours > 23 || minutes > 59 {
                return Err(PrimitiveError::TimestampOutOfRange);
            }
            let offset = hours * 3_600 + minutes * 60;
            if *sign == b'-' { -offset } else { offset }
        }
        _ => return Err(PrimitiveError::TimestampOutOfRange),
    };
    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)
        .and_then(|value| value.checked_add(hour * 3_600 + minute * 60 + second))
        .and_then(|value| value.checked_sub(offset_seconds))
        .ok_or(PrimitiveError::TimestampOutOfRange)?;
    seconds
        .checked_mul(1_000)
        .and_then(|value| value.checked_add(millis))
        .ok_or(PrimitiveError::TimestampOutOfRange)
}

fn parse_digits(value: &str, start: usize, length: usize) -> Result<u32, PrimitiveError> {
    value
        .get(start..start + length)
        .filter(|value| value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse().ok())
        .ok_or(PrimitiveError::TimestampOutOfRange)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

// Howard Hinnant's proleptic Gregorian civil-date conversion.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn nonnegative_integer(value: &Value) -> Option<u64> {
    value.as_u64()
}

fn scalar_str(value: &Value) -> Option<&str> {
    value.as_str()
}

fn bounded_scalar_string(
    value: &Value,
    maximum: usize,
    path: &'static str,
    context: &mut Context,
) -> Option<Box<str>> {
    match value {
        Value::String(value) if value.len() <= maximum => Some(value.clone().into()),
        Value::String(_) => {
            context.diagnostic(NormalizationDiagnosticCode::StringTruncated, path);
            None
        }
        Value::Number(value) => {
            let value = value.to_string();
            (value.len() <= maximum).then(|| value.into())
        }
        Value::Bool(value) => Some(value.to_string().into()),
        _ => None,
    }
}

fn body_to_value(body: &NormalizedEventBody) -> Value {
    let mut root = Map::new();
    root.insert(
        "timestamp".into(),
        json!(body.occurred_at.unix_millis() as f64 / 1_000.0),
    );
    if body.platform != EventPlatform::Other {
        root.insert("platform".into(), json!(body.platform.as_str()));
    }
    if body.level != EventLevel::Error {
        root.insert("level".into(), json!(body.level.as_str()));
    }
    insert_optional(&mut root, "logger", body.logger.as_deref());
    insert_optional(&mut root, "message", body.message.as_deref());
    insert_optional(&mut root, "transaction", body.transaction.as_deref());
    insert_optional(&mut root, "release", body.release.as_deref());
    insert_optional(&mut root, "dist", body.dist.as_deref());
    insert_optional(&mut root, "environment", body.environment.as_deref());
    if !body.fingerprint.is_empty() {
        root.insert("fingerprint".into(), json!(body.fingerprint));
    }
    if !body.exceptions.is_empty() {
        root.insert(
            "exception".into(),
            json!({"values": body.exceptions.iter().map(exception_to_value).collect::<Vec<_>>() }),
        );
    }
    if !body.stacktrace.is_empty() {
        root.insert("stacktrace".into(), frames_to_value(&body.stacktrace));
    }
    if !body.tags.is_empty() {
        root.insert(
            "tags".into(),
            Value::Array(
                body.tags
                    .iter()
                    .map(|tag| json!([tag.key, tag.value]))
                    .collect(),
            ),
        );
    }
    if let Some(request) = &body.request {
        root.insert("request".into(), canonical_to_value(request));
    }
    if let Some(user) = &body.user {
        root.insert("user".into(), canonical_to_value(user));
    }
    if !body.contexts.is_empty() {
        root.insert("contexts".into(), canonical_map_to_value(&body.contexts));
    }
    if !body.breadcrumbs.is_empty() {
        root.insert(
            "breadcrumbs".into(),
            json!({"values": body.breadcrumbs.iter().map(breadcrumb_to_value).collect::<Vec<_>>() }),
        );
    }
    for (key, value) in &body.unknown {
        root.insert(key.to_string(), canonical_to_value(value));
    }
    Value::Object(root)
}

fn exception_to_value(exception: &NormalizedException) -> Value {
    let mut value = canonical_map_to_json_map(&exception.unknown);
    insert_optional(&mut value, "type", exception.ty.as_deref());
    insert_optional(&mut value, "value", exception.value.as_deref());
    insert_optional(&mut value, "module", exception.module.as_deref());
    insert_optional(&mut value, "thread_id", exception.thread_id.as_deref());
    if let Some(mechanism) = &exception.mechanism {
        value.insert("mechanism".into(), canonical_to_value(mechanism));
    }
    if !exception.stacktrace.is_empty() {
        value.insert("stacktrace".into(), frames_to_value(&exception.stacktrace));
    }
    if !exception.raw_stacktrace.is_empty() {
        value.insert(
            "raw_stacktrace".into(),
            frames_to_value(&exception.raw_stacktrace),
        );
    }
    Value::Object(value)
}

fn frames_to_value(frames: &[NormalizedFrame]) -> Value {
    json!({"frames": frames.iter().map(frame_to_value).collect::<Vec<_>>()})
}

fn frame_to_value(frame: &NormalizedFrame) -> Value {
    let mut value = canonical_map_to_json_map(&frame.unknown);
    insert_optional(&mut value, "filename", frame.filename.as_deref());
    insert_optional(&mut value, "abs_path", frame.absolute_path.as_deref());
    insert_optional(&mut value, "function", frame.function.as_deref());
    insert_optional(&mut value, "module", frame.module.as_deref());
    insert_optional(&mut value, "package", frame.package.as_deref());
    insert_optional(
        &mut value,
        "instruction_addr",
        frame.instruction_address.as_deref(),
    );
    insert_optional(&mut value, "symbol_addr", frame.symbol_address.as_deref());
    if let Some(line) = frame.line {
        value.insert("lineno".into(), json!(line));
    }
    if let Some(column) = frame.column {
        value.insert("colno".into(), json!(column));
    }
    if let Some(in_app) = frame.in_app {
        value.insert("in_app".into(), json!(in_app));
    }
    insert_optional(&mut value, "context_line", frame.context_line.as_deref());
    if !frame.pre_context.is_empty() {
        value.insert("pre_context".into(), json!(frame.pre_context));
    }
    if !frame.post_context.is_empty() {
        value.insert("post_context".into(), json!(frame.post_context));
    }
    if !frame.variables.is_empty() {
        value.insert("vars".into(), canonical_map_to_value(&frame.variables));
    }
    Value::Object(value)
}

fn breadcrumb_to_value(breadcrumb: &NormalizedBreadcrumb) -> Value {
    let mut value = canonical_map_to_json_map(&breadcrumb.unknown);
    if let Some(timestamp) = breadcrumb.timestamp {
        value.insert(
            "timestamp".into(),
            json!(timestamp.unix_millis() as f64 / 1_000.0),
        );
    }
    insert_optional(&mut value, "type", breadcrumb.ty.as_deref());
    insert_optional(&mut value, "category", breadcrumb.category.as_deref());
    if let Some(level) = breadcrumb.level {
        value.insert("level".into(), json!(level.as_str()));
    }
    insert_optional(&mut value, "message", breadcrumb.message.as_deref());
    if !breadcrumb.data.is_empty() {
        value.insert("data".into(), canonical_map_to_value(&breadcrumb.data));
    }
    Value::Object(value)
}

fn insert_optional(root: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        root.insert(key.to_owned(), json!(value));
    }
}

fn canonical_map_to_value(values: &BTreeMap<Box<str>, CanonicalValue>) -> Value {
    Value::Object(canonical_map_to_json_map(values))
}

fn canonical_map_to_json_map(values: &BTreeMap<Box<str>, CanonicalValue>) -> Map<String, Value> {
    values
        .iter()
        .map(|(key, value)| (key.to_string(), canonical_to_value(value)))
        .collect()
}

fn canonical_to_value(value: &CanonicalValue) -> Value {
    match value {
        CanonicalValue::Null => Value::Null,
        CanonicalValue::Bool(value) => Value::Bool(*value),
        CanonicalValue::Number(value) => serde_json::from_str(value).unwrap_or(Value::Null),
        CanonicalValue::String(value) => Value::String(value.to_string()),
        CanonicalValue::Array(values) => {
            Value::Array(values.iter().map(canonical_to_value).collect())
        }
        CanonicalValue::Object(values) => canonical_map_to_value(values),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metric_domain::{EventId, ProjectId, ScrubbedEventPayload};

    fn accepted(json: &str) -> AcceptedEvent {
        AcceptedEvent {
            project_id: ProjectId::new(42).unwrap(),
            event_id: EventId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            received_at: Timestamp::from_unix_millis(1_753_200_000_000).unwrap(),
            policy_revision: 7,
            payload: ScrubbedEventPayload::new(json.as_bytes()),
        }
    }

    #[test]
    fn normalizes_python_sdk_family_golden() {
        let event = accepted(include_str!(
            "../../server/tests/fixtures/python-2.32.0-error-event-v1.json"
        ));
        let output = Normalizer::new(NormalizerLimits::default())
            .unwrap()
            .normalize(&event)
            .unwrap();
        assert_eq!(output.body.platform, EventPlatform::Python);
        assert_eq!(output.body.release.as_deref(), Some("fixture@1.0.0"));
        assert_eq!(output.body.environment.as_deref(), Some("test"));
        assert_eq!(output.body.exceptions.len(), 1);
        assert_eq!(output.body.exceptions[0].stacktrace[0].line, Some(12));
        assert!(output.diagnostics.is_empty());
    }

    #[test]
    fn normalizes_javascript_and_native_family_shapes() {
        let javascript = accepted(
            r#"{"timestamp":1753200000.125,"platform":"javascript","level":"warning","exception":{"values":[{"type":"TypeError","stacktrace":{"frames":[{"filename":"app.min.js","lineno":1,"colno":4}]}}]},"tags":[["region","eu"],["region","us"]]}"#,
        );
        let js = Normalizer::new(NormalizerLimits::default())
            .unwrap()
            .normalize(&javascript)
            .unwrap();
        assert_eq!(js.body.platform, EventPlatform::JavaScript);
        assert_eq!(js.body.level, EventLevel::Warning);
        assert_eq!(js.body.tags[0].value.as_ref(), "us");
        assert!(
            js.diagnostics
                .iter()
                .any(|item| item.code == NormalizationDiagnosticCode::DuplicateTag)
        );

        let native = accepted(
            r#"{"timestamp":"2025-07-22T12:00:00.500+02:00","platform":"native","stacktrace":{"frames":[{"instruction_addr":"0x10","package":"demo"}]}}"#,
        );
        let native = Normalizer::new(NormalizerLimits::default())
            .unwrap()
            .normalize(&native)
            .unwrap();
        assert_eq!(native.body.platform, EventPlatform::Native);
        assert_eq!(native.body.occurred_at.unix_millis(), 1_753_178_400_500);
        assert_eq!(
            native.body.stacktrace[0].instruction_address.as_deref(),
            Some("0x10")
        );
    }

    #[test]
    fn canonical_projection_is_idempotent() {
        let normalizer = Normalizer::new(NormalizerLimits::default()).unwrap();
        let input = accepted(
            r#"{"timestamp":"2026-07-21T18:00:08.054966Z","platform":"python","level":"error","tags":{"z":"2","a":"1"},"request":{"headers":{"x":"y"}},"future":{"b":2,"a":1}}"#,
        );
        let first = normalizer.normalize(&input).unwrap();
        let canonical = normalizer.canonical_json(&first.body);
        let second = normalizer
            .normalize(&AcceptedEvent {
                payload: ScrubbedEventPayload::new(canonical),
                ..input
            })
            .unwrap();
        assert_eq!(first.body, second.body);
        assert!(second.diagnostics.is_empty());
    }

    #[test]
    fn collection_and_complexity_bounds_are_enforced() {
        let limits = NormalizerLimits {
            max_tags: 2,
            max_depth: 3,
            ..NormalizerLimits::default()
        };
        let normalizer = Normalizer::new(limits).unwrap();
        let output = normalizer
            .normalize(&accepted(r#"{"tags":{"c":"3","a":"1","b":"2"}}"#))
            .unwrap();
        assert_eq!(output.body.tags.len(), 2);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|item| item.code == NormalizationDiagnosticCode::CollectionTruncated)
        );
        assert_eq!(
            normalizer.normalize(&accepted(r#"{"future":{"a":{"b":{"c":{"d":1}}}}}"#)),
            Err(NormalizationError::TooComplex)
        );
    }

    #[test]
    fn malformed_structured_fields_are_bounded_fuzz_regressions() {
        let normalizer = Normalizer::new(NormalizerLimits::default()).unwrap();
        for json in [
            r#"{"timestamp":{},"exception":{"values":"no"},"breadcrumbs":{"values":[null,1,{}]}}"#,
            r#"{"tags":[[],["only-key"],[null,{}]],"contexts":[],"request":"secret"}"#,
            r#"{"level":{"nested":true},"stacktrace":{"frames":[null,false,{"vars":[1]}]}}"#,
        ] {
            let output = normalizer.normalize(&accepted(json)).unwrap();
            assert!(output.diagnostics.len() <= normalizer.limits().max_diagnostics);
        }
    }

    #[test]
    fn identity_fields_are_exact_or_rejected_without_truncation() {
        let normalizer = Normalizer::new(NormalizerLimits::default()).unwrap();
        let exact = normalizer
            .normalize(&accepted(
                r#"{"release":"Product@1.0","environment":"Production"}"#,
            ))
            .unwrap();
        assert_eq!(exact.body.release.as_deref(), Some("Product@1.0"));
        assert_eq!(exact.body.environment.as_deref(), Some("Production"));
        let oversized = format!(r#"{{"release":"{}"}}"#, "x".repeat(MAX_RELEASE_BYTES + 1));
        assert_eq!(
            normalizer.normalize(&accepted(&oversized)),
            Err(NormalizationError::IdentityFieldTooLarge)
        );
    }

    #[test]
    fn sdk_family_platform_golden_table() {
        let normalizer = Normalizer::new(NormalizerLimits::default()).unwrap();
        let cases = [
            ("node", EventPlatform::Node),
            ("java", EventPlatform::Java),
            ("csharp", EventPlatform::DotNet),
            ("go", EventPlatform::Go),
            ("rust", EventPlatform::Rust),
            ("php", EventPlatform::Php),
            ("ruby", EventPlatform::Ruby),
            ("cocoa", EventPlatform::Cocoa),
            ("dart", EventPlatform::Dart),
        ];
        for (platform, expected) in cases {
            let input = format!(
                r#"{{"platform":"{platform}","exception":{{"values":[{{"type":"Synthetic","value":"fixture"}}]}}}}"#
            );
            let output = normalizer.normalize(&accepted(&input)).unwrap();
            assert_eq!(output.body.platform, expected, "{platform}");
            assert_eq!(output.body.exceptions.len(), 1, "{platform}");
        }
    }

    #[test]
    fn deterministic_property_corpus_preserves_canonical_fixed_point() {
        let normalizer = Normalizer::new(NormalizerLimits::default()).unwrap();
        for seed in 0..128_u64 {
            let tags = if seed % 2 == 0 {
                json!([["z", seed.to_string()], ["a", "first"]])
            } else {
                json!({"a": "first", "z": seed.to_string()})
            };
            let input_value = json!({
                "timestamp": 1_753_200_000.0 + seed as f64 / 1_000.0,
                "platform": if seed % 3 == 0 { "python" } else { "rust" },
                "tags": tags,
                "contexts": {"trace": {"span_id": format!("{seed:016x}")}},
                "breadcrumbs": {"values": [{"timestamp": 1_753_200_000.0, "message": seed.to_string()}]},
                "future": {"seed": seed, "enabled": seed % 2 == 0}
            });
            let input = serde_json::to_string(&input_value).unwrap();
            let accepted = accepted(&input);
            let first = normalizer.normalize(&accepted).unwrap();
            let repeated = normalizer.normalize(&accepted).unwrap();
            assert_eq!(first, repeated);
            let canonical = normalizer.canonical_json(&first.body);
            let fixed = normalizer
                .normalize(&AcceptedEvent {
                    payload: ScrubbedEventPayload::new(canonical),
                    ..accepted
                })
                .unwrap();
            assert_eq!(first.body, fixed.body, "seed {seed}");
            assert!(fixed.diagnostics.is_empty(), "seed {seed}");
        }
    }

    #[test]
    #[ignore = "Phase 5 CPU/output-allocation baseline runs in release mode"]
    fn performance_normalizer_adr0037_corpus_rps() {
        let normalizer = Normalizer::new(NormalizerLimits::default()).unwrap();
        let sizes = [1_024_usize, 4_096, 16_384, 131_072];
        let iterations = [20_000_u64, 10_000, 4_000, 500];
        let mut rates = [0_f64; 4];
        let mut output_bytes = [0_usize; 4];

        for (index, target_bytes) in sizes.into_iter().enumerate() {
            let payload = serde_json::to_vec(&json!({
                "timestamp": "2026-07-21T18:00:08.054966Z",
                "platform": "rust",
                "exception": {"values": [{
                    "type": "SyntheticError",
                    "stacktrace": {"frames": [{"filename": "main.rs", "lineno": 42, "in_app": true}]}
                }]},
                "tags": {"environment": "benchmark", "fixture": target_bytes.to_string()},
                "extra": "x".repeat(target_bytes.saturating_sub(300))
            }))
            .unwrap();
            let event = AcceptedEvent {
                project_id: ProjectId::new(42).unwrap(),
                event_id: EventId::parse("0123456789abcdef0123456789abcdef").unwrap(),
                received_at: Timestamp::from_unix_millis(1_753_200_000_000).unwrap(),
                policy_revision: 7,
                payload: ScrubbedEventPayload::new(payload),
            };
            let sample = normalizer.normalize(&event).unwrap();
            output_bytes[index] = normalizer.canonical_json(&sample.body).len();
            let started = std::time::Instant::now();
            for _ in 0..iterations[index] {
                std::hint::black_box(normalizer.normalize(&event).unwrap());
            }
            rates[index] = iterations[index] as f64 / started.elapsed().as_secs_f64();
        }

        let weighted_rps =
            1.0 / (0.60 / rates[0] + 0.30 / rates[1] + 0.09 / rates[2] + 0.01 / rates[3]);
        eprintln!(
            "Normalizer Phase 5: rps_1k={:.0},rps_4k={:.0},rps_16k={:.0},rps_128k={:.0},weighted_rps={:.0},out_1k={},out_4k={},out_16k={},out_128k={}",
            rates[0],
            rates[1],
            rates[2],
            rates[3],
            weighted_rps,
            output_bytes[0],
            output_bytes[1],
            output_bytes[2],
            output_bytes[3]
        );
        assert!(
            weighted_rps >= 7_500.0,
            "Normalizer weighted baseline {weighted_rps:.0} RPS is below recovery gate"
        );
    }
}
