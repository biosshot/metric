use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
};

use hmac::{Hmac, Mac};
use metric_domain::{
    AcceptedEvent, DsnKey, EventId, IpScrubPolicy, ProjectAcceptanceState, ProjectId,
    ProjectKeyState, ProjectSnapshot, ScrubbedEventPayload,
    blob::{
        AttachmentFilename, BlobChecksum, BlobContentType, BlobKey, BlobKind, BlobObjectId,
        EventAttachment,
    },
    feedback::{FeedbackRecord, FeedbackStatus},
    finalization::{derive_environment_id, derive_release_id},
    inbound_filter::{
        InboundFilterField, InboundFilterFields, InboundFilterMatch, InboundFilterSignal,
    },
    monitors::{
        MonitorConfig, MonitorDefinition, MonitorId, MonitorRun, MonitorRunId, MonitorRunSource,
        MonitorRunStatus, MonitorSchedule, MonitorUpdate,
    },
    releases::validate_version,
    sessions::{SessionId, SessionState, SessionUpdate},
    signals::{
        LogId, LogRecord, LogSeverity, SignalBody, SpanId, SpanOperationClass, SpanRecord,
        SpanRecordId, TraceId,
    },
};
use metric_ports::{
    BlobChunkSource, BlobStore, BlobStoreError, Clock, DurableOutcome, EventSink, EventSinkError,
    FeedbackSink, FeedbackStoreError, IngestOutcome, IngestOutcomeKind, LogSink, MonitorSink,
    OutcomeSink, ProjectResolveError, ProjectResolver, RandomSource, SessionSink, SignalStoreError,
    SpanSink,
};
use serde_json::{Map, Value};
use sha2::Sha256;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Semaphore;

use crate::{observability::Metrics, shutdown::ShutdownSignal};

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

enum ValidatedPrimaryEvent {
    Accepted {
        event_id: EventId,
        payload: Vec<u8>,
    },
    Filtered {
        event_id: EventId,
        matched: InboundFilterMatch,
    },
}

