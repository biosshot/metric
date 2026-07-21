use std::{collections::BTreeSet, sync::Arc};

use faultkeep_domain::{
    AcceptedEvent, DsnKey, EventId, IpScrubPolicy, ProjectId, ProjectSnapshot, ScrubbedEventPayload,
};
use faultkeep_ports::{
    Clock, DurableOutcome, EventSink, EventSinkError, IngestOutcome, IngestOutcomeKind,
    OutcomeSink, ProjectResolveError, ProjectResolver, RandomSource,
};
use hmac::{Hmac, Mac};
use serde_json::{Map, Value};
use sha2::Sha256;
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::shutdown::ShutdownSignal;

const MAX_AUTH_SOURCES: usize = 4;
const MAX_SCRUB_DEPTH: usize = 64;
const FILTERED: &str = "[Filtered]";

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

#[derive(Debug, Clone, Copy)]
pub struct DiscardedItem {
    pub category: Option<DisabledCategory>,
    pub reason: &'static str,
}

#[derive(Clone)]
pub struct PrimaryEvent {
    pub header_event_id: Option<EventId>,
    pub raw_json: Box<[u8]>,
}

impl std::fmt::Debug for PrimaryEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PrimaryEvent")
            .field("header_event_id", &self.header_event_id)
            .field("bytes", &self.raw_json.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct IngestRequest {
    pub path_project_id: ProjectId,
    pub auth_keys: Vec<DsnKey>,
    pub dsn_project_id: Option<ProjectId>,
    pub envelope_event_id: Option<EventId>,
    pub primary: Option<PrimaryEvent>,
    pub discarded: Vec<DiscardedItem>,
    pub client_report_quantity: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestResult {
    pub event_id: Option<EventId>,
    pub durable: Option<DurableOutcome>,
    pub disabled_categories: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestErrorKind {
    Invalid,
    Unauthorized,
    TooLarge,
    RateLimited,
    Unavailable,
    Timeout,
    ShuttingDown,
    ScrubFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("ingest request failed")]
pub struct IngestError {
    kind: IngestErrorKind,
    code: &'static str,
}

impl IngestError {
    #[must_use]
    pub const fn kind(self) -> IngestErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }

    pub const fn invalid(code: &'static str) -> Self {
        Self {
            kind: IngestErrorKind::Invalid,
            code,
        }
    }

    pub const fn unavailable(code: &'static str) -> Self {
        Self {
            kind: IngestErrorKind::Unavailable,
            code,
        }
    }

    pub const fn rate_limited(code: &'static str) -> Self {
        Self {
            kind: IngestErrorKind::RateLimited,
            code,
        }
    }
}

pub struct IngestService {
    resolver: Arc<dyn ProjectResolver>,
    event_sink: Arc<dyn EventSink>,
    outcome_sink: Arc<dyn OutcomeSink>,
    clock: Arc<dyn Clock>,
    _random: Arc<dyn RandomSource>,
    storage_permits: Arc<Semaphore>,
    shutdown: ShutdownSignal,
}

impl IngestService {
    #[must_use]
    pub fn new(
        resolver: Arc<dyn ProjectResolver>,
        event_sink: Arc<dyn EventSink>,
        outcome_sink: Arc<dyn OutcomeSink>,
        clock: Arc<dyn Clock>,
        random: Arc<dyn RandomSource>,
        max_waiting_for_storage: usize,
        shutdown: ShutdownSignal,
    ) -> Self {
        Self {
            resolver,
            event_sink,
            outcome_sink,
            clock,
            _random: random,
            storage_permits: Arc::new(Semaphore::new(max_waiting_for_storage)),
            shutdown,
        }
    }

    pub async fn ingest(&self, request: IngestRequest) -> Result<IngestResult, IngestError> {
        if self.shutdown.is_cancelled() {
            return Err(IngestError {
                kind: IngestErrorKind::ShuttingDown,
                code: "shutting_down",
            });
        }
        let key = one_auth_key(&request.auth_keys)?;
        let snapshot = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return Err(IngestError {
                kind: IngestErrorKind::ShuttingDown,
                code: "shutting_down",
            }),
            resolved = self.resolver.resolve(key) => resolved.map_err(map_resolve_error)?,
        };
        validate_project_consistency(&request, &snapshot)?;

        for item in &request.discarded {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Unsupported,
                reason: item.reason,
                quantity: 1,
            });
        }
        if request.client_report_quantity > 0 {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Unsupported,
                reason: "sdk_client_report",
                quantity: request.client_report_quantity,
            });
        }
        let disabled_categories = request
            .discarded
            .iter()
            .filter_map(|item| item.category)
            .map(DisabledCategory::sentry_name)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        let Some(primary) = request.primary else {
            return Ok(IngestResult {
                event_id: None,
                durable: None,
                disabled_categories,
            });
        };
        if !snapshot.items.error {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Unsupported,
                reason: "feature_disabled",
                quantity: 1,
            });
            return Ok(IngestResult {
                event_id: None,
                durable: None,
                disabled_categories,
            });
        }

        let (event_id, payload) =
            validate_and_scrub_event(primary, request.envelope_event_id, &snapshot)?;
        let accepted = AcceptedEvent {
            project_id: snapshot.project_id,
            event_id,
            received_at: self.clock.now(),
            policy_revision: snapshot.scrub_policy.revision,
            payload: ScrubbedEventPayload::new(payload),
        };
        let _permit = self
            .storage_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| IngestError::unavailable("storage_wait_capacity"))?;
        let durable = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return Err(IngestError {
                kind: IngestErrorKind::ShuttingDown,
                code: "shutting_down",
            }),
            result = self.event_sink.persist(accepted) => result.map_err(map_sink_error)?,
        };
        self.outcome_sink.record(IngestOutcome {
            kind: match durable {
                DurableOutcome::Accepted => IngestOutcomeKind::Accepted,
                DurableOutcome::Duplicate => IngestOutcomeKind::Duplicate,
            },
            reason: "event",
            quantity: 1,
        });
        Ok(IngestResult {
            event_id: Some(event_id),
            durable: Some(durable),
            disabled_categories,
        })
    }

    pub fn record_outcome(&self, outcome: IngestOutcome) {
        self.outcome_sink.record(outcome);
    }
}

