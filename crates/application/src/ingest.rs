use std::{collections::BTreeSet, sync::Arc};

use faultkeep_domain::{
    AcceptedEvent, DsnKey, EventId, IpScrubPolicy, ProjectAcceptanceState, ProjectId,
    ProjectKeyState, ProjectSnapshot, ScrubbedEventPayload,
    blob::{
        AttachmentFilename, BlobChecksum, BlobContentType, BlobKey, BlobKind, BlobObjectId,
        EventAttachment,
    },
    signals::{
        LogId, LogRecord, LogSeverity, SignalBody, SpanId, SpanOperationClass, SpanRecord,
        SpanRecordId, TraceId,
    },
};
use faultkeep_ports::{
    BlobChunkSource, BlobStore, BlobStoreError, Clock, DurableOutcome, EventSink, EventSinkError,
    IngestOutcome, IngestOutcomeKind, LogSink, OutcomeSink, ProjectResolveError, ProjectResolver,
    RandomSource, SignalStore, SignalStoreError,
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
    pub primary: Option<PrimaryEvent>,
    pub signals: Vec<PendingSignal>,
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
    signal_store: Option<Arc<dyn SignalStore>>,
    log_sink: Option<Arc<dyn LogSink>>,
    span_permits: Arc<Semaphore>,
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
            signal_store: None,
            log_sink: None,
            span_permits: Arc::new(Semaphore::new(max_waiting_for_storage.max(1))),
        }
    }

    #[must_use]
    pub fn with_signal_store(mut self, signal_store: Arc<dyn SignalStore>) -> Self {
        self.signal_store = Some(signal_store);
        self
    }

    #[must_use]
    pub fn with_log_sink(mut self, log_sink: Arc<dyn LogSink>) -> Self {
        self.log_sink = Some(log_sink);
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
            validate_and_scrub_event(primary, request.envelope_event_id, &snapshot)?;
        let attachments = self
            .persist_attachments(
                snapshot.project_id,
                event_id,
                &snapshot,
                request.attachments,
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
                    logs.extend(normalize_logs(snapshot, received_at, &signal.raw_json)?)
                }
                PendingSignalKind::Transaction if snapshot.items.transaction => spans.extend(
                    normalize_transaction(snapshot, received_at, &signal.raw_json)?,
                ),
                PendingSignalKind::Span if snapshot.items.span => {
                    spans.extend(normalize_spans(snapshot, received_at, &signal.raw_json)?)
                }
                PendingSignalKind::Log
                | PendingSignalKind::Transaction
                | PendingSignalKind::Span => {
                    disabled_categories.push(match signal.kind {
                        PendingSignalKind::Log => "log",
                        PendingSignalKind::Transaction => "transaction",
                        PendingSignalKind::Span => "span",
                    });
                    self.outcome_sink.record(IngestOutcome {
                        kind: IngestOutcomeKind::Unsupported,
                        reason: "feature_disabled",
                        quantity: 1,
                    });
                }
            }
        }
        if logs.is_empty() && spans.is_empty() {
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
            let store = self
                .signal_store
                .as_ref()
                .ok_or_else(|| IngestError::unavailable("signal_storage_unavailable"))?;
            let _permit = self
                .span_permits
                .clone()
                .try_acquire_owned()
                .map_err(|_| IngestError::rate_limited("span_lane_capacity"))?;
            let quantity = u64::try_from(spans.len()).unwrap_or(u64::MAX);
            store
                .persist_spans(spans)
                .await
                .map_err(map_signal_store_error)?;
            self.outcome_sink.record(IngestOutcome {
                kind: IngestOutcomeKind::Accepted,
                reason: "span",
                quantity,
            });
        }
        disabled_categories.sort_unstable();
        disabled_categories.dedup();
        Ok(disabled_categories)
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
            let Some(bytes) = scrub_safe_attachment(&attachment, snapshot)? else {
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
    hasher.update(b"faultkeep:event-attachment:v1");
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
    hasher.update(b"faultkeep:minidump-event:v1");
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
    hasher.update(b"faultkeep:minidump-object:v1");
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

fn normalize_logs(
    snapshot: &ProjectSnapshot,
    received_at: faultkeep_domain::Timestamp,
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

fn normalize_transaction(
    snapshot: &ProjectSnapshot,
    received_at: faultkeep_domain::Timestamp,
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
    received_at: faultkeep_domain::Timestamp,
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
    received_at: faultkeep_domain::Timestamp,
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
    use faultkeep_domain::{ItemCapabilities, ProjectIngestLimits, ScrubPolicy, SecretBytes};

    fn snapshot() -> ProjectSnapshot {
        ProjectSnapshot {
            project_id: ProjectId::new(42).unwrap(),
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
            },
            limits: ProjectIngestLimits::default(),
            grouping_revision: 1,
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
            faultkeep_domain::Timestamp::from_unix_millis(1_753_372_800_200).unwrap(),
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
            faultkeep_domain::Timestamp::from_unix_millis(1_753_372_801_600).unwrap(),
            payload,
        )
        .unwrap();
        let second = normalize_transaction(
            &snapshot(),
            faultkeep_domain::Timestamp::from_unix_millis(1_753_372_801_600).unwrap(),
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
            faultkeep_domain::Timestamp::from_unix_millis(5_000).unwrap(),
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