impl std::fmt::Debug for ValidatedPrimaryEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accepted { event_id, payload } => formatter
                .debug_struct("Accepted")
                .field("event_id", event_id)
                .field("payload_bytes", &payload.len())
                .finish(),
            Self::Filtered { event_id, matched } => formatter
                .debug_struct("Filtered")
                .field("event_id", event_id)
                .field("signal", &matched.signal)
                .field("field", &matched.field)
                .finish(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingAttachment {
    pub position: u32,
    pub filename: Box<str>,
    pub content_type: Box<str>,
    pub attachment_type: Box<str>,
    pub bytes: Box<[u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingSignalKind {
    Log,
    Transaction,
    Span,
    Session,
}

#[derive(Debug, Clone)]
pub struct PendingSignal {
    pub kind: PendingSignalKind,
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
    /// Error dependency root for attachments; this is an Envelope role.
    pub primary: Option<PrimaryEvent>,
    /// Independent Log/Transaction/Span items, not an exhaustive signal taxonomy.
    pub signals: Vec<PendingSignal>,
    pub check_ins: Vec<Box<[u8]>>,
    pub attachments: Vec<PendingAttachment>,
    pub discarded: Vec<DiscardedItem>,
    pub client_report_quantity: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct AttachmentIngestConfig {
    pub enabled: bool,
    pub chunk_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct MinidumpIngestConfig {
    pub enabled: bool,
    pub max_bytes: u64,
    pub chunk_bytes: usize,
    pub retained_header_bytes: usize,
}

impl Default for MinidumpIngestConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_bytes: 100 * 1024 * 1024,
            chunk_bytes: 64 * 1024,
            retained_header_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MinidumpRequest {
    pub path_project_id: ProjectId,
    pub auth_keys: Vec<DsnKey>,
    pub dsn_project_id: Option<ProjectId>,
    pub supplied_event_id: Option<EventId>,
}

impl Default for AttachmentIngestConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            chunk_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FeedbackIngestConfig {
    pub retention_days: u32,
    pub max_message_bytes: usize,
    pub max_name_bytes: usize,
    pub max_contact_bytes: usize,
    pub max_url_bytes: usize,
    pub max_attachments: usize,
    pub max_attachment_bytes: usize,
    pub max_total_attachment_bytes: usize,
    pub max_submissions_per_minute: u32,
    pub limiter_capacity: usize,
    pub allow_png_screenshots: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct MonitorIngestConfig {
    pub retention_days: u32,
    pub max_check_ins_per_minute: u32,
    pub limiter_capacity: usize,
}

impl Default for MonitorIngestConfig {
    fn default() -> Self {
        Self {
            retention_days: 90,
            max_check_ins_per_minute: 10_000,
            limiter_capacity: 10_000,
        }
    }
}

impl Default for FeedbackIngestConfig {
    fn default() -> Self {
        Self {
            retention_days: 90,
            max_message_bytes: 4 * 1024,
            max_name_bytes: 256,
            max_contact_bytes: 320,
            max_url_bytes: 2 * 1024,
            max_attachments: 3,
            max_attachment_bytes: 2 * 1024 * 1024,
            max_total_attachment_bytes: 5 * 1024 * 1024,
            max_submissions_per_minute: 30,
            limiter_capacity: 10_000,
            allow_png_screenshots: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FeedbackRateWindow {
    opened_at: i64,
    submissions: u32,
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
    blob_store: Option<Arc<dyn BlobStore>>,
    attachment_config: AttachmentIngestConfig,
    minidump_config: MinidumpIngestConfig,
    log_sink: Option<Arc<dyn LogSink>>,
    span_sink: Option<Arc<dyn SpanSink>>,
    session_sink: Option<Arc<dyn SessionSink>>,
    feedback_sink: Option<Arc<dyn FeedbackSink>>,
    feedback_config: FeedbackIngestConfig,
    feedback_rate: Mutex<HashMap<ProjectId, FeedbackRateWindow>>,
    monitor_sink: Option<Arc<dyn MonitorSink>>,
    monitor_config: MonitorIngestConfig,
    monitor_rate: Mutex<HashMap<ProjectId, FeedbackRateWindow>>,
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
            blob_store: None,
            attachment_config: AttachmentIngestConfig::default(),
            minidump_config: MinidumpIngestConfig::default(),
            log_sink: None,
            span_sink: None,
            session_sink: None,
            feedback_sink: None,
            feedback_config: FeedbackIngestConfig::default(),
            feedback_rate: Mutex::new(HashMap::new()),
            monitor_sink: None,
            monitor_config: MonitorIngestConfig::default(),
            monitor_rate: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn with_span_sink(mut self, span_sink: Arc<dyn SpanSink>) -> Self {
        self.span_sink = Some(span_sink);
        self
    }

    #[must_use]
    pub fn with_log_sink(mut self, log_sink: Arc<dyn LogSink>) -> Self {
        self.log_sink = Some(log_sink);
        self
    }

    #[must_use]
    pub fn with_session_sink(mut self, session_sink: Arc<dyn SessionSink>) -> Self {
        self.session_sink = Some(session_sink);
        self
    }

    #[must_use]
    pub fn with_feedback_sink(
        mut self,
        feedback_sink: Arc<dyn FeedbackSink>,
        config: FeedbackIngestConfig,
    ) -> Self {
        self.feedback_sink = Some(feedback_sink);
        self.feedback_config = config;
        self
    }

    #[must_use]
    pub fn with_monitor_sink(
        mut self,
        monitor_sink: Arc<dyn MonitorSink>,
        config: MonitorIngestConfig,
    ) -> Self {
        self.monitor_sink = Some(monitor_sink);
        self.monitor_config = config;
        self
    }

    #[must_use]
    pub const fn with_minidumps(mut self, config: MinidumpIngestConfig) -> Self {
        self.minidump_config = config;
        self
    }

    #[must_use]
    pub fn with_blob_store(
        mut self,
        blob_store: Arc<dyn BlobStore>,
        config: AttachmentIngestConfig,
    ) -> Self {
        self.blob_store = Some(blob_store);
        self.attachment_config = config;
        self
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
        let mut disabled_categories = request
            .discarded
            .iter()
            .filter_map(|item| item.category)
            .map(DisabledCategory::sentry_name)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        disabled_categories.extend(self.persist_signals(&snapshot, request.signals).await?);
        disabled_categories.extend(self.persist_check_ins(&snapshot, request.check_ins).await?);
        disabled_categories.sort_unstable();
        disabled_categories.dedup();

        let Some(primary) = request.primary else {
            if !request.attachments.is_empty() {
                self.outcome_sink.record(IngestOutcome {
                    kind: IngestOutcomeKind::Unsupported,
                    reason: "attachment_without_event",
                    quantity: request.attachments.len() as u64,
                });
            }
            return Ok(IngestResult {
                event_id: None,
                durable: None,
                disabled_categories,
            });
        };
        if primary.raw_json.len() > snapshot.limits.max_event_bytes.get() as usize {
            return Err(IngestError {
                kind: IngestErrorKind::TooLarge,
                code: "project_event_too_large",
            });
        }
        if primary_is_feedback(&primary)? {
            return self
                .ingest_feedback(
                    &snapshot,
                    primary,
                    request.envelope_event_id,
                    request.attachments,
                    disabled_categories,
                )
                .await;
        }
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

        let (event_id, mut payload) =
            match validate_and_scrub_event(primary, request.envelope_event_id, &snapshot)? {
                ValidatedPrimaryEvent::Accepted { event_id, payload } => (event_id, payload),
                ValidatedPrimaryEvent::Filtered { event_id, matched } => {
                    self.record_filtered(matched);
                    return Ok(IngestResult {
                        event_id: Some(event_id),
                        durable: None,
                        disabled_categories,
                    });
                }
            };
        let attachments = self
            .persist_attachments(
                snapshot.project_id,
                event_id,
                &snapshot,
                request.attachments,
                false,
            )
            .await?;
        if attachments.dropped > 0 {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Unsupported,
                reason: "attachment_policy_unsupported",
                quantity: attachments.dropped,
            });
            disabled_categories.push(DisabledCategory::Attachment.sentry_name());
            disabled_categories.sort_unstable();
            disabled_categories.dedup();
        }
        if !attachments.accepted.is_empty() {
            append_attachment_metadata(&mut payload, &attachments.accepted)?;
        }
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

    async fn ingest_feedback(
        &self,
        snapshot: &ProjectSnapshot,
        primary: PrimaryEvent,
        envelope_event_id: Option<EventId>,
        attachments: Vec<PendingAttachment>,
        mut disabled_categories: Vec<&'static str>,
    ) -> Result<IngestResult, IngestError> {
        if !snapshot.items.feedback {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Unsupported,
                reason: "feedback_disabled",
                quantity: 1,
            });
            return Ok(IngestResult {
                event_id: None,
                durable: None,
                disabled_categories,
            });
        }
        validate_feedback_attachment_limits(&attachments, self.feedback_config)?;
        let received_at = self.clock.now();
        let mut feedback = normalize_feedback(
            snapshot,
            primary,
            envelope_event_id,
            received_at,
            self.feedback_config,
        )?;
        self.admit_feedback(snapshot.project_id, received_at)?;
        let persisted = self
            .persist_attachments(
                snapshot.project_id,
                feedback.feedback_id,
                snapshot,
                attachments,
                self.feedback_config.allow_png_screenshots,
            )
            .await?;
        if persisted.dropped > 0 {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Unsupported,
                reason: "feedback_attachment_policy_unsupported",
                quantity: persisted.dropped,
            });
            disabled_categories.push(DisabledCategory::Attachment.sentry_name());
            disabled_categories.sort_unstable();
            disabled_categories.dedup();
        }
        feedback.attachments = persisted.accepted;
        feedback
            .validate()
            .map_err(|_| IngestError::invalid("invalid_feedback"))?;
        let sink = self
            .feedback_sink
            .as_ref()
            .ok_or_else(|| IngestError::unavailable("feedback_storage_unavailable"))?;
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
            result = sink.persist_feedback(feedback.clone()) => {
                result.map_err(map_feedback_store_error)?
            },
        };
        self.outcome_sink.record(IngestOutcome {
            kind: match durable {
                DurableOutcome::Accepted => IngestOutcomeKind::Accepted,
                DurableOutcome::Duplicate => IngestOutcomeKind::Duplicate,
            },
            reason: "feedback",
            quantity: 1,
        });
        Ok(IngestResult {
            event_id: Some(feedback.feedback_id),
            durable: Some(durable),
            disabled_categories,
        })
    }

    fn admit_feedback(
        &self,
        project_id: ProjectId,
        received_at: metric_domain::Timestamp,
    ) -> Result<(), IngestError> {
        let mut windows = self
            .feedback_rate
            .lock()
            .map_err(|_| IngestError::unavailable("feedback_limiter_unavailable"))?;
        let now = received_at.unix_millis();
        if let Some(window) = windows.get_mut(&project_id) {
            if now.saturating_sub(window.opened_at) >= 60_000 {
                *window = FeedbackRateWindow {
                    opened_at: now,
                    submissions: 1,
                };
                return Ok(());
            }
            if window.submissions >= self.feedback_config.max_submissions_per_minute {
                return Err(IngestError::rate_limited("feedback_rate_limited"));
            }
            window.submissions = window.submissions.saturating_add(1);
            return Ok(());
        }
        if windows.len() >= self.feedback_config.limiter_capacity {
            return Err(IngestError::rate_limited("feedback_limiter_capacity"));
        }
        windows.insert(
            project_id,
            FeedbackRateWindow {
                opened_at: now,
                submissions: 1,
            },
        );
        Ok(())
    }

    fn admit_monitor(
        &self,
        project_id: ProjectId,
        received_at: metric_domain::Timestamp,
    ) -> Result<(), IngestError> {
        admit_window(
            &self.monitor_rate,
            project_id,
            received_at,
            self.monitor_config.max_check_ins_per_minute,
            self.monitor_config.limiter_capacity,
            "monitor_rate_limited",
            "monitor_limiter_capacity",
        )
    }

    async fn persist_check_ins(
        &self,
        snapshot: &ProjectSnapshot,
        check_ins: Vec<Box<[u8]>>,
    ) -> Result<Vec<&'static str>, IngestError> {
        if check_ins.is_empty() {
            return Ok(Vec::new());
        }
        if !snapshot.items.check_in {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Unsupported,
                reason: "feature_disabled",
                quantity: u64::try_from(check_ins.len()).unwrap_or(u64::MAX),
            });
            return Ok(vec!["monitor"]);
        }
        let received_at = self.clock.now();
        let mut updates = Vec::with_capacity(check_ins.len());
        for payload in check_ins {
            if payload.len() > snapshot.limits.max_event_bytes.get() as usize {
                return Err(IngestError {
                    kind: IngestErrorKind::TooLarge,
                    code: "project_check_in_too_large",
                });
            }
            self.admit_monitor(snapshot.project_id, received_at)?;
            updates.push(normalize_check_in(
                snapshot,
                received_at,
                &payload,
                self.monitor_config,
            )?);
        }
        let sink = self
            .monitor_sink
            .as_ref()
            .ok_or_else(|| IngestError::unavailable("monitor_storage_unavailable"))?;
        let outcomes = sink
            .persist_monitors(updates)
            .await
            .map_err(map_signal_store_error)?;
        let accepted = outcomes
            .iter()
            .filter(|outcome| **outcome == DurableOutcome::Accepted)
            .count();
        let duplicate = outcomes.len().saturating_sub(accepted);
        if accepted > 0 {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Accepted,
                reason: "monitor",
                quantity: accepted as u64,
            });
        }
        if duplicate > 0 {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Duplicate,
                reason: "monitor",
                quantity: duplicate as u64,
            });
        }
        Ok(Vec::new())
    }

    async fn persist_signals(
        &self,
        snapshot: &ProjectSnapshot,
        signals: Vec<PendingSignal>,
    ) -> Result<Vec<&'static str>, IngestError> {
        if signals.is_empty() {
            return Ok(Vec::new());
        }
        let received_at = self.clock.now();
        let mut logs = Vec::new();
        let mut spans = Vec::new();
        let mut sessions = Vec::new();
        let mut disabled_categories = Vec::new();
        for signal in signals {
            if signal.raw_json.len() > snapshot.limits.max_event_bytes.get() as usize {
                return Err(IngestError {
                    kind: IngestErrorKind::TooLarge,
                    code: "project_signal_too_large",
                });
            }
            match signal.kind {
                PendingSignalKind::Log if snapshot.items.log => {
                    for record in normalize_logs(snapshot, received_at, &signal.raw_json)? {
                        let mut fields = InboundFilterFields::empty(InboundFilterSignal::Log);
                        fields.release = record.release.as_deref();
                        fields.environment = record.environment.as_deref();
                        fields.service = record.service.as_deref();
                        fields.message = Some(&record.message);
                        fields.severity = Some(record.severity.as_str());
                        if let Some(matched) = snapshot.inbound_filters.matches(&fields) {
                            self.record_filtered(matched);
                        } else {
                            logs.push(record);
                        }
                    }
                }
                PendingSignalKind::Transaction if snapshot.items.transaction => {
                    for record in normalize_transaction(snapshot, received_at, &signal.raw_json)? {
                        let signal = if record.is_segment {
                            InboundFilterSignal::Transaction
                        } else {
                            InboundFilterSignal::Span
                        };
                        if let Some(matched) = snapshot
                            .inbound_filters
                            .matches(&span_filter_fields(&record, signal))
                        {
                            self.record_filtered(matched);
                        } else {
                            spans.push(record);
                        }
                    }
                }
                PendingSignalKind::Span if snapshot.items.span => {
                    for record in normalize_spans(snapshot, received_at, &signal.raw_json)? {
                        if let Some(matched) = snapshot
                            .inbound_filters
                            .matches(&span_filter_fields(&record, InboundFilterSignal::Span))
                        {
                            self.record_filtered(matched);
                        } else {
                            spans.push(record);
                        }
                    }
                }
                PendingSignalKind::Session => {
                    sessions.push(normalize_session(snapshot, received_at, &signal.raw_json)?);
                }
                PendingSignalKind::Log
                | PendingSignalKind::Transaction
                | PendingSignalKind::Span => {
                    disabled_categories.push(match signal.kind {
                        PendingSignalKind::Log => "log",
                        PendingSignalKind::Transaction => "transaction",
                        PendingSignalKind::Span => "span",
                        PendingSignalKind::Session => "session",
                    });
                    self.outcome_sink.record(IngestOutcome {
                        kind: IngestOutcomeKind::Unsupported,
                        reason: "feature_disabled",
                        quantity: 1,
                    });
                }
            }
        }
        if logs.is_empty() && spans.is_empty() && sessions.is_empty() {
            disabled_categories.sort_unstable();
            disabled_categories.dedup();
            return Ok(disabled_categories);
        }
        if !logs.is_empty() {
            let sink = self
                .log_sink
                .as_ref()
                .ok_or_else(|| IngestError::unavailable("log_storage_unavailable"))?;
            let quantity = u64::try_from(logs.len()).unwrap_or(u64::MAX);
            sink.persist_logs(logs)
                .await
                .map_err(map_signal_store_error)?;
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Accepted,
                reason: "log",
                quantity,
            });
        }
        if !spans.is_empty() {
            let sink = self
                .span_sink
                .as_ref()
                .ok_or_else(|| IngestError::unavailable("span_storage_unavailable"))?;
            let quantity = u64::try_from(spans.len()).unwrap_or(u64::MAX);
            sink.persist_spans(spans)
                .await
                .map_err(map_span_sink_error)?;
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Accepted,
                reason: "span",
                quantity,
            });
        }
        if !sessions.is_empty() {
            let sink = self
                .session_sink
                .as_ref()
                .ok_or_else(|| IngestError::unavailable("session_storage_unavailable"))?;
            let quantity = u64::try_from(sessions.len()).unwrap_or(u64::MAX);
            sink.persist_sessions(sessions)
                .await
                .map_err(map_signal_store_error)?;
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Accepted,
                reason: "session",
                quantity,
            });
        }
        disabled_categories.sort_unstable();
        disabled_categories.dedup();
        Ok(disabled_categories)
    }

    fn record_filtered(&self, matched: InboundFilterMatch) {
        Metrics.inbound_filtered(matched.signal, matched.field);
        self.outcome_sink.record(IngestOutcome {
            kind: IngestOutcomeKind::Filtered,
            reason: "inbound_filter",
            quantity: 1,
        });
    }

    pub fn record_outcome(&self, outcome: IngestOutcome) {
        self.outcome_sink.record(outcome);
    }

    pub async fn ingest_minidump(
        &self,
        request: MinidumpRequest,
        mut source: Box<dyn BlobChunkSource>,
    ) -> Result<IngestResult, IngestError> {
        if self.shutdown.is_cancelled() {
            return Err(IngestError {
                kind: IngestErrorKind::ShuttingDown,
                code: "shutting_down",
            });
        }
        let key = one_auth_key(&request.auth_keys)?;
        let snapshot = self
            .resolver
            .resolve(key)
            .await
            .map_err(map_resolve_error)?;
        if snapshot.project_id != request.path_project_id
            || snapshot.state != ProjectAcceptanceState::Active
            || snapshot.key_state != ProjectKeyState::Active
            || request
                .dsn_project_id
                .is_some_and(|project| project != snapshot.project_id)
        {
            return Err(IngestError {
                kind: IngestErrorKind::Unauthorized,
                code: "unauthorized",
            });
        }
        if !self.minidump_config.enabled {
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Unsupported,
                reason: "minidump_disabled",
                quantity: 1,
            });
            return Ok(IngestResult {
                event_id: request.supplied_event_id,
                durable: None,
                disabled_categories: vec!["error"],
            });
        }
        if !snapshot.items.error {
            return Ok(IngestResult {
                event_id: request.supplied_event_id,
                durable: None,
                disabled_categories: vec!["error"],
            });
        }
        let store = self
            .blob_store
            .as_ref()
            .ok_or_else(|| IngestError::unavailable("blob_storage_unavailable"))?;
        let received_at = self.clock.now();
        let mut writer = store
            .begin(BlobKind::Minidump, received_at)
            .await
            .map_err(map_blob_error)?;
        let mut header = Vec::with_capacity(self.minidump_config.retained_header_bytes);
        let mut hasher = blake3::Hasher::new();
        let mut size = 0_u64;
        loop {
            let Some(chunk) = source
                .next_chunk(self.minidump_config.chunk_bytes)
                .await
                .map_err(map_blob_error)?
            else {
                break;
            };
            let length = u64::try_from(chunk.len()).map_err(|_| IngestError {
                kind: IngestErrorKind::TooLarge,
                code: "minidump_too_large",
            })?;
            size = size.checked_add(length).ok_or(IngestError {
                kind: IngestErrorKind::TooLarge,
                code: "minidump_too_large",
            })?;
            if size > self.minidump_config.max_bytes {
                writer.abort().await.map_err(map_blob_error)?;
                return Err(IngestError {
                    kind: IngestErrorKind::TooLarge,
                    code: "minidump_too_large",
                });
            }
            let retained = self
                .minidump_config
                .retained_header_bytes
                .saturating_sub(header.len())
                .min(chunk.len());
            header.extend_from_slice(&chunk[..retained]);
            hasher.update(&chunk);
            writer.write_chunk(chunk).await.map_err(map_blob_error)?;
        }
        validate_minidump_header(&header, size)?;
        let checksum = BlobChecksum::from_bytes(*hasher.finalize().as_bytes());
        let event_id = request
            .supplied_event_id
            .unwrap_or_else(|| minidump_event_id(snapshot.project_id, checksum));
        let object_id = minidump_object_id(snapshot.project_id, event_id, checksum);
        let key = BlobKey::event_owned(snapshot.project_id, event_id, object_id);
        let blob = writer.commit(key).await.map_err(map_blob_error)?;
        if blob.checksum != checksum {
            return Err(IngestError::unavailable("blob_checksum_mismatch"));
        }
        let payload = serde_json::to_vec(&serde_json::json!({
            "event_id": event_id.to_string(),
            "platform": "native",
            "level": "fatal",
            "timestamp": received_at.unix_millis() as f64 / 1000.0,
            "mechanism": { "type": "minidump" },
            "native_crash": {
                "kind": "minidump",
                "blob_key": blob.key.as_str(),
                "object_id": object_id.to_string(),
                "size": blob.size,
                "checksum": blob.checksum.to_string(),
            }
        }))
        .map_err(|_| IngestError::invalid("invalid_minidump_event"))?;
        let accepted = AcceptedEvent {
            project_id: snapshot.project_id,
            event_id,
            received_at,
            policy_revision: snapshot.scrub_policy.revision,
            payload: ScrubbedEventPayload::new(payload),
        };
        let _permit = self
            .storage_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| IngestError::unavailable("storage_wait_capacity"))?;
        let durable = self
            .event_sink
            .persist(accepted)
            .await
            .map_err(map_sink_error)?;
        self.outcome_sink.record(IngestOutcome {
            kind: match durable {
                DurableOutcome::Accepted => IngestOutcomeKind::Accepted,
                DurableOutcome::Duplicate => IngestOutcomeKind::Duplicate,
            },
            reason: "minidump",
            quantity: 1,
        });
        Ok(IngestResult {
            event_id: Some(event_id),
            durable: Some(durable),
            disabled_categories: Vec::new(),
        })
    }

    async fn persist_attachments(
        &self,
        project_id: ProjectId,
        event_id: EventId,
        snapshot: &ProjectSnapshot,
        attachments: Vec<PendingAttachment>,
        allow_png: bool,
    ) -> Result<PersistedAttachments, IngestError> {
        if attachments.is_empty() {
            return Ok(PersistedAttachments::default());
        }
        if !self.attachment_config.enabled {
            return Ok(PersistedAttachments {
                accepted: Vec::new(),
                dropped: attachments.len() as u64,
            });
        }
        let store = self
            .blob_store
            .as_ref()
            .ok_or_else(|| IngestError::unavailable("blob_storage_unavailable"))?;
        let mut result = PersistedAttachments::default();
        for attachment in attachments {
            let Some(bytes) = scrub_safe_attachment(&attachment, snapshot, allow_png)? else {
                result.dropped = result.dropped.saturating_add(1);
                continue;
            };
            let checksum = BlobChecksum::from_bytes(*blake3::hash(&bytes).as_bytes());
            let object_id =
                attachment_object_id(project_id, event_id, attachment.position, checksum);
            let key = BlobKey::event_owned(project_id, event_id, object_id);
            let created_at = self.clock.now();
            let mut writer = store
                .begin(BlobKind::EventAttachment, created_at)
                .await
                .map_err(map_blob_error)?;
            for chunk in bytes.chunks(self.attachment_config.chunk_bytes.max(1)) {
                writer
                    .write_chunk(chunk.into())
                    .await
                    .map_err(map_blob_error)?;
            }
            let blob = writer.commit(key).await.map_err(map_blob_error)?;
            if blob.checksum != checksum {
                return Err(IngestError::unavailable("blob_checksum_mismatch"));
            }
            let filename = AttachmentFilename::sanitized(&attachment.filename)
                .map_err(|_| IngestError::invalid("invalid_attachment_filename"))?;
            let content_type = BlobContentType::new(&attachment.content_type)
                .map_err(|_| IngestError::invalid("invalid_attachment_content_type"))?;
            if attachment.attachment_type.is_empty()
                || attachment.attachment_type.len() > 128
                || attachment.attachment_type.chars().any(char::is_control)
            {
                return Err(IngestError::invalid("invalid_attachment_type"));
            }
            result.accepted.push(EventAttachment {
                attachment_id: object_id,
                blob,
                filename,
                content_type,
                attachment_type: attachment.attachment_type,
            });
        }
        Ok(result)
    }
}