fn one_auth_key(keys: &[DsnKey]) -> Result<DsnKey, IngestError> {
    if keys.is_empty() || keys.len() > MAX_AUTH_SOURCES {
        return Err(IngestError {
            kind: IngestErrorKind::Unauthorized,
            code: "unauthorized",
        });
    }
    let first = keys[0];
    if keys.iter().any(|key| *key != first) {
        return Err(IngestError::invalid("conflicting_auth"));
    }
    Ok(first)
}

fn validate_project_consistency(
    request: &IngestRequest,
    snapshot: &ProjectSnapshot,
) -> Result<(), IngestError> {
    if snapshot.project_id != request.path_project_id
        || request
            .dsn_project_id
            .is_some_and(|project| project != snapshot.project_id)
    {
        return Err(IngestError {
            kind: IngestErrorKind::Unauthorized,
            code: "unauthorized",
        });
    }
    Ok(())
}

fn validate_and_scrub_event(
    primary: PrimaryEvent,
    envelope_event_id: Option<EventId>,
    snapshot: &ProjectSnapshot,
) -> Result<(EventId, Vec<u8>), IngestError> {
    let mut value: Value = serde_json::from_slice(&primary.raw_json)
        .map_err(|_| IngestError::invalid("invalid_event_json"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| IngestError::invalid("event_not_object"))?;
    if object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "transaction")
    {
        return Err(IngestError::invalid("event_type_not_error"));
    }
    let body_event_id = object
        .get("event_id")
        .and_then(Value::as_str)
        .ok_or_else(|| IngestError::invalid("missing_event_id"))
        .and_then(|value| {
            EventId::parse(value).map_err(|_| IngestError::invalid("invalid_event_id"))
        })?;
    for stated in [primary.header_event_id, envelope_event_id]
        .into_iter()
        .flatten()
    {
        if stated != body_event_id {
            return Err(IngestError::invalid("conflicting_event_id"));
        }
    }
    object.insert(
        "event_id".to_owned(),
        Value::String(body_event_id.to_string()),
    );
    object.remove("project");
    scrub_value(&mut value, None, &snapshot.scrub_policy, 0)?;
    let payload = serde_json::to_vec(&value).map_err(|_| IngestError {
        kind: IngestErrorKind::ScrubFailed,
        code: "scrub_failed",
    })?;
    Ok((body_event_id, payload))
}

