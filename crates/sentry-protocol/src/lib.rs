//! Bounded Sentry wire parsing for the Phase 1 Error Event transport.

use std::collections::BTreeSet;

use metric_domain::{DsnKey, EventId, ProjectId};
use serde::Deserialize;
use thiserror::Error;
use url::Url;

const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_AUTH_BYTES: usize = 2 * 1024;
const MAX_CLIENT_REPORT_ENTRIES: usize = 100;
const MAX_CLIENT_REPORT_TEXT_BYTES: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct EnvelopeLimits {
    pub max_items: usize,
    pub max_event_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct AttachmentLimits {
    pub max_count: usize,
    pub max_item_bytes: usize,
    pub max_total_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsnAuth {
    pub key: DsnKey,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEnvelope {
    pub event_id: Option<EventId>,
    pub dsn: Option<DsnAuth>,
    /// The single Error item that owns dependent attachment items. "Primary" is an
    /// Envelope relationship, not a claim that Errors are the only telemetry signal.
    pub primary: Option<RawEvent>,
    /// Independently durable telemetry items: Logs, Transactions, standalone Spans
    /// and application Sessions. Errors occupy the dependency-root role.
    pub signals: Vec<RawSignal>,
    pub attachments: Vec<RawAttachment>,
    pub discarded: Vec<DiscardedItem>,
    pub client_report_quantity: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawSignalKind {
    Log,
    Transaction,
    Span,
    Session,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RawSignal {
    pub kind: RawSignalKind,
    pub bytes: Box<[u8]>,
}

impl std::fmt::Debug for RawSignal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RawSignal")
            .field("kind", &self.kind)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RawAttachment {
    pub position: u32,
    pub filename: Box<str>,
    pub content_type: Box<str>,
    pub attachment_type: Box<str>,
    pub bytes: Box<[u8]>,
}

impl std::fmt::Debug for RawAttachment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RawAttachment")
            .field("position", &self.position)
            .field("filename", &self.filename)
            .field("content_type", &self.content_type)
            .field("attachment_type", &self.attachment_type)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RawEvent {
    pub header_event_id: Option<EventId>,
    pub bytes: Box<[u8]>,
}

impl std::fmt::Debug for RawEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RawEvent")
            .field("header_event_id", &self.header_event_id)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DisabledCategory {
    Transaction,
    Session,
    Profile,
    Replay,
    CheckIn,
    Span,
    Statsd,
    Attachment,
    OtherKnown,
}

impl DisabledCategory {
    #[must_use]
    pub const fn sentry_name(self) -> &'static str {
        match self {
            Self::Transaction => "transaction",
            Self::Session => "session",
            Self::Profile => "profile",
            Self::Replay => "replay",
            Self::CheckIn => "monitor",
            Self::Span => "span",
            Self::Statsd => "metric_bucket",
            Self::Attachment => "attachment",
            Self::OtherKnown => "default",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscardedItem {
    pub category: Option<DisabledCategory>,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolErrorKind {
    Invalid,
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("Sentry request is invalid")]
pub struct ProtocolError {
    kind: ProtocolErrorKind,
    code: &'static str,
}

impl ProtocolError {
    #[must_use]
    pub const fn kind(self) -> ProtocolErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }

    const fn invalid(code: &'static str) -> Self {
        Self {
            kind: ProtocolErrorKind::Invalid,
            code,
        }
    }

    const fn too_large(code: &'static str) -> Self {
        Self {
            kind: ProtocolErrorKind::TooLarge,
            code,
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireEnvelopeHeader {
    event_id: Option<String>,
    dsn: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireItemHeader {
    #[serde(rename = "type")]
    kind: Option<String>,
    length: Option<u64>,
    event_id: Option<String>,
    filename: Option<String>,
    content_type: Option<String>,
    attachment_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireClientReport {
    discarded_events: Vec<WireClientReportEntry>,
}

#[derive(Debug, Deserialize)]
struct WireClientReportEntry {
    reason: String,
    category: String,
    quantity: u64,
}

pub fn parse_envelope(
    body: &[u8],
    limits: EnvelopeLimits,
) -> Result<ParsedEnvelope, ProtocolError> {
    parse_envelope_with_attachments(
        body,
        limits,
        AttachmentLimits {
            max_count: 0,
            max_item_bytes: 0,
            max_total_bytes: 0,
        },
    )
}

pub fn parse_envelope_with_attachments(
    body: &[u8],
    limits: EnvelopeLimits,
    attachment_limits: AttachmentLimits,
) -> Result<ParsedEnvelope, ProtocolError> {
    let (header_bytes, mut cursor) = line_at(body, 0)?;
    let header: WireEnvelopeHeader = serde_json::from_slice(header_bytes)
        .map_err(|_| ProtocolError::invalid("invalid_envelope_header"))?;
    let event_id = parse_optional_event_id(header.event_id.as_deref())?;
    let dsn = header.dsn.as_deref().map(parse_dsn).transpose()?;
    let mut primary = None;
    let mut signals = Vec::new();
    let mut attachments = Vec::new();
    let mut attachment_bytes = 0_usize;
    let mut discarded = Vec::new();
    let mut client_report_quantity = 0_u64;
    let mut item_count = 0_usize;

    while cursor < body.len() {
        item_count += 1;
        if item_count > limits.max_items {
            return Err(ProtocolError::too_large("too_many_items"));
        }
        let (item_header_bytes, payload_start) = line_at(body, cursor)?;
        let item_header: WireItemHeader = serde_json::from_slice(item_header_bytes)
            .map_err(|_| ProtocolError::invalid("invalid_item_header"))?;
        let kind = item_header.kind.as_deref().unwrap_or("event");
        let (payload, next_cursor) = match item_header.length {
            Some(length) => {
                let length = usize::try_from(length)
                    .map_err(|_| ProtocolError::too_large("item_length_overflow"))?;
                let payload_end = payload_start
                    .checked_add(length)
                    .ok_or_else(|| ProtocolError::too_large("item_length_overflow"))?;
                if payload_end > body.len() {
                    return Err(ProtocolError::invalid("truncated_item"));
                }
                let mut next_cursor = payload_end;
                if next_cursor < body.len() {
                    if body[next_cursor] != b'\n' {
                        return Err(ProtocolError::invalid("missing_item_separator"));
                    }
                    next_cursor += 1;
                }
                (&body[payload_start..payload_end], next_cursor)
            }
            None => lengthless_payload(body, payload_start, limits.max_event_bytes)?,
        };
        let length = payload.len();
        match classify_item(kind) {
            ItemClass::Event => {
                if length > limits.max_event_bytes {
                    return Err(ProtocolError::too_large("event_too_large"));
                }
                if primary.is_some() {
                    return Err(ProtocolError::invalid("multiple_primary_events"));
                }
                primary = Some(RawEvent {
                    header_event_id: parse_optional_event_id(item_header.event_id.as_deref())?,
                    bytes: payload.into(),
                });
            }
            ItemClass::ClientReport => {
                client_report_quantity =
                    client_report_quantity.saturating_add(parse_client_report(payload)?);
            }
            ItemClass::Attachment => {
                if attachment_limits.max_count == 0 {
                    discarded.push(DiscardedItem {
                        category: Some(DisabledCategory::Attachment),
                        reason: "attachment_policy_disabled",
                    });
                } else {
                    if attachments.len() >= attachment_limits.max_count {
                        return Err(ProtocolError::too_large("too_many_attachments"));
                    }
                    if length > attachment_limits.max_item_bytes {
                        return Err(ProtocolError::too_large("attachment_too_large"));
                    }
                    attachment_bytes = attachment_bytes
                        .checked_add(length)
                        .ok_or_else(|| ProtocolError::too_large("attachments_too_large"))?;
                    if attachment_bytes > attachment_limits.max_total_bytes {
                        return Err(ProtocolError::too_large("attachments_too_large"));
                    }
                    let position = u32::try_from(item_count)
                        .map_err(|_| ProtocolError::too_large("too_many_items"))?;
                    attachments.push(RawAttachment {
                        position,
                        filename: bounded_metadata(
                            item_header.filename.as_deref().unwrap_or("attachment"),
                            "attachment_filename_too_large",
                        )?,
                        content_type: bounded_metadata(
                            item_header
                                .content_type
                                .as_deref()
                                .unwrap_or("application/octet-stream"),
                            "attachment_content_type_too_large",
                        )?,
                        attachment_type: bounded_metadata(
                            item_header
                                .attachment_type
                                .as_deref()
                                .unwrap_or("event.attachment"),
                            "attachment_type_too_large",
                        )?,
                        bytes: payload.into(),
                    });
                }
            }
            ItemClass::Signal(kind) => {
                if length > limits.max_event_bytes {
                    return Err(ProtocolError::too_large("signal_too_large"));
                }
                signals.push(RawSignal {
                    kind,
                    bytes: payload.into(),
                });
            }
            ItemClass::Disabled(category) => discarded.push(DiscardedItem {
                category: Some(category),
                reason: "feature_disabled",
            }),
            ItemClass::Unknown => discarded.push(DiscardedItem {
                category: None,
                reason: "unknown_item_type",
            }),
        }
        cursor = next_cursor;
    }

    if primary.is_none()
        && signals.is_empty()
        && discarded.is_empty()
        && client_report_quantity == 0
    {
        return Err(ProtocolError::invalid("empty_envelope"));
    }
    Ok(ParsedEnvelope {
        event_id,
        dsn,
        primary,
        signals,
        attachments,
        discarded,
        client_report_quantity,
    })
}

fn bounded_metadata(value: &str, code: &'static str) -> Result<Box<str>, ProtocolError> {
    if value.len() > 256 || value.chars().any(char::is_control) {
        return Err(ProtocolError::too_large(code));
    }
    Ok(value.into())
}

pub fn parse_store_event(body: &[u8], max_event_bytes: usize) -> Result<RawEvent, ProtocolError> {
    if body.len() > max_event_bytes {
        return Err(ProtocolError::too_large("event_too_large"));
    }
    if body.is_empty() {
        return Err(ProtocolError::invalid("empty_event"));
    }
    Ok(RawEvent {
        header_event_id: None,
        bytes: body.into(),
    })
}

pub fn parse_x_sentry_auth(value: &str) -> Result<DsnKey, ProtocolError> {
    if value.len() > MAX_AUTH_BYTES {
        return Err(ProtocolError::invalid("auth_header_too_large"));
    }
    let fields = value
        .strip_prefix("Sentry ")
        .ok_or_else(|| ProtocolError::invalid("invalid_auth_scheme"))?;
    let mut keys = BTreeSet::new();
    for field in fields.split(',') {
        let Some((name, value)) = field.trim().split_once('=') else {
            return Err(ProtocolError::invalid("invalid_auth_field"));
        };
        if name.trim() == "sentry_key" {
            keys.insert(
                DsnKey::parse(value.trim())
                    .map_err(|_| ProtocolError::invalid("invalid_dsn_key"))?,
            );
        }
    }
    if keys.len() != 1 {
        return Err(ProtocolError::invalid("missing_or_conflicting_dsn_key"));
    }
    Ok(*keys.first().expect("one key was checked"))
}

pub fn parse_query_auth(query: &str) -> Result<Option<DsnKey>, ProtocolError> {
    if query.len() > MAX_AUTH_BYTES {
        return Err(ProtocolError::invalid("auth_query_too_large"));
    }
    let mut keys = BTreeSet::new();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if name == "sentry_key" {
            keys.insert(
                DsnKey::parse(&value).map_err(|_| ProtocolError::invalid("invalid_dsn_key"))?,
            );
        }
    }
    if keys.len() > 1 {
        return Err(ProtocolError::invalid("conflicting_dsn_key"));
    }
    Ok(keys.first().copied())
}

fn line_at(body: &[u8], start: usize) -> Result<(&[u8], usize), ProtocolError> {
    let remaining = body
        .get(start..)
        .ok_or_else(|| ProtocolError::invalid("invalid_framing"))?;
    let newline = remaining
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| ProtocolError::invalid("missing_header_newline"))?;
    if newline > MAX_HEADER_BYTES {
        return Err(ProtocolError::too_large("header_too_large"));
    }
    Ok((&remaining[..newline], start + newline + 1))
}

fn lengthless_payload(
    body: &[u8],
    start: usize,
    max_bytes: usize,
) -> Result<(&[u8], usize), ProtocolError> {
    let remaining = body
        .get(start..)
        .ok_or_else(|| ProtocolError::invalid("truncated_item"))?;
    let bounded = remaining.len().min(max_bytes.saturating_add(1));
    if let Some(newline) = remaining[..bounded].iter().position(|byte| *byte == b'\n') {
        return Ok((&remaining[..newline], start + newline + 1));
    }
    if remaining.len() > max_bytes {
        return Err(ProtocolError::too_large("lengthless_item_too_large"));
    }
    Ok((remaining, body.len()))
}

fn parse_optional_event_id(value: Option<&str>) -> Result<Option<EventId>, ProtocolError> {
    value.map(parse_wire_event_id).transpose()
}

fn parse_wire_event_id(value: &str) -> Result<EventId, ProtocolError> {
    if let Ok(event_id) = EventId::parse(value) {
        return Ok(event_id);
    }
    if value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
    {
        let compact = value
            .bytes()
            .filter(|byte| *byte != b'-')
            .map(char::from)
            .collect::<String>();
        return EventId::parse(&compact).map_err(|_| ProtocolError::invalid("invalid_event_id"));
    }
    Err(ProtocolError::invalid("invalid_event_id"))
}

fn parse_dsn(value: &str) -> Result<DsnAuth, ProtocolError> {
    if value.len() > MAX_AUTH_BYTES {
        return Err(ProtocolError::invalid("dsn_too_large"));
    }
    let dsn = Url::parse(value).map_err(|_| ProtocolError::invalid("invalid_dsn"))?;
    if !matches!(dsn.scheme(), "http" | "https") {
        return Err(ProtocolError::invalid("invalid_dsn_scheme"));
    }
    let key =
        DsnKey::parse(dsn.username()).map_err(|_| ProtocolError::invalid("invalid_dsn_key"))?;
    let project = dsn
        .path_segments()
        .and_then(Iterator::last)
        .ok_or_else(|| ProtocolError::invalid("missing_dsn_project"))?
        .parse::<i32>()
        .map_err(|_| ProtocolError::invalid("invalid_dsn_project"))?;
    let project_id =
        ProjectId::new(project).map_err(|_| ProtocolError::invalid("invalid_dsn_project"))?;
    Ok(DsnAuth { key, project_id })
}

fn parse_client_report(payload: &[u8]) -> Result<u64, ProtocolError> {
    let report: WireClientReport = serde_json::from_slice(payload)
        .map_err(|_| ProtocolError::invalid("invalid_client_report"))?;
    if report.discarded_events.len() > MAX_CLIENT_REPORT_ENTRIES {
        return Err(ProtocolError::too_large("client_report_too_large"));
    }
    let mut quantity = 0_u64;
    for entry in report.discarded_events {
        if entry.reason.len() > MAX_CLIENT_REPORT_TEXT_BYTES
            || entry.category.len() > MAX_CLIENT_REPORT_TEXT_BYTES
        {
            return Err(ProtocolError::too_large("client_report_field_too_large"));
        }
        quantity = quantity.saturating_add(entry.quantity);
    }
    Ok(quantity)
}

enum ItemClass {
    Event,
    Signal(RawSignalKind),
    ClientReport,
    Attachment,
    Disabled(DisabledCategory),
    Unknown,
}

fn classify_item(kind: &str) -> ItemClass {
    match kind {
        "event" | "feedback" => ItemClass::Event,
        "client_report" => ItemClass::ClientReport,
        "log" => ItemClass::Signal(RawSignalKind::Log),
        "transaction" => ItemClass::Signal(RawSignalKind::Transaction),
        "session" => ItemClass::Signal(RawSignalKind::Session),
        "sessions" => ItemClass::Disabled(DisabledCategory::Session),
        "profile" | "profile_chunk" => ItemClass::Disabled(DisabledCategory::Profile),
        "replay_event" | "replay_recording" => ItemClass::Disabled(DisabledCategory::Replay),
        "check_in" => ItemClass::Disabled(DisabledCategory::CheckIn),
        "span" => ItemClass::Signal(RawSignalKind::Span),
        "statsd" | "metric_buckets" => ItemClass::Disabled(DisabledCategory::Statsd),
        "attachment" => ItemClass::Attachment,
        "view_hierarchy" => ItemClass::Disabled(DisabledCategory::Attachment),
        "form_data" | "user_report" | "security" => {
            ItemClass::Disabled(DisabledCategory::OtherKnown)
        }
        _ => ItemClass::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = r#"{"event_id":"0123456789abcdef0123456789abcdef","message":"boom"}"#;

    fn envelope(item_header: &str, payload: &str) -> Vec<u8> {
        format!("{{}}\n{item_header}\n{payload}").into_bytes()
    }

    #[test]
    fn parses_length_delimited_error_event() {
        let body = envelope(
            &format!(r#"{{"type":"event","length":{}}}"#, EVENT.len()),
            EVENT,
        );
        let parsed = parse_envelope(
            &body,
            EnvelopeLimits {
                max_items: 100,
                max_event_bytes: 1024,
            },
        )
        .unwrap();
        assert_eq!(parsed.primary.unwrap().bytes.as_ref(), EVENT.as_bytes());
    }

    #[test]
    fn current_feedback_item_is_the_primary_record() {
        let payload = r#"{"event_id":"0123456789abcdef0123456789abcdef","type":"feedback","contexts":{"feedback":{"message":"Checkout failed"}}}"#;
        let parsed = parse_envelope(
            &envelope(r#"{"type":"feedback"}"#, payload),
            EnvelopeLimits {
                max_items: 100,
                max_event_bytes: 1024,
            },
        )
        .unwrap();
        assert_eq!(parsed.primary.unwrap().bytes.as_ref(), payload.as_bytes());
        assert!(parsed.discarded.is_empty());
    }

    #[test]
    fn structured_logs_transactions_spans_and_sessions_are_signal_items() {
        for (kind, expected) in [
            ("log", RawSignalKind::Log),
            ("transaction", RawSignalKind::Transaction),
            ("span", RawSignalKind::Span),
            ("session", RawSignalKind::Session),
        ] {
            let payload = if kind == "log" {
                r#"{"version":2,"items":[{"timestamp":1,"level":"info","body":"ready"}]}"#
            } else if kind == "session" {
                r#"{"sid":"01234567-89ab-cdef-0123-456789abcdef","started":"2026-01-01T00:00:00Z","status":"ok","attrs":{"release":"backend@1"}}"#
            } else {
                r#"{"trace_id":"0123456789abcdef0123456789abcdef","span_id":"0123456789abcdef"}"#
            };
            let body = envelope(&format!(r#"{{"type":"{kind}"}}"#), payload);
            let parsed = parse_envelope(
                &body,
                EnvelopeLimits {
                    max_items: 10,
                    max_event_bytes: 1024,
                },
            )
            .unwrap();
            assert_eq!(parsed.signals.len(), 1);
            assert_eq!(parsed.signals[0].kind, expected);
            assert!(parsed.discarded.is_empty());
        }
    }

    #[test]
    fn parses_hyphenated_event_id_from_official_rust_sdk_envelope() {
        let event_id = "01234567-89ab-cdef-0123-456789abcdef";
        let body = format!(
            "{{\"event_id\":\"{event_id}\"}}\n{{\"type\":\"event\",\"length\":{}}}\n{EVENT}",
            EVENT.len()
        );
        let parsed = parse_envelope(
            body.as_bytes(),
            EnvelopeLimits {
                max_items: 1,
                max_event_bytes: 1024,
            },
        )
        .unwrap();

        assert_eq!(
            parsed.event_id.unwrap(),
            EventId::parse("0123456789abcdef0123456789abcdef").unwrap()
        );
    }

    #[test]
    fn rejects_noncanonical_hyphenated_event_id() {
        let body = format!(
            "{{\"event_id\":\"0123456-789ab-cdef-0123-456789abcdef0\"}}\n\
             {{\"type\":\"event\",\"length\":{}}}\n{EVENT}",
            EVENT.len()
        );
        assert_eq!(
            parse_envelope(
                body.as_bytes(),
                EnvelopeLimits {
                    max_items: 1,
                    max_event_bytes: 1024,
                },
            )
            .unwrap_err()
            .code(),
            "invalid_event_id"
        );
    }

    #[test]
    fn attachment_parser_preserves_bounded_metadata_and_enforces_aggregate_limit() {
        let attachment = r#"{"safe":true}"#;
        let body = format!(
            "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{}\n{{\"type\":\"attachment\",\"length\":{},\"filename\":\"context.json\",\"content_type\":\"application/json\"}}\n{}",
            EVENT.len(),
            EVENT,
            attachment.len(),
            attachment
        );
        let parsed = parse_envelope_with_attachments(
            body.as_bytes(),
            EnvelopeLimits {
                max_items: 10,
                max_event_bytes: 1024,
            },
            AttachmentLimits {
                max_count: 1,
                max_item_bytes: 1024,
                max_total_bytes: 1024,
            },
        )
        .unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename.as_ref(), "context.json");

        let error = parse_envelope_with_attachments(
            body.as_bytes(),
            EnvelopeLimits {
                max_items: 10,
                max_event_bytes: 1024,
            },
            AttachmentLimits {
                max_count: 1,
                max_item_bytes: 4,
                max_total_bytes: 4,
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::TooLarge);
        assert_eq!(error.code(), "attachment_too_large");
    }

    #[test]
    fn parses_official_lengthless_error_event() {
        let body = envelope(
            r#"{"type":"event","content_type":"application/json"}"#,
            EVENT,
        );
        let parsed = parse_envelope(
            &body,
            EnvelopeLimits {
                max_items: 100,
                max_event_bytes: 1024,
            },
        )
        .unwrap();
        assert_eq!(parsed.primary.unwrap().bytes.as_ref(), EVENT.as_bytes());
    }

    #[test]
    fn lengthless_items_remain_bounded_and_newline_framed() {
        let body = format!(
            "{{}}\n{{\"type\":\"event\"}}\n{EVENT}\n{{\"type\":\"client_report\"}}\n\
             {{\"discarded_events\":[{{\"reason\":\"queue_overflow\",\"category\":\"error\",\"quantity\":2}}]}}"
        );
        let parsed = parse_envelope(
            body.as_bytes(),
            EnvelopeLimits {
                max_items: 2,
                max_event_bytes: 1024,
            },
        )
        .unwrap();
        assert!(parsed.primary.is_some());
        assert_eq!(parsed.client_report_quantity, 2);

        let oversized = envelope(r#"{"type":"event"}"#, &"x".repeat(17));
        assert_eq!(
            parse_envelope(
                &oversized,
                EnvelopeLimits {
                    max_items: 1,
                    max_event_bytes: 16,
                },
            )
            .unwrap_err()
            .code(),
            "lengthless_item_too_large"
        );
    }

    #[test]
    fn declared_length_is_authoritative() {
        let body = envelope(r#"{"type":"event","length":999}"#, EVENT);
        assert_eq!(
            parse_envelope(
                &body,
                EnvelopeLimits {
                    max_items: 100,
                    max_event_bytes: 1024,
                },
            )
            .unwrap_err()
            .code(),
            "truncated_item"
        );
    }

    #[test]
    fn declared_length_property_never_reads_past_the_available_payload() {
        for declared in 0..=(EVENT.len() + 16) {
            let body = envelope(&format!(r#"{{"type":"event","length":{declared}}}"#), EVENT);
            let result = parse_envelope(
                &body,
                EnvelopeLimits {
                    max_items: 1,
                    max_event_bytes: EVENT.len() + 16,
                },
            );
            if declared == EVENT.len() {
                assert_eq!(result.unwrap().primary.unwrap().bytes.len(), declared);
            } else {
                assert!(
                    result.is_err(),
                    "declared length {declared} must fail closed"
                );
            }
        }
    }

    #[test]
    fn mixed_signal_item_does_not_remove_error() {
        let body = format!(
            "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{}\n{{\"type\":\"transaction\",\"length\":2}}\n{{}}",
            EVENT.len(),
            EVENT
        );
        let parsed = parse_envelope(
            body.as_bytes(),
            EnvelopeLimits {
                max_items: 100,
                max_event_bytes: 1024,
            },
        )
        .unwrap();
        assert!(parsed.primary.is_some());
        assert!(parsed.discarded.is_empty());
        assert_eq!(parsed.signals.len(), 1);
        assert_eq!(parsed.signals[0].kind, RawSignalKind::Transaction);
    }

    #[test]
    fn client_report_quantity_is_bounded_and_saturating() {
        let payload =
            r#"{"discarded_events":[{"reason":"queue_overflow","category":"error","quantity":7}]}"#;
        let body = envelope(
            &format!(r#"{{"type":"client_report","length":{}}}"#, payload.len()),
            payload,
        );
        let parsed = parse_envelope(
            &body,
            EnvelopeLimits {
                max_items: 1,
                max_event_bytes: 1024,
            },
        )
        .unwrap();
        assert_eq!(parsed.client_report_quantity, 7);
        assert!(parsed.primary.is_none());
    }

    #[test]
    fn parses_supported_auth_forms() {
        let key = parse_x_sentry_auth(
            "Sentry sentry_version=7, sentry_client=test/1, sentry_key=0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        assert_eq!(key.to_string(), "0123456789abcdef0123456789abcdef");
        assert_eq!(
            parse_query_auth("sentry_version=7&sentry_key=0123456789abcdef0123456789abcdef")
                .unwrap(),
            Some(key)
        );
    }

    #[test]
    fn parses_dsn_from_envelope_header() {
        let body = format!(
            "{{\"dsn\":\"https://0123456789abcdef0123456789abcdef@example.invalid/42\"}}\n{{\"type\":\"event\",\"length\":{}}}\n{}",
            EVENT.len(),
            EVENT
        );
        let parsed = parse_envelope(
            body.as_bytes(),
            EnvelopeLimits {
                max_items: 100,
                max_event_bytes: 1024,
            },
        )
        .unwrap();
        assert_eq!(parsed.dsn.unwrap().project_id.get(), 42);
    }

    #[test]
    fn fuzz_regression_missing_separator_is_rejected() {
        let body = format!(
            "{{}}\n{{\"type\":\"transaction\",\"length\":2}}\n{{}}{{\"type\":\"event\",\"length\":{}}}\n{}",
            EVENT.len(),
            EVENT
        );
        assert!(
            parse_envelope(
                body.as_bytes(),
                EnvelopeLimits {
                    max_items: 100,
                    max_event_bytes: 1024,
                },
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "performance baseline runs in release mode"]
    fn performance_envelope_parser_rps() {
        let body = envelope(
            &format!(r#"{{"type":"event","length":{}}}"#, EVENT.len()),
            EVENT,
        );
        let iterations = 100_000_u64;
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(
                parse_envelope(
                    &body,
                    EnvelopeLimits {
                        max_items: 100,
                        max_event_bytes: 1024,
                    },
                )
                .unwrap(),
            );
        }
        let rps = iterations as f64 / started.elapsed().as_secs_f64();
        eprintln!("envelope parser: {rps:.0} requests/s");
        assert!(
            rps >= 20_000.0,
            "parser baseline {rps:.0} RPS is below gate"
        );
    }
}