#[derive(Default)]
struct PersistedAttachments {
    accepted: Vec<EventAttachment>,
    dropped: u64,
}

fn scrub_safe_attachment(
    attachment: &PendingAttachment,
    snapshot: &ProjectSnapshot,
    allow_png: bool,
) -> Result<Option<Vec<u8>>, IngestError> {
    match attachment.content_type.as_ref() {
        "application/json" => {
            let mut value: Value = serde_json::from_slice(&attachment.bytes)
                .map_err(|_| IngestError::invalid("invalid_attachment_json"))?;
            scrub_value(&mut value, None, &snapshot.scrub_policy, 0)?;
            serde_json::to_vec(&value)
                .map(Some)
                .map_err(|_| IngestError::invalid("invalid_attachment_json"))
        }
        "text/plain" => {
            let text = std::str::from_utf8(&attachment.bytes)
                .map_err(|_| IngestError::invalid("invalid_attachment_utf8"))?;
            let lowercase = text.to_ascii_lowercase();
            if lowercase.contains("authorization:")
                || lowercase.contains("bearer ")
                || lowercase.contains("password")
                || lowercase.contains("private key")
            {
                Ok(None)
            } else {
                Ok(Some(attachment.bytes.to_vec()))
            }
        }
        "image/png"
            if allow_png
                && attachment.bytes.len() >= 8
                && attachment.bytes[..8] == *b"\x89PNG\r\n\x1a\n" =>
        {
            Ok(Some(attachment.bytes.to_vec()))
        }
        _ => Ok(None),
    }
}