fn scrub_value(
    value: &mut Value,
    field: Option<&str>,
    policy: &faultkeep_domain::ScrubPolicy,
    depth: usize,
) -> Result<(), IngestError> {
    if depth > MAX_SCRUB_DEPTH {
        return Err(IngestError {
            kind: IngestErrorKind::ScrubFailed,
            code: "scrub_depth_exceeded",
        });
    }
    if field.is_some_and(is_sensitive_field) {
        *value = Value::String(FILTERED.to_owned());
        return Ok(());
    }
    if field.is_some_and(is_ip_field) {
        scrub_ip(value, policy)?;
        return Ok(());
    }
    match value {
        Value::Object(object) => scrub_object(object, policy, depth + 1)?,
        Value::Array(values) => {
            for value in values {
                scrub_value(value, None, policy, depth + 1)?;
            }
        }
        Value::String(text) => scrub_string(text),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

fn scrub_object(
    object: &mut Map<String, Value>,
    policy: &faultkeep_domain::ScrubPolicy,
    depth: usize,
) -> Result<(), IngestError> {
    for (field, value) in object {
        scrub_value(value, Some(field), policy, depth)?;
    }
    Ok(())
}

fn scrub_string(text: &mut String) {
    let lowercase = text.to_ascii_lowercase();
    if lowercase.starts_with("bearer ")
        || text.contains("-----BEGIN PRIVATE KEY-----")
        || text.contains("-----BEGIN RSA PRIVATE KEY-----")
    {
        *text = FILTERED.to_owned();
        return;
    }
    if let Ok(url) = url::Url::parse(text) {
        if !url.username().is_empty() || url.password().is_some() {
            *text = "[Filtered URL Credentials]".to_owned();
        }
    }
}

fn scrub_ip(value: &mut Value, policy: &faultkeep_domain::ScrubPolicy) -> Result<(), IngestError> {
    match policy.ip_policy {
        IpScrubPolicy::Keep => {}
        IpScrubPolicy::Remove => *value = Value::Null,
        IpScrubPolicy::Truncate => {
            if let Some(ip) = value.as_str() {
                *value = Value::String(truncate_ip(ip));
            }
        }
        IpScrubPolicy::Hmac => {
            if let Some(ip) = value.as_str() {
                let mut mac =
                    Hmac::<Sha256>::new_from_slice(policy.hmac_key.expose()).map_err(|_| {
                        IngestError {
                            kind: IngestErrorKind::ScrubFailed,
                            code: "scrub_failed",
                        }
                    })?;
                mac.update(ip.as_bytes());
                *value = Value::String(format!(
                    "hmac:v1:{}",
                    hex::encode(mac.finalize().into_bytes())
                ));
            }
        }
    }
    Ok(())
}

fn truncate_ip(ip: &str) -> String {
    if let Ok(address) = ip.parse::<std::net::IpAddr>() {
        match address {
            std::net::IpAddr::V4(address) => {
                let octets = address.octets();
                std::net::Ipv4Addr::new(octets[0], octets[1], octets[2], 0).to_string()
            }
            std::net::IpAddr::V6(mut address) => {
                let mut segments = address.segments();
                segments[4..].fill(0);
                address = segments.into();
                address.to_string()
            }
        }
    } else {
        FILTERED.to_owned()
    }
}

fn is_sensitive_field(field: &str) -> bool {
    matches!(
        normalize_field(field).as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "password"
            | "passwd"
            | "secret"
            | "accesstoken"
            | "refreshtoken"
            | "apikey"
            | "privatekey"
    )
}

fn is_ip_field(field: &str) -> bool {
    matches!(
        normalize_field(field).as_str(),
        "ip" | "ipaddress" | "remoteaddr"
    )
}

fn normalize_field(field: &str) -> String {
    field
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn map_resolve_error(error: ProjectResolveError) -> IngestError {
    match error {
        ProjectResolveError::Unauthorized => IngestError {
            kind: IngestErrorKind::Unauthorized,
            code: "unauthorized",
        },
        ProjectResolveError::Unavailable => IngestError::unavailable("project_unavailable"),
    }
}

fn map_sink_error(error: EventSinkError) -> IngestError {
    match error {
        EventSinkError::Unavailable => IngestError::unavailable("storage_unavailable"),
        EventSinkError::Ambiguous => IngestError::unavailable("ambiguous_durable_ack"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faultkeep_domain::{ItemCapabilities, ScrubPolicy, SecretBytes};

    fn snapshot() -> ProjectSnapshot {
        ProjectSnapshot {
            project_id: ProjectId::new(42).unwrap(),
            scrub_policy: ScrubPolicy {
                revision: 7,
                ip_policy: IpScrubPolicy::Hmac,
                hmac_key: SecretBytes::new([7; 32]),
            },
            items: ItemCapabilities {
                error: true,
                client_report: true,
            },
        }
    }

    #[test]
    fn mandatory_floor_scrubs_unknown_nested_credentials() {
        let raw = br#"{"event_id":"0123456789abcdef0123456789abcdef","unknown":{"password":"open-sesame","header":"Bearer token","url":"https://user:pass@example.invalid/a"},"user":{"ip_address":"192.0.2.1"}}"#;
        let (_, scrubbed) = validate_and_scrub_event(
            PrimaryEvent {
                header_event_id: None,
                raw_json: raw.as_slice().into(),
            },
            None,
            &snapshot(),
        )
        .unwrap();
        let text = String::from_utf8(scrubbed).unwrap();
        assert!(!text.contains("open-sesame"));
        assert!(!text.contains("Bearer token"));
        assert!(!text.contains("user:pass"));
        assert!(!text.contains("192.0.2.1"));
        assert!(text.contains("hmac:v1:"));
    }

    #[test]
    fn conflicting_event_ids_fail_closed() {
        let event_id = EventId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let raw = br#"{"event_id":"0123456789abcdef0123456789abcdef"}"#;
        assert_eq!(
            validate_and_scrub_event(
                PrimaryEvent {
                    header_event_id: Some(event_id),
                    raw_json: raw.as_slice().into(),
                },
                None,
                &snapshot(),
            )
            .unwrap_err()
            .code(),
            "conflicting_event_id"
        );
    }

    #[test]
    #[ignore = "performance baseline runs in release mode"]
    fn performance_event_validation_and_scrub_rps() {
        let raw = br#"{"event_id":"0123456789abcdef0123456789abcdef","message":"synthetic","request":{"url":"https://user:password@example.invalid/path","headers":{"authorization":"Bearer secret"}},"user":{"ip_address":"192.0.2.10"},"extra":{"padding":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}}"#;
        let iterations = 20_000_u64;
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(
                validate_and_scrub_event(
                    PrimaryEvent {
                        header_event_id: None,
                        raw_json: raw.as_slice().into(),
                    },
                    None,
                    &snapshot(),
                )
                .unwrap(),
            );
        }
        let rps = iterations as f64 / started.elapsed().as_secs_f64();
        eprintln!("event validation + scrub: {rps:.0} requests/s");
        assert!(rps >= 20_000.0, "scrub baseline {rps:.0} RPS is below gate");
    }
}