fn attachment_object_id(
    project_id: ProjectId,
    event_id: EventId,
    position: u32,
    checksum: BlobChecksum,
) -> BlobObjectId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric:event-attachment:v1");
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(&event_id.as_bytes());
    hasher.update(&position.to_be_bytes());
    hasher.update(&checksum.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    BlobObjectId::from_bytes(bytes)
}

fn validate_minidump_header(header: &[u8], total_size: u64) -> Result<(), IngestError> {
    if header.len() < 32 || &header[..4] != b"MDMP" {
        return Err(IngestError::invalid("invalid_minidump"));
    }
    let streams = u32::from_le_bytes(header[8..12].try_into().expect("bounded header")) as u64;
    let directory = u32::from_le_bytes(header[12..16].try_into().expect("bounded header")) as u64;
    if streams == 0 || streams > 4096 {
        return Err(IngestError::invalid("invalid_minidump_directory"));
    }
    let directory_end = directory
        .checked_add(streams.saturating_mul(12))
        .ok_or_else(|| IngestError::invalid("invalid_minidump_directory"))?;
    if directory < 32 || directory_end > total_size {
        return Err(IngestError::invalid("invalid_minidump_directory"));
    }
    Ok(())
}

fn minidump_event_id(project_id: ProjectId, checksum: BlobChecksum) -> EventId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric:minidump-event:v1");
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(&checksum.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    EventId::from_bytes(bytes)
}

fn minidump_object_id(
    project_id: ProjectId,
    event_id: EventId,
    checksum: BlobChecksum,
) -> BlobObjectId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric:minidump-object:v1");
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(&event_id.as_bytes());
    hasher.update(&checksum.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    BlobObjectId::from_bytes(bytes)
}

fn append_attachment_metadata(
    payload: &mut Vec<u8>,
    attachments: &[EventAttachment],
) -> Result<(), IngestError> {
    let mut event: Value =
        serde_json::from_slice(payload).map_err(|_| IngestError::invalid("invalid_event_json"))?;
    let object = event
        .as_object_mut()
        .ok_or_else(|| IngestError::invalid("event_not_object"))?;
    let metadata = attachments
        .iter()
        .map(|attachment| {
            serde_json::json!({
                "attachment_id": attachment.attachment_id.to_string(),
                "blob_key": attachment.blob.key.as_str(),
                "filename": attachment.filename.as_str(),
                "content_type": attachment.content_type.as_str(),
                "attachment_type": attachment.attachment_type,
                "size": attachment.blob.size,
                "checksum": attachment.blob.checksum.to_string(),
                "created_at": attachment.blob.created_at.unix_millis(),
            })
        })
        .collect();
    object.insert("attachments".to_owned(), Value::Array(metadata));
    *payload = serde_json::to_vec(&event).map_err(|_| IngestError {
        kind: IngestErrorKind::ScrubFailed,
        code: "scrub_failed",
    })?;
    Ok(())
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
        || snapshot.state != ProjectAcceptanceState::Active
        || snapshot.key_state != ProjectKeyState::Active
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

fn primary_is_feedback(primary: &PrimaryEvent) -> Result<bool, IngestError> {
    let value: Value = serde_json::from_slice(&primary.raw_json)
        .map_err(|_| IngestError::invalid("invalid_event_json"))?;
    Ok(value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "feedback"))
}

fn validate_feedback_attachment_limits(
    attachments: &[PendingAttachment],
    config: FeedbackIngestConfig,
) -> Result<(), IngestError> {
    if attachments.len() > config.max_attachments {
        return Err(IngestError {
            kind: IngestErrorKind::TooLarge,
            code: "feedback_too_many_attachments",
        });
    }
    let mut total = 0_usize;
    for attachment in attachments {
        if attachment.bytes.len() > config.max_attachment_bytes {
            return Err(IngestError {
                kind: IngestErrorKind::TooLarge,
                code: "feedback_attachment_too_large",
            });
        }
        total = total
            .checked_add(attachment.bytes.len())
            .ok_or(IngestError {
                kind: IngestErrorKind::TooLarge,
                code: "feedback_attachments_too_large",
            })?;
    }
    if total > config.max_total_attachment_bytes {
        return Err(IngestError {
            kind: IngestErrorKind::TooLarge,
            code: "feedback_attachments_too_large",
        });
    }
    Ok(())
}

fn normalize_feedback(
    snapshot: &ProjectSnapshot,
    primary: PrimaryEvent,
    envelope_event_id: Option<EventId>,
    received_at: metric_domain::Timestamp,
    config: FeedbackIngestConfig,
) -> Result<FeedbackRecord, IngestError> {
    let mut value: Value = serde_json::from_slice(&primary.raw_json)
        .map_err(|_| IngestError::invalid("invalid_feedback_json"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| IngestError::invalid("feedback_not_object"))?;
    if object.get("type").and_then(Value::as_str) != Some("feedback") {
        return Err(IngestError::invalid("invalid_feedback_type"));
    }
    let feedback_id = object
        .get("event_id")
        .and_then(Value::as_str)
        .ok_or_else(|| IngestError::invalid("missing_feedback_id"))
        .and_then(|value| {
            EventId::parse(value).map_err(|_| IngestError::invalid("invalid_feedback_id"))
        })?;
    for stated in [primary.header_event_id, envelope_event_id]
        .into_iter()
        .flatten()
    {
        if stated != feedback_id {
            return Err(IngestError::invalid("conflicting_event_id"));
        }
    }
    object.remove("project");
    scrub_value(&mut value, None, &snapshot.scrub_policy, 0)?;
    let context = value
        .pointer("/contexts/feedback")
        .and_then(Value::as_object)
        .ok_or_else(|| IngestError::invalid("missing_feedback_context"))?;
    let message = feedback_text(
        context.get("message"),
        config.max_message_bytes,
        true,
        "invalid_feedback_message",
    )?
    .ok_or_else(|| IngestError::invalid("missing_feedback_message"))?;
    let name = feedback_text(
        context.get("name"),
        config.max_name_bytes,
        false,
        "invalid_feedback_name",
    )?;
    let contact_email = feedback_text(
        context
            .get("contact_email")
            .or_else(|| context.get("email")),
        config.max_contact_bytes,
        false,
        "invalid_feedback_contact",
    )?;
    let url = feedback_text(
        context.get("url"),
        config.max_url_bytes,
        false,
        "invalid_feedback_url",
    )?;
    if url.as_deref().is_some_and(|value| {
        url::Url::parse(value)
            .ok()
            .is_none_or(|url| !matches!(url.scheme(), "http" | "https"))
    }) {
        return Err(IngestError::invalid("invalid_feedback_url"));
    }
    let associated_event_id = optional_event_id(
        context.get("associated_event_id"),
        "invalid_associated_event_id",
    )?;
    let replay_id = optional_event_id(context.get("replay_id"), "invalid_replay_id")?;
    let trace_id = value
        .pointer("/contexts/trace/trace_id")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| IngestError::invalid("invalid_feedback_trace_id"))
                .and_then(|value| {
                    TraceId::parse(value)
                        .map_err(|_| IngestError::invalid("invalid_feedback_trace_id"))
                })
        })
        .transpose()?;
    let retention_millis = i64::from(config.retention_days)
        .checked_mul(24 * 60 * 60 * 1_000)
        .ok_or_else(|| IngestError::invalid("invalid_feedback_retention"))?;
    let expires_at = received_at
        .unix_millis()
        .checked_add(retention_millis)
        .and_then(|value| metric_domain::Timestamp::from_unix_millis(value).ok())
        .ok_or_else(|| IngestError::invalid("invalid_feedback_retention"))?;
    Ok(FeedbackRecord {
        project_id: snapshot.project_id,
        feedback_id,
        received_at,
        status: FeedbackStatus::Open,
        status_changed_at: received_at,
        message,
        name,
        contact_email,
        url,
        associated_event_id,
        issue_id: None,
        trace_id,
        replay_id,
        attachments: Vec::new(),
        expires_at,
    })
}

fn feedback_text(
    value: Option<&Value>,
    maximum: usize,
    required: bool,
    code: &'static str,
) -> Result<Option<Box<str>>, IngestError> {
    let Some(value) = value else {
        return if required {
            Err(IngestError::invalid(code))
        } else {
            Ok(None)
        };
    };
    let value = value
        .as_str()
        .ok_or_else(|| IngestError::invalid(code))?
        .trim();
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(IngestError::invalid(code));
    }
    Ok(Some(value.into()))
}

fn optional_event_id(
    value: Option<&Value>,
    code: &'static str,
) -> Result<Option<EventId>, IngestError> {
    value
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| IngestError::invalid(code))
                .and_then(|value| EventId::parse(value).map_err(|_| IngestError::invalid(code)))
        })
        .transpose()
}

fn validate_and_scrub_event(
    primary: PrimaryEvent,
    envelope_event_id: Option<EventId>,
    snapshot: &ProjectSnapshot,
) -> Result<ValidatedPrimaryEvent, IngestError> {
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
    if snapshot
        .inbound_filters
        .has_signal(InboundFilterSignal::Error)
    {
        let needs_request_url = snapshot
            .inbound_filters
            .has_field(InboundFilterSignal::Error, InboundFilterField::RequestHost)
            || snapshot
                .inbound_filters
                .has_field(InboundFilterSignal::Error, InboundFilterField::RequestPath);
        let request_url = if needs_request_url {
            value
                .pointer("/request/url")
                .and_then(Value::as_str)
                .and_then(|value| url::Url::parse(value).ok())
        } else {
            None
        };
        let mut fields = InboundFilterFields::empty(InboundFilterSignal::Error);
        fields.release = value.get("release").and_then(Value::as_str);
        fields.environment = value.get("environment").and_then(Value::as_str);
        fields.service = value
            .get("server_name")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .pointer("/contexts/trace/data/service.name")
                    .and_then(Value::as_str)
            });
        fields.message = value
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/logentry/formatted").and_then(Value::as_str))
            .or_else(|| value.pointer("/logentry/message").and_then(Value::as_str));
        fields.exception_type = value
            .pointer("/exception/values/0/type")
            .and_then(Value::as_str);
        fields.logger = value.get("logger").and_then(Value::as_str);
        fields.request_host = request_url.as_ref().and_then(url::Url::host_str);
        fields.request_path = request_url.as_ref().map(url::Url::path);
        if let Some(matched) = snapshot.inbound_filters.matches(&fields) {
            return Ok(ValidatedPrimaryEvent::Filtered {
                event_id: body_event_id,
                matched,
            });
        }
    }
    let payload = serde_json::to_vec(&value).map_err(|_| IngestError {
        kind: IngestErrorKind::ScrubFailed,
        code: "scrub_failed",
    })?;
    Ok(ValidatedPrimaryEvent::Accepted {
        event_id: body_event_id,
        payload,
    })
}

fn span_filter_fields<'a>(
    record: &'a SpanRecord,
    signal: InboundFilterSignal,
) -> InboundFilterFields<'a> {
    let mut fields = InboundFilterFields::empty(signal);
    fields.release = record.release.as_deref();
    fields.environment = record.environment.as_deref();
    fields.service = record.service.as_deref();
    fields.name = Some(&record.name);
    fields.operation = Some(&record.operation);
    fields.status = Some(&record.status);
    fields.duration_ms = Some(record.duration_ns / 1_000_000);
    fields
}

fn scrub_value(
    value: &mut Value,
    field: Option<&str>,
    policy: &metric_domain::ScrubPolicy,
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
    policy: &metric_domain::ScrubPolicy,
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

fn scrub_ip(value: &mut Value, policy: &metric_domain::ScrubPolicy) -> Result<(), IngestError> {
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

fn normalize_logs(
    snapshot: &ProjectSnapshot,
    received_at: metric_domain::Timestamp,
    payload: &[u8],
) -> Result<Vec<LogRecord>, IngestError> {
    let mut container: Value =
        serde_json::from_slice(payload).map_err(|_| IngestError::invalid("invalid_log_json"))?;
    scrub_value(&mut container, None, &snapshot.scrub_policy, 0)?;
    let values = container
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![container]);
    if values.is_empty() || values.len() > 100 {
        return Err(IngestError::invalid("invalid_log_count"));
    }
    let mut records = Vec::with_capacity(values.len());
    for value in values {
        let occurred_at_ns = seconds_to_ns(
            value
                .get("timestamp")
                .ok_or_else(|| IngestError::invalid("missing_log_timestamp"))?,
        )?;
        let message = bounded_text(
            value
                .get("body")
                .and_then(Value::as_str)
                .ok_or_else(|| IngestError::invalid("missing_log_body"))?,
            8_192,
            "log_body_too_large",
        )?;
        let level = value.get("level").and_then(Value::as_str).unwrap_or("info");
        let attributes = value.get("attributes").and_then(Value::as_object);
        let trace_id = value
            .get("trace_id")
            .and_then(Value::as_str)
            .map(TraceId::parse)
            .transpose()
            .map_err(|_| IngestError::invalid("invalid_log_trace_id"))?;
        let span_id = attribute_string(attributes, "sentry.trace.parent_span_id")
            .map(SpanId::parse)
            .transpose()
            .map_err(|_| IngestError::invalid("invalid_log_span_id"))?;
        let body =
            serde_json::to_vec(&value).map_err(|_| IngestError::invalid("invalid_log_json"))?;
        let id = LogId::deterministic(snapshot.project_id, received_at, occurred_at_ns, &body);
        records.push(LogRecord {
            id,
            project_id: snapshot.project_id,
            received_at,
            occurred_at_ns,
            severity: LogSeverity::from_wire(level),
            message,
            trace_id,
            span_id,
            environment: attribute_boxed(attributes, "sentry.environment", 128),
            release: attribute_boxed(attributes, "sentry.release", 256),
            service: attribute_boxed(attributes, "service.name", 256)
                .or_else(|| attribute_boxed(attributes, "sentry.service.name", 256)),
            body: SignalBody::new(body),
        });
    }
    Ok(records)
}

fn normalize_session(
    snapshot: &ProjectSnapshot,
    received_at: metric_domain::Timestamp,
    payload: &[u8],
) -> Result<SessionUpdate, IngestError> {
    let mut value: Value = serde_json::from_slice(payload)
        .map_err(|_| IngestError::invalid("invalid_session_json"))?;
    scrub_value(&mut value, None, &snapshot.scrub_policy, 0)?;
    let object = value
        .as_object()
        .ok_or_else(|| IngestError::invalid("invalid_session_json"))?;
    let sdk_id = object
        .get("sid")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .filter(|value| !value.is_nil())
        .ok_or_else(|| IngestError::invalid("invalid_session_id"))?;
    let attributes = object.get("attrs").and_then(Value::as_object);
    let release = attribute_string(attributes, "release")
        .ok_or_else(|| IngestError::invalid("missing_session_release"))?;
    validate_version(release).map_err(|_| IngestError::invalid("invalid_session_release"))?;
    let environment = attribute_string(attributes, "environment").unwrap_or("production");
    if environment.is_empty() || environment.len() > 64 {
        return Err(IngestError::invalid("invalid_session_environment"));
    }
    let started_at = parse_session_timestamp(
        object
            .get("started")
            .ok_or_else(|| IngestError::invalid("missing_session_started"))?,
    )?;
    let updated_at = object
        .get("timestamp")
        .map(parse_session_timestamp)
        .transpose()?
        .unwrap_or(received_at);
    let state = match object.get("status").and_then(Value::as_str).unwrap_or("ok") {
        "started" | "ok" => SessionState::Ok,
        "exited" => SessionState::Exited,
        "crashed" => SessionState::Crashed,
        "abnormal" => SessionState::Abnormal,
        _ => return Err(IngestError::invalid("invalid_session_status")),
    };
    let sequence = object.get("seq").and_then(Value::as_u64);
    let duration_ms = object
        .get("duration")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= i64::MAX as f64 / 1_000.0)
        .map(|seconds| (seconds * 1_000.0).round() as u64);
    let user_digest = object
        .get("did")
        .and_then(Value::as_str)
        .or_else(|| attribute_string(attributes, "user_id"))
        .filter(|value| !value.is_empty())
        .map(|value| {
            let mut mac = Hmac::<Sha256>::new_from_slice(snapshot.scrub_policy.hmac_key.expose())
                .expect("HMAC accepts fixed-size key");
            mac.update(b"metric/session-user/v1");
            mac.update(value.as_bytes());
            let digest = mac.finalize().into_bytes();
            let mut bounded = [0; 16];
            bounded.copy_from_slice(&digest[..16]);
            bounded
        });
    let update = SessionUpdate {
        id: SessionId::derive(snapshot.project_id, *sdk_id.as_bytes()),
        project_id: snapshot.project_id,
        release_id: derive_release_id(snapshot.organization_id, release),
        environment_id: derive_environment_id(snapshot.project_id, environment),
        started_at,
        updated_at,
        state,
        sequence,
        duration_ms,
        user_digest,
    };
    update
        .validate()
        .map_err(|_| IngestError::invalid("invalid_session_lifecycle"))?;
    Ok(update)
}

fn normalize_check_in(
    snapshot: &ProjectSnapshot,
    received_at: metric_domain::Timestamp,
    payload: &[u8],
    ingest: MonitorIngestConfig,
) -> Result<MonitorUpdate, IngestError> {
    let mut value: Value = serde_json::from_slice(payload)
        .map_err(|_| IngestError::invalid("invalid_check_in_json"))?;
    scrub_value(&mut value, None, &snapshot.scrub_policy, 0)?;
    let object = value
        .as_object()
        .ok_or_else(|| IngestError::invalid("invalid_check_in_json"))?;
    let check_in_id = object
        .get("check_in_id")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .filter(|value| !value.is_nil())
        .ok_or_else(|| IngestError::invalid("invalid_check_in_id"))?;
    let slug = object
        .get("monitor_slug")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| IngestError::invalid("missing_monitor_slug"))?;
    let environment = object
        .get("environment")
        .and_then(Value::as_str)
        .unwrap_or("production");
    let monitor_id = MonitorId::derive(snapshot.project_id, slug, environment);
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| IngestError::invalid("missing_check_in_status"))
        .and_then(|value| {
            MonitorRunStatus::parse(value)
                .map_err(|_| IngestError::invalid("invalid_check_in_status"))
        })?;
    if matches!(status, MonitorRunStatus::Timeout | MonitorRunStatus::Missed) {
        return Err(IngestError::invalid("invalid_sdk_check_in_status"));
    }
    let duration_ms = object
        .get("duration")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= i64::MAX as f64 / 1_000.0)
        .map(|seconds| (seconds * 1_000.0).round() as u64);
    let started_at = if status == MonitorRunStatus::InProgress {
        received_at
    } else {
        let duration = duration_ms.unwrap_or(0).min(i64::MAX as u64) as i64;
        metric_domain::Timestamp::from_unix_millis(
            received_at.unix_millis().saturating_sub(duration),
        )
        .map_err(|_| IngestError::invalid("invalid_check_in_duration"))?
    };
    let definition = object
        .get("monitor_config")
        .map(|value| {
            normalize_monitor_definition(
                snapshot,
                received_at,
                monitor_id,
                slug,
                environment,
                value,
            )
        })
        .transpose()?;
    let timeout_at = definition
        .as_ref()
        .filter(|_| status == MonitorRunStatus::InProgress)
        .map(|definition| definition.config.timeout_at(started_at))
        .transpose()
        .map_err(|_| IngestError::invalid("invalid_monitor_runtime"))?;
    let release_id = object
        .get("release")
        .and_then(Value::as_str)
        .filter(|value| validate_version(value).is_ok())
        .map(|value| derive_release_id(snapshot.organization_id, value));
    let delete_at = metric_domain::Timestamp::from_unix_millis(
        received_at.unix_millis().saturating_add(
            i64::from(ingest.retention_days)
                .saturating_mul(24 * 60 * 60)
                .saturating_mul(1_000),
        ),
    )
    .map_err(|_| IngestError::invalid("invalid_monitor_retention"))?;
    let run = MonitorRun {
        id: MonitorRunId::sdk(monitor_id, *check_in_id.as_bytes()),
        project_id: snapshot.project_id,
        monitor_id,
        check_in_id: Some(*check_in_id.as_bytes()),
        status,
        source: MonitorRunSource::Sdk,
        scheduled_for: None,
        started_at,
        finished_at: status.is_terminal().then_some(received_at),
        duration_ms,
        received_at,
        release_id,
        timeout_at,
        delete_at: Some(delete_at),
        http_status: None,
        uptime_failure: None,
    };
    let update = MonitorUpdate { definition, run };
    update
        .validate()
        .map_err(|_| IngestError::invalid("invalid_check_in"))?;
    Ok(update)
}

fn normalize_monitor_definition(
    snapshot: &ProjectSnapshot,
    received_at: metric_domain::Timestamp,
    monitor_id: MonitorId,
    slug: &str,
    environment: &str,
    value: &Value,
) -> Result<MonitorDefinition, IngestError> {
    let config = value
        .as_object()
        .ok_or_else(|| IngestError::invalid("invalid_monitor_config"))?;
    let timezone = config
        .get("timezone")
        .and_then(Value::as_str)
        .unwrap_or("UTC");
    if timezone != "UTC" {
        return Err(IngestError::invalid("unsupported_monitor_timezone"));
    }
    let schedule = config
        .get("schedule")
        .and_then(Value::as_object)
        .ok_or_else(|| IngestError::invalid("missing_monitor_schedule"))?;
    let schedule_type = schedule
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| IngestError::invalid("missing_monitor_schedule_type"))?;
    let schedule = match schedule_type {
        "crontab" => MonitorSchedule::crontab(
            schedule
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| IngestError::invalid("invalid_monitor_schedule"))?,
        ),
        "interval" => {
            let value = schedule
                .get("value")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| IngestError::invalid("invalid_monitor_interval"))?;
            let factor = match schedule.get("unit").and_then(Value::as_str) {
                Some("minute") => 1,
                Some("hour") => 60,
                Some("day") => 24 * 60,
                Some("week") => 7 * 24 * 60,
                _ => return Err(IngestError::invalid("invalid_monitor_interval_unit")),
            };
            MonitorSchedule::interval(
                value
                    .checked_mul(factor)
                    .ok_or_else(|| IngestError::invalid("invalid_monitor_interval"))?,
            )
        }
        _ => return Err(IngestError::invalid("unsupported_monitor_schedule")),
    }
    .map_err(|_| IngestError::invalid("invalid_monitor_schedule"))?;
    let checkin_margin_minutes = optional_u32(config, "checkin_margin")?.unwrap_or(1);
    let max_runtime_minutes = optional_u32(config, "max_runtime")?.unwrap_or(30);
    let monitor_config = MonitorConfig {
        schedule,
        checkin_margin_seconds: checkin_margin_minutes
            .checked_mul(60)
            .ok_or_else(|| IngestError::invalid("invalid_monitor_margin"))?,
        max_runtime_seconds: max_runtime_minutes
            .checked_mul(60)
            .ok_or_else(|| IngestError::invalid("invalid_monitor_runtime"))?,
    };
    monitor_config
        .validate()
        .map_err(|_| IngestError::invalid("invalid_monitor_config"))?;
    let next_expected_at = monitor_config
        .schedule
        .next_after(received_at)
        .map_err(|_| IngestError::invalid("invalid_monitor_schedule"))?;
    let definition = MonitorDefinition {
        id: monitor_id,
        project_id: snapshot.project_id,
        slug: slug.into(),
        name: slug.into(),
        environment_id: derive_environment_id(snapshot.project_id, environment),
        environment: environment.into(),
        enabled: true,
        managed_by_web: false,
        revision: 1,
        config: monitor_config,
        uptime: None,
        next_expected_at,
        last_run_id: None,
        last_status: None,
        last_check_in_at: None,
        created_at: received_at,
        updated_at: received_at,
    };
    definition
        .validate()
        .map_err(|_| IngestError::invalid("invalid_monitor_definition"))?;
    Ok(definition)
}

fn optional_u32(object: &Map<String, Value>, key: &str) -> Result<Option<u32>, IngestError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| IngestError::invalid("invalid_monitor_config"))
        })
        .transpose()
}

fn parse_session_timestamp(value: &Value) -> Result<metric_domain::Timestamp, IngestError> {
    let milliseconds = if let Some(seconds) = value.as_f64() {
        if !seconds.is_finite()
            || seconds < i64::MIN as f64 / 1_000.0
            || seconds > i64::MAX as f64 / 1_000.0
        {
            return Err(IngestError::invalid("invalid_session_timestamp"));
        }
        (seconds * 1_000.0).round() as i64
    } else {
        let text = value
            .as_str()
            .ok_or_else(|| IngestError::invalid("invalid_session_timestamp"))?;
        let parsed = OffsetDateTime::parse(text, &Rfc3339)
            .map_err(|_| IngestError::invalid("invalid_session_timestamp"))?;
        i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000)
            .map_err(|_| IngestError::invalid("invalid_session_timestamp"))?
    };
    metric_domain::Timestamp::from_unix_millis(milliseconds)
        .map_err(|_| IngestError::invalid("invalid_session_timestamp"))
}

fn admit_window(
    storage: &Mutex<HashMap<ProjectId, FeedbackRateWindow>>,
    project_id: ProjectId,
    received_at: metric_domain::Timestamp,
    maximum: u32,
    capacity: usize,
    rate_code: &'static str,
    capacity_code: &'static str,
) -> Result<(), IngestError> {
    let mut windows = storage
        .lock()
        .map_err(|_| IngestError::unavailable("rate_limiter_unavailable"))?;
    let now = received_at.unix_millis();
    if let Some(window) = windows.get_mut(&project_id) {
        if now.saturating_sub(window.opened_at) >= 60_000 {
            *window = FeedbackRateWindow {
                opened_at: now,
                submissions: 1,
            };
            return Ok(());
        }
        if window.submissions >= maximum {
            return Err(IngestError::rate_limited(rate_code));
        }
        window.submissions = window.submissions.saturating_add(1);
        return Ok(());
    }
    if windows.len() >= capacity {
        return Err(IngestError::rate_limited(capacity_code));
    }
    windows.insert(
        project_id,
        FeedbackRateWindow {
            opened_at: now,
            submissions: 1,
        },
    );
    Ok(())
}

fn normalize_transaction(
    snapshot: &ProjectSnapshot,
    received_at: metric_domain::Timestamp,
    payload: &[u8],
) -> Result<Vec<SpanRecord>, IngestError> {
    let mut value: Value = serde_json::from_slice(payload)
        .map_err(|_| IngestError::invalid("invalid_transaction_json"))?;
    scrub_value(&mut value, None, &snapshot.scrub_policy, 0)?;
    let trace = value
        .pointer("/contexts/trace")
        .and_then(Value::as_object)
        .ok_or_else(|| IngestError::invalid("missing_transaction_trace"))?;
    let trace_id = required_trace_id(trace.get("trace_id"))?;
    let root_span_id = required_span_id(trace.get("span_id"))?;
    let environment = optional_bounded(value.get("environment"), 128);
    let release = optional_bounded(value.get("release"), 256);
    let service = attribute_boxed(
        trace.get("data").and_then(Value::as_object),
        "service.name",
        256,
    );
    let root = span_from_parts(
        snapshot.project_id,
        received_at,
        &value,
        trace,
        trace_id,
        root_span_id,
        true,
        environment.clone(),
        release.clone(),
        service.clone(),
    )?;
    let children = value
        .get("spans")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if children.len() > 1_000 {
        return Err(IngestError::invalid("too_many_transaction_spans"));
    }
    let mut records = Vec::with_capacity(children.len() + 1);
    records.push(root);
    for child in children {
        let object = child
            .as_object()
            .ok_or_else(|| IngestError::invalid("invalid_transaction_span"))?;
        let child_trace_id = object
            .get("trace_id")
            .map(|value| required_trace_id(Some(value)))
            .transpose()?
            .unwrap_or(trace_id);
        if child_trace_id != trace_id {
            return Err(IngestError::invalid("conflicting_span_trace_id"));
        }
        let span_id = required_span_id(object.get("span_id"))?;
        records.push(span_from_parts(
            snapshot.project_id,
            received_at,
            &child,
            object,
            trace_id,
            span_id,
            false,
            environment.clone(),
            release.clone(),
            service.clone(),
        )?);
    }
    derive_insights(&mut records);
    Ok(records)
}

fn normalize_spans(
    snapshot: &ProjectSnapshot,
    received_at: metric_domain::Timestamp,
    payload: &[u8],
) -> Result<Vec<SpanRecord>, IngestError> {
    let mut container: Value =
        serde_json::from_slice(payload).map_err(|_| IngestError::invalid("invalid_span_json"))?;
    scrub_value(&mut container, None, &snapshot.scrub_policy, 0)?;
    let values = container
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![container]);
    if values.is_empty() || values.len() > 1_000 {
        return Err(IngestError::invalid("invalid_span_count"));
    }
    let mut records = Vec::with_capacity(values.len());
    for value in values {
        let object = value
            .as_object()
            .ok_or_else(|| IngestError::invalid("invalid_span_json"))?;
        let trace_id = required_trace_id(object.get("trace_id"))?;
        let span_id = required_span_id(object.get("span_id"))?;
        let attributes = object
            .get("attributes")
            .or_else(|| object.get("data"))
            .and_then(Value::as_object);
        let environment = attribute_boxed(attributes, "sentry.environment", 128);
        let release = attribute_boxed(attributes, "sentry.release", 256);
        let service = attribute_boxed(attributes, "service.name", 256);
        records.push(span_from_parts(
            snapshot.project_id,
            received_at,
            &value,
            object,
            trace_id,
            span_id,
            object
                .get("is_segment")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            environment,
            release,
            service,
        )?);
    }
    derive_insights(&mut records);
    Ok(records)
}

#[allow(clippy::too_many_arguments)]
fn span_from_parts(
    project_id: ProjectId,
    received_at: metric_domain::Timestamp,
    value: &Value,
    fields: &Map<String, Value>,
    trace_id: TraceId,
    span_id: SpanId,
    is_segment: bool,
    environment: Option<Box<str>>,
    release: Option<Box<str>>,
    service: Option<Box<str>>,
) -> Result<SpanRecord, IngestError> {
    let context = value.pointer("/contexts/trace").and_then(Value::as_object);
    let start = fields
        .get("start_timestamp")
        .or_else(|| value.get("start_timestamp"))
        .ok_or_else(|| IngestError::invalid("missing_span_start"))?;
    let end = fields
        .get("end_timestamp")
        .or_else(|| fields.get("timestamp"))
        .or_else(|| value.get("timestamp"))
        .ok_or_else(|| IngestError::invalid("missing_span_end"))?;
    let started_at_ns = seconds_to_ns(start)?;
    let ended_at_ns = seconds_to_ns(end)?;
    let duration_ns = ended_at_ns
        .checked_sub(started_at_ns)
        .filter(|duration| *duration >= 0)
        .ok_or_else(|| IngestError::invalid("invalid_span_duration"))?;
    let attributes = fields
        .get("attributes")
        .or_else(|| fields.get("data"))
        .and_then(Value::as_object)
        .or_else(|| {
            context
                .and_then(|trace| trace.get("data"))
                .and_then(Value::as_object)
        });
    let operation = fields
        .get("op")
        .and_then(Value::as_str)
        .or_else(|| {
            context
                .and_then(|trace| trace.get("op"))
                .and_then(Value::as_str)
        })
        .or_else(|| attribute_string(attributes, "sentry.op"))
        .unwrap_or("unknown");
    let name = fields
        .get("name")
        .or_else(|| fields.get("description"))
        .or_else(|| value.get("transaction"))
        .and_then(Value::as_str)
        .unwrap_or("unnamed span");
    let status = fields
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| {
            context
                .and_then(|trace| trace.get("status"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    let parent_span_id = fields
        .get("parent_span_id")
        .or_else(|| context.and_then(|trace| trace.get("parent_span_id")))
        .and_then(Value::as_str)
        .map(SpanId::parse)
        .transpose()
        .map_err(|_| IngestError::invalid("invalid_parent_span_id"))?;
    let body = serde_json::to_vec(value).map_err(|_| IngestError::invalid("invalid_span_json"))?;
    Ok(SpanRecord {
        id: SpanRecordId::deterministic(project_id, trace_id, span_id),
        project_id,
        received_at,
        started_at_ns,
        duration_ns,
        trace_id,
        span_id,
        parent_span_id,
        is_segment,
        operation_class: SpanOperationClass::from_operation(operation),
        operation: bounded_text(operation, 128, "span_operation_too_large")?,
        status: bounded_text(status, 64, "span_status_too_large")?,
        name: bounded_text(name, 1_024, "span_name_too_large")?,
        environment,
        release,
        service,
        insight_flags: 0,
        body: SignalBody::new(body),
    })
}

fn derive_insights(records: &mut [SpanRecord]) {
    let Some(root_index) = records.iter().position(|record| record.is_segment) else {
        for record in records {
            record.insight_flags = insight_flags_for_span(record);
        }
        return;
    };
    let mut flags = insight_flags_for_span(&records[root_index]);
    let mut databases = std::collections::BTreeMap::<Box<str>, usize>::new();
    let mut http = std::collections::BTreeMap::<Box<str>, usize>::new();
    let mut cache = 0_usize;
    for record in records.iter().filter(|record| !record.is_segment) {
        flags |= insight_flags_for_span(record);
        match record.operation_class {
            SpanOperationClass::Database => {
                *databases.entry(record.name.clone()).or_default() += 1;
            }
            SpanOperationClass::HttpClient => {
                *http.entry(record.name.clone()).or_default() += 1;
            }
            SpanOperationClass::Cache => cache += 1,
            _ => {}
        }
    }
    if databases.values().any(|count| *count >= 5) {
        flags |= 1 << 1;
    }
    if http.values().any(|count| *count >= 3) {
        flags |= 1 << 2;
    }
    if cache >= 5 {
        flags |= 1 << 4;
    }
    records[root_index].insight_flags = flags;
}

fn insight_flags_for_span(record: &SpanRecord) -> u32 {
    let mut flags = 0_u32;
    if record.is_segment && record.duration_ns >= 1_000_000_000 {
        flags |= 1;
    }
    if record.operation_class == SpanOperationClass::Database && record.duration_ns >= 250_000_000 {
        flags |= 1 << 3;
    }
    if record.operation_class == SpanOperationClass::Queue && record.duration_ns >= 500_000_000 {
        flags |= 1 << 5;
    }
    if record.operation_class == SpanOperationClass::Task && record.duration_ns >= 1_000_000_000 {
        flags |= 1 << 6;
    }
    if !record.is_segment && !matches!(record.status.as_ref(), "" | "ok" | "cancelled") {
        flags |= 1 << 7;
    }
    flags
}

fn required_trace_id(value: Option<&Value>) -> Result<TraceId, IngestError> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| IngestError::invalid("missing_trace_id"))
        .and_then(|value| {
            TraceId::parse(value).map_err(|_| IngestError::invalid("invalid_trace_id"))
        })
}

fn required_span_id(value: Option<&Value>) -> Result<SpanId, IngestError> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| IngestError::invalid("missing_span_id"))
        .and_then(|value| SpanId::parse(value).map_err(|_| IngestError::invalid("invalid_span_id")))
}

fn seconds_to_ns(value: &Value) -> Result<i64, IngestError> {
    let seconds = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
        .filter(|value| value.is_finite())
        .ok_or_else(|| IngestError::invalid("invalid_signal_timestamp"))?;
    let nanoseconds = seconds * 1_000_000_000.0;
    if nanoseconds < i64::MIN as f64 || nanoseconds > i64::MAX as f64 {
        return Err(IngestError::invalid("signal_timestamp_out_of_range"));
    }
    Ok(nanoseconds.round() as i64)
}

fn attribute_string<'a>(attributes: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a str> {
    let value = attributes?.get(key)?;
    value
        .get("value")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
}

fn attribute_boxed(
    attributes: Option<&Map<String, Value>>,
    key: &str,
    maximum: usize,
) -> Option<Box<str>> {
    attribute_string(attributes, key).map(|value| truncate_text(value, maximum).into())
}

fn optional_bounded(value: Option<&Value>, maximum: usize) -> Option<Box<str>> {
    value
        .and_then(Value::as_str)
        .map(|value| truncate_text(value, maximum).into())
}

fn bounded_text(value: &str, maximum: usize, code: &'static str) -> Result<Box<str>, IngestError> {
    if value.chars().any(char::is_control) || value.len() > maximum {
        return Err(IngestError::invalid(code));
    }
    Ok(value.into())
}

fn truncate_text(value: &str, maximum: usize) -> &str {
    if value.len() <= maximum {
        return value;
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &value[..boundary]
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

fn map_signal_store_error(error: SignalStoreError) -> IngestError {
    match error {
        SignalStoreError::Conflict | SignalStoreError::InvalidData => {
            IngestError::invalid("signal_conflict")
        }
        SignalStoreError::Capacity => IngestError::rate_limited("log_lane_capacity"),
        SignalStoreError::NotFound | SignalStoreError::Unavailable => {
            IngestError::unavailable("signal_storage_unavailable")
        }
    }
}

fn map_span_sink_error(error: SignalStoreError) -> IngestError {
    match error {
        SignalStoreError::Conflict | SignalStoreError::InvalidData => {
            IngestError::invalid("invalid_span")
        }
        SignalStoreError::Capacity => IngestError::rate_limited("span_lane_capacity"),
        SignalStoreError::NotFound | SignalStoreError::Unavailable => {
            IngestError::unavailable("span_storage_unavailable")
        }
    }
}

fn map_feedback_store_error(error: FeedbackStoreError) -> IngestError {
    match error {
        FeedbackStoreError::Conflict | FeedbackStoreError::InvalidData => {
            IngestError::invalid("feedback_conflict")
        }
        FeedbackStoreError::Capacity => IngestError::rate_limited("feedback_storage_capacity"),
        FeedbackStoreError::NotFound | FeedbackStoreError::Unavailable => {
            IngestError::unavailable("feedback_storage_unavailable")
        }
    }
}

fn map_sink_error(error: EventSinkError) -> IngestError {
    match error {
        EventSinkError::Unavailable => IngestError::unavailable("storage_unavailable"),
        EventSinkError::Ambiguous => IngestError::unavailable("ambiguous_durable_ack"),
    }
}

fn map_blob_error(error: BlobStoreError) -> IngestError {
    match error {
        BlobStoreError::Invalid => IngestError::invalid("invalid_blob_request"),
        BlobStoreError::TooLarge => IngestError {
            kind: IngestErrorKind::TooLarge,
            code: "minidump_too_large",
        },
        BlobStoreError::Capacity => IngestError::unavailable("blob_capacity_exhausted"),
        BlobStoreError::NotFound | BlobStoreError::Corrupt | BlobStoreError::Unavailable => {
            IngestError::unavailable("blob_storage_unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metric_domain::{
        ItemCapabilities, ProjectIngestLimits, ScrubPolicy, SecretBytes, Timestamp,
    };

    fn snapshot() -> ProjectSnapshot {
        ProjectSnapshot {
            project_id: ProjectId::new(42).unwrap(),
            organization_id: metric_domain::OrganizationId::new(1).unwrap(),
            state: ProjectAcceptanceState::Active,
            key_state: ProjectKeyState::Active,
            scrub_policy: ScrubPolicy {
                revision: 7,
                ip_policy: IpScrubPolicy::Hmac,
                hmac_key: SecretBytes::new([7; 32]),
            },
            items: ItemCapabilities {
                error: true,
                client_report: true,
                log: true,
                transaction: true,
                span: true,
                feedback: true,
                check_in: true,
            },
            limits: ProjectIngestLimits::default(),
            inbound_filters: Default::default(),
            grouping_revision: 1,
        }
    }

    #[test]
    fn feedback_is_bounded_and_scrubbed_before_persistence() {
        let snapshot = snapshot();
        let received_at = Timestamp::from_unix_millis(1_700_000_000_000).unwrap();
        let feedback = normalize_feedback(
            &snapshot,
            PrimaryEvent {
                header_event_id: Some(EventId::from_bytes([3; 16])),
                raw_json: serde_json::to_vec(&serde_json::json!({
                    "event_id": EventId::from_bytes([3; 16]).to_string(),
                    "type": "feedback",
                    "contexts": {
                        "feedback": {
                            "message": "Checkout failed",
                            "name": "Ada",
                            "email": "ada@example.com",
                            "url": "https://example.test/checkout",
                        }
                    },
                    "password": "must-not-survive",
                }))
                .unwrap()
                .into_boxed_slice(),
            },
            Some(EventId::from_bytes([3; 16])),
            received_at,
            FeedbackIngestConfig::default(),
        )
        .unwrap();
        assert_eq!(feedback.message.as_ref(), "Checkout failed");
        assert_eq!(feedback.contact_email.as_deref(), Some("ada@example.com"));
        let oversized = PendingAttachment {
            position: 0,
            filename: "large.txt".into(),
            content_type: "text/plain".into(),
            attachment_type: "event.attachment".into(),
            bytes: vec![0; FeedbackIngestConfig::default().max_attachment_bytes + 1]
                .into_boxed_slice(),
        };
        assert_eq!(
            validate_feedback_attachment_limits(&[oversized], FeedbackIngestConfig::default())
                .unwrap_err()
                .code(),
            "feedback_attachment_too_large"
        );
    }

    #[test]
    fn pinned_node_session_is_compact_and_project_scoped() {
        let update = normalize_session(
            &snapshot(),
            metric_domain::Timestamp::from_unix_millis(1_767_225_601_000).unwrap(),
            br#"{
                "sid":"01234567-89ab-cdef-0123-456789abcdef",
                "init":true,
                "started":"2026-01-01T00:00:00Z",
                "timestamp":"2026-01-01T00:00:01Z",
                "status":"crashed",
                "errors":1,
                "seq":2,
                "did":"bounded-user",
                "attrs":{"release":"backend@1.2.3","environment":"production"}
            }"#,
        )
        .unwrap();
        assert_eq!(update.project_id, ProjectId::new(42).unwrap());
        assert_eq!(update.state, SessionState::Crashed);
        assert_eq!(update.sequence, Some(2));
        assert!(update.user_digest.is_some());
        assert_eq!(
            update.release_id,
            derive_release_id(snapshot().organization_id, "backend@1.2.3")
        );
        assert_eq!(
            update.environment_id,
            derive_environment_id(snapshot().project_id, "production")
        );
    }

    #[test]
    fn mandatory_floor_scrubs_unknown_nested_credentials() {
        let raw = br#"{"event_id":"0123456789abcdef0123456789abcdef","unknown":{"password":"open-sesame","header":"Bearer token","url":"https://user:pass@example.invalid/a"},"user":{"ip_address":"192.0.2.1"}}"#;
        let validated = validate_and_scrub_event(
            PrimaryEvent {
                header_event_id: None,
                raw_json: raw.as_slice().into(),
            },
            None,
            &snapshot(),
        )
        .unwrap();
        let ValidatedPrimaryEvent::Accepted { payload, .. } = validated else {
            panic!("empty filter policy must accept the Event");
        };
        let text = String::from_utf8(payload).unwrap();
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
    fn official_node_log_container_normalizes_correlation_and_scrubs_secrets() {
        let payload = br#"{
            "version":2,
            "items":[{
                "timestamp":1753372800.125,
                "level":"error",
                "body":"payment failed",
                "trace_id":"0123456789abcdef0123456789abcdef",
                "attributes":{
                    "sentry.trace.parent_span_id":{"value":"0123456789abcdef","type":"string"},
                    "sentry.environment":{"value":"production","type":"string"},
                    "service.name":{"value":"checkout","type":"string"},
                    "authorization":{"value":"Bearer secret","type":"string"}
                }
            }]
        }"#;
        let records = normalize_logs(
            &snapshot(),
            metric_domain::Timestamp::from_unix_millis(1_753_372_800_200).unwrap(),
            payload,
        )
        .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message.as_ref(), "payment failed");
        assert_eq!(records[0].service.as_deref(), Some("checkout"));
        assert_eq!(
            records[0].trace_id.unwrap().to_string(),
            "0123456789abcdef0123456789abcdef"
        );
        assert!(!String::from_utf8_lossy(records[0].body.as_bytes()).contains("Bearer secret"));
    }

    #[test]
    fn transaction_expands_to_stable_root_and_children_with_insights() {
        let payload = br#"{
            "type":"transaction",
            "transaction":"GET /orders",
            "start_timestamp":1753372800.0,
            "timestamp":1753372801.5,
            "environment":"production",
            "contexts":{"trace":{
                "trace_id":"0123456789abcdef0123456789abcdef",
                "span_id":"1111111111111111",
                "op":"http.server",
                "status":"ok",
                "data":{"service.name":"api"}
            }},
            "spans":[{
                "trace_id":"0123456789abcdef0123456789abcdef",
                "span_id":"2222222222222222",
                "parent_span_id":"1111111111111111",
                "start_timestamp":1753372800.1,
                "timestamp":1753372800.6,
                "op":"db.sql.query",
                "status":"ok",
                "description":"SELECT orders"
            }]
        }"#;
        let first = normalize_transaction(
            &snapshot(),
            metric_domain::Timestamp::from_unix_millis(1_753_372_801_600).unwrap(),
            payload,
        )
        .unwrap();
        let second = normalize_transaction(
            &snapshot(),
            metric_domain::Timestamp::from_unix_millis(1_753_372_801_600).unwrap(),
            payload,
        )
        .unwrap();
        assert_eq!(first.len(), 2);
        assert!(first[0].is_segment);
        assert_eq!(first[0].id, second[0].id);
        assert_ne!(first[0].insight_flags & 1, 0);
        assert_ne!(first[0].insight_flags & (1 << 3), 0);
    }

    #[test]
    fn malformed_span_time_and_identity_fail_before_storage() {
        let payload = br#"{
            "trace_id":"00000000000000000000000000000000",
            "span_id":"0123456789abcdef",
            "name":"invalid",
            "start_timestamp":5,
            "end_timestamp":4
        }"#;
        let error = normalize_spans(
            &snapshot(),
            metric_domain::Timestamp::from_unix_millis(5_000).unwrap(),
            payload,
        )
        .unwrap_err();
        assert_eq!(error.code(), "invalid_trace_id");
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
