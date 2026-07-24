//! Bounded, versioned Incident Capsule export from ADR-0038.

use std::{
    collections::BTreeSet,
    future::Future,
    io::{self, Write},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use faultkeep_domain::{
    EventId, EventKey, ProjectId, Timestamp,
    api::{EventView, IssueActivityKind, IssueActivityView, IssueStatBucket},
    auth::{AuthContext, Permission, RequestCorrelationId},
    grouping::{GroupingComponentKind, IssueId},
    issue::{ActorKind, ActorRef, IssueSnapshot, IssueStatus},
};
use faultkeep_ports::{Clock, InvestigationStore, InvestigationStoreError};
use futures_util::{StreamExt, stream};
use serde::Serialize;
use serde_json::{Map, Value, json};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::mpsc;
use zip::write::SimpleFileOptions;

use crate::{
    auth::{AuthError, IdentityService},
    issues::{IssueService, IssueServiceError},
    shutdown::ShutdownSignal,
};

pub const INCIDENT_CAPSULE_MEDIA_TYPE: &str = "application/vnd.incident-capsule+zip; version=1";
pub const INCIDENT_CAPSULE_VERSION: u16 = 1;
const MAX_STATISTICS_RANGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const HARD_MAX_EVENTS: usize = 10;
const HARD_MAX_ACTIVITIES: usize = 100;
const HARD_MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
const HARD_MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const HARD_MAX_CONCURRENCY: usize = 4;
const HARD_MAX_TIMEOUT: Duration = Duration::from_secs(30);
const MIN_STREAM_CHUNK_BYTES: usize = 4 * 1024;
const MAX_STREAM_CHUNK_BYTES: usize = 1024 * 1024;
const MAX_STREAM_BUFFER_CHUNKS: usize = 16;

#[derive(Debug, Clone, Copy)]
pub struct IncidentCapsuleConfig {
    pub max_events: usize,
    pub max_activities: usize,
    pub max_total_uncompressed_bytes: u64,
    pub max_entry_bytes: u64,
    pub generation_timeout: Duration,
    pub max_concurrency: usize,
    pub stream_chunk_bytes: usize,
    pub stream_buffer_chunks: usize,
}

impl Default for IncidentCapsuleConfig {
    fn default() -> Self {
        Self {
            max_events: HARD_MAX_EVENTS,
            max_activities: HARD_MAX_ACTIVITIES,
            max_total_uncompressed_bytes: HARD_MAX_TOTAL_BYTES,
            max_entry_bytes: HARD_MAX_ENTRY_BYTES,
            generation_timeout: HARD_MAX_TIMEOUT,
            max_concurrency: HARD_MAX_CONCURRENCY,
            stream_chunk_bytes: 64 * 1024,
            stream_buffer_chunks: 4,
        }
    }
}

impl IncidentCapsuleConfig {
    #[must_use]
    pub fn is_valid(self) -> bool {
        (1..=HARD_MAX_EVENTS).contains(&self.max_events)
            && (1..=HARD_MAX_ACTIVITIES).contains(&self.max_activities)
            && (1..=HARD_MAX_TOTAL_BYTES).contains(&self.max_total_uncompressed_bytes)
            && (1..=HARD_MAX_ENTRY_BYTES).contains(&self.max_entry_bytes)
            && self.max_entry_bytes <= self.max_total_uncompressed_bytes
            && !self.generation_timeout.is_zero()
            && self.generation_timeout <= HARD_MAX_TIMEOUT
            && (1..=HARD_MAX_CONCURRENCY).contains(&self.max_concurrency)
            && (MIN_STREAM_CHUNK_BYTES..=MAX_STREAM_CHUNK_BYTES).contains(&self.stream_chunk_bytes)
            && (1..=MAX_STREAM_BUFFER_CHUNKS).contains(&self.stream_buffer_chunks)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncidentEventSelection {
    Default,
    Explicit(Vec<EventId>),
}

#[derive(Debug, Clone)]
pub struct IncidentCapsuleRequest {
    pub project_id: ProjectId,
    pub issue_id: IssueId,
    pub selection: IncidentEventSelection,
    pub statistics_from: Option<Timestamp>,
    pub statistics_until: Option<Timestamp>,
    pub request_id: RequestCorrelationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IncidentCapsuleError {
    #[error("Incident Capsule request is invalid")]
    InvalidRequest,
    #[error("Incident Capsule request is forbidden")]
    Forbidden,
    #[error("Incident Capsule target does not exist")]
    NotFound,
    #[error("Incident Capsule resource limit was exceeded")]
    LimitExceeded,
    #[error("Incident Capsule generation was cancelled")]
    Cancelled,
    #[error("Incident Capsule generation timed out")]
    GenerationTimeout,
    #[error("Incident Capsule service is temporarily unavailable")]
    Unavailable,
}

impl IncidentCapsuleError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::LimitExceeded => "limit_exceeded",
            Self::Cancelled => "cancelled",
            Self::GenerationTimeout => "generation_timeout",
            Self::Unavailable => "temporarily_unavailable",
        }
    }
}

pub type CapsuleAccessFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), IncidentCapsuleError>> + Send + 'a>>;

/// Authorization/audit capability shared by HTTP and a possible future high-level
/// caller. It exposes neither credential storage nor raw audit persistence.
pub trait IncidentCapsuleAccess: Send + Sync + 'static {
    fn authorize<'a>(
        &'a self,
        context: &'a AuthContext,
        project_id: ProjectId,
    ) -> CapsuleAccessFuture<'a>;

    fn audit<'a>(
        &'a self,
        context: &'a AuthContext,
        request_id: RequestCorrelationId,
        project_id: ProjectId,
        issue_id: IssueId,
        selected_event_count: usize,
        result_size_class: &'static str,
    ) -> CapsuleAccessFuture<'a>;
}

impl IncidentCapsuleAccess for IdentityService {
    fn authorize<'a>(
        &'a self,
        context: &'a AuthContext,
        project_id: ProjectId,
    ) -> CapsuleAccessFuture<'a> {
        Box::pin(async move {
            if !has_export_permissions(context) {
                return Err(IncidentCapsuleError::Forbidden);
            }
            self.authorize_project(context, project_id, Permission::IncidentExport)
                .await
                .map_err(map_auth_error)
        })
    }

    fn audit<'a>(
        &'a self,
        context: &'a AuthContext,
        request_id: RequestCorrelationId,
        project_id: ProjectId,
        issue_id: IssueId,
        selected_event_count: usize,
        result_size_class: &'static str,
    ) -> CapsuleAccessFuture<'a> {
        Box::pin(async move {
            self.record_incident_capsule_audit(
                context,
                request_id,
                project_id,
                issue_id,
                selected_event_count,
                result_size_class,
            )
            .await
            .map_err(map_auth_error)
        })
    }
}

fn has_export_permissions(context: &AuthContext) -> bool {
    context.permissions.contains(Permission::IssueRead)
        && context.permissions.contains(Permission::EventRead)
        && context.permissions.contains(Permission::IncidentExport)
}

pub struct IncidentCapsuleDownload {
    pub filename: String,
    pub media_type: &'static str,
    pub selected_event_count: usize,
    pub uncompressed_bytes: u64,
    pub receiver: mpsc::Receiver<Result<Vec<u8>, IncidentCapsuleError>>,
}

pub struct IncidentCapsuleService {
    access: Arc<dyn IncidentCapsuleAccess>,
    issues: Arc<IssueService>,
    investigation: Arc<dyn InvestigationStore>,
    clock: Arc<dyn Clock>,
    config: IncidentCapsuleConfig,
    shutdown: ShutdownSignal,
}

impl IncidentCapsuleService {
    pub fn new(
        access: Arc<dyn IncidentCapsuleAccess>,
        issues: Arc<IssueService>,
        investigation: Arc<dyn InvestigationStore>,
        clock: Arc<dyn Clock>,
        config: IncidentCapsuleConfig,
        shutdown: ShutdownSignal,
    ) -> Result<Self, IncidentCapsuleError> {
        if !config.is_valid() {
            return Err(IncidentCapsuleError::InvalidRequest);
        }
        Ok(Self {
            access,
            issues,
            investigation,
            clock,
            config,
            shutdown,
        })
    }

    pub async fn prepare(
        &self,
        context: &AuthContext,
        request: IncidentCapsuleRequest,
    ) -> Result<IncidentCapsuleDownload, IncidentCapsuleError> {
        let started = Instant::now();
        let generation_deadline = started
            .checked_add(self.config.generation_timeout)
            .ok_or(IncidentCapsuleError::GenerationTimeout)?;
        let result = tokio::select! {
            () = self.shutdown.cancelled() => Err(IncidentCapsuleError::Cancelled),
            result = tokio::time::timeout(
                self.config.generation_timeout,
                self.prepare_inner(context, &request),
            ) => result.map_err(|_| IncidentCapsuleError::GenerationTimeout)?,
        };
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                observe_generation(error.code(), started);
                return Err(error);
            }
        };

        let audit_remaining = generation_deadline.saturating_duration_since(Instant::now());
        if audit_remaining.is_zero() {
            observe_generation(IncidentCapsuleError::GenerationTimeout.code(), started);
            return Err(IncidentCapsuleError::GenerationTimeout);
        }
        let audit = tokio::time::timeout(
            audit_remaining,
            self.access.audit(
                context,
                request.request_id,
                request.project_id,
                request.issue_id,
                prepared.selected_event_count,
                size_class(prepared.uncompressed_bytes),
            ),
        )
        .await
        .map_err(|_| IncidentCapsuleError::GenerationTimeout)
        .and_then(|result| result);
        if let Err(error) = audit {
            observe_generation(error.code(), started);
            return Err(error);
        }
        observe_generation("ready", started);

        metrics::histogram!("faultkeep_incident_capsule_selected_events")
            .record(prepared.selected_event_count as f64);
        metrics::histogram!("faultkeep_incident_capsule_uncompressed_bytes")
            .record(prepared.uncompressed_bytes as f64);

        let (sender, receiver) = mpsc::channel(self.config.stream_buffer_chunks);
        let error_sender = sender.clone();
        let chunk_bytes = self.config.stream_chunk_bytes;
        let shutdown = self.shutdown.clone();
        let stream_deadline = generation_deadline;
        tokio::task::spawn_blocking(move || {
            if let Err(error) = write_archive(
                prepared.entries,
                sender,
                chunk_bytes,
                shutdown,
                stream_deadline,
            ) {
                let _ = error_sender.try_send(Err(error));
            }
        });
        Ok(IncidentCapsuleDownload {
            filename: format!("issue-{}.incident.zip", request.issue_id),
            media_type: INCIDENT_CAPSULE_MEDIA_TYPE,
            selected_event_count: prepared.selected_event_count,
            uncompressed_bytes: prepared.uncompressed_bytes,
            receiver,
        })
    }

    async fn prepare_inner(
        &self,
        context: &AuthContext,
        request: &IncidentCapsuleRequest,
    ) -> Result<PreparedCapsule, IncidentCapsuleError> {
        validate_request(request, self.config.max_events)?;
        self.access.authorize(context, request.project_id).await?;
        let issue = self
            .issues
            .load(request.project_id, request.issue_id)
            .await
            .map_err(map_issue_error)?;

        let selection = self.select_events(request, &issue).await?;
        let statistics_range = statistics_range(request, &issue)?;
        let (events, statistics, activity) = tokio::try_join!(
            self.load_selected_events(request.project_id, request.issue_id, &selection.ids),
            async {
                self.investigation
                    .issue_statistics(
                        request.project_id,
                        request.issue_id,
                        statistics_range.0,
                        statistics_range.1,
                        100,
                    )
                    .await
                    .map_err(map_store_error)
            },
            async {
                self.investigation
                    .issue_activity(
                        request.project_id,
                        request.issue_id,
                        None,
                        self.config.max_activities,
                    )
                    .await
                    .map_err(map_store_error)
            },
        )?;

        let mut omissions = selection.omissions;
        let (mut event_views, missing) = events;
        omissions.extend(missing);
        if statistics.len() == 100 {
            omissions.push(Omission::new(
                "statistics_limit_reached",
                "Hourly statistics were bounded to the first 100 retained buckets",
            ));
        }
        event_views.sort_by_key(|event| event.key.event_id().as_bytes());

        let mut entries = Vec::with_capacity(event_views.len() + 6);
        entries.push(CapsuleEntry::json("issue.json", issue_value(&issue)?));
        if statistics.is_empty() {
            omissions.push(Omission::new(
                "statistics_not_retained",
                "No retained hourly buckets matched the bounded range",
            ));
        } else {
            entries.push(CapsuleEntry::json(
                "statistics/hourly.json",
                statistics_value(statistics_range, &statistics)?,
            ));
        }
        if activity.items.is_empty() {
            omissions.push(Omission::new(
                "activity_empty",
                "No retained activity entries were available",
            ));
        } else {
            entries.push(CapsuleEntry::json(
                "activity.json",
                activity_value(&activity.items)?,
            ));
        }
        for event in &event_views {
            entries.push(CapsuleEntry::json(
                format!("events/{}.json", event.key.event_id()),
                event_value(event)?,
            ));
        }
        entries.push(CapsuleEntry::json(
            "diagnostics/capabilities.json",
            capabilities_value(),
        ));
        entries.push(CapsuleEntry::text(
            "README.txt",
            b"Faultkeep Incident Capsule version 1\n\
              \n\
              This archive contains scrubbed investigation DTOs for one Issue.\n\
              It contains no attachment bytes, debug files, source bundles, credentials,\n\
              internal storage keys, or server-side share capability.\n",
        ));

        let selected_event_count = event_views.len();
        let selection_value = SelectionManifest {
            mode: match &request.selection {
                IncidentEventSelection::Default => "default",
                IncidentEventSelection::Explicit(_) => "explicit",
            },
            event_ids: selection.ids.iter().map(ToString::to_string).collect(),
            statistics_from: timestamp_string(statistics_range.0)?,
            statistics_until: timestamp_string(statistics_range.1)?,
        };
        let generated_at = timestamp_string(self.clock.now())?;
        validate_entries(&entries, self.config)?;
        let manifest = manifest_entry(
            generated_at,
            context,
            request,
            selection_value,
            &entries,
            omissions,
        )?;
        entries.push(manifest);
        validate_entries(&entries, self.config)?;
        let uncompressed_bytes = entries.iter().try_fold(0_u64, |total, entry| {
            total
                .checked_add(entry.bytes.len() as u64)
                .ok_or(IncidentCapsuleError::LimitExceeded)
        })?;
        Ok(PreparedCapsule {
            entries,
            selected_event_count,
            uncompressed_bytes,
        })
    }

    async fn select_events(
        &self,
        request: &IncidentCapsuleRequest,
        issue: &IssueSnapshot,
    ) -> Result<SelectedEvents, IncidentCapsuleError> {
        match &request.selection {
            IncidentEventSelection::Explicit(ids) => Ok(SelectedEvents {
                ids: ids.clone(),
                omissions: Vec::new(),
            }),
            IncidentEventSelection::Default => {
                let until = add_millis(issue.last_seen, 1)?;
                let recent = self
                    .investigation
                    .list_events(
                        request.project_id,
                        Some(request.issue_id),
                        issue.first_seen,
                        until,
                        None,
                        self.config.max_events,
                    )
                    .await
                    .map_err(map_store_error)?;
                let mut seen = BTreeSet::new();
                let mut ids = Vec::with_capacity(self.config.max_events);
                for id in [
                    issue.first_event_id,
                    issue.latest_event_id,
                    issue.representative_event_id,
                ]
                .into_iter()
                .chain(recent.items.iter().map(|event| event.key.event_id()))
                {
                    if seen.insert(id.as_bytes()) {
                        ids.push(id);
                        if ids.len() == self.config.max_events {
                            break;
                        }
                    }
                }
                Ok(SelectedEvents {
                    ids,
                    omissions: Vec::new(),
                })
            }
        }
    }

    async fn load_selected_events(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
        ids: &[EventId],
    ) -> Result<(Vec<EventView>, Vec<Omission>), IncidentCapsuleError> {
        let investigation = Arc::clone(&self.investigation);
        let results = stream::iter(ids.iter().copied())
            .map(move |event_id| {
                let investigation = Arc::clone(&investigation);
                async move {
                    let result = investigation
                        .load_event(project_id, EventKey::new(project_id, event_id))
                        .await;
                    (event_id, result)
                }
            })
            .buffered(self.config.max_concurrency)
            .collect::<Vec<_>>()
            .await;
        let mut events = Vec::with_capacity(results.len());
        let mut omissions = Vec::new();
        for (event_id, result) in results {
            match result {
                Ok(event) if event.issue_id == issue_id => events.push(event),
                Ok(_) => return Err(IncidentCapsuleError::InvalidRequest),
                Err(InvestigationStoreError::NotFound) => omissions.push(Omission::new(
                    "event_not_retained",
                    format!("Selected Event {event_id} is no longer retained"),
                )),
                Err(error) => return Err(map_store_error(error)),
            }
        }
        Ok((events, omissions))
    }
}

fn observe_generation(outcome: &'static str, started: Instant) {
    metrics::counter!("faultkeep_incident_capsule_exports_total", "outcome" => outcome)
        .increment(1);
    metrics::histogram!("faultkeep_incident_capsule_generation_seconds")
        .record(started.elapsed().as_secs_f64());
}

struct SelectedEvents {
    ids: Vec<EventId>,
    omissions: Vec<Omission>,
}

struct PreparedCapsule {
    entries: Vec<CapsuleEntry>,
    selected_event_count: usize,
    uncompressed_bytes: u64,
}

struct CapsuleEntry {
    path: String,
    media_type: &'static str,
    bytes: Vec<u8>,
    compression: zip::CompressionMethod,
}

impl CapsuleEntry {
    fn json(path: impl Into<String>, value: Value) -> Self {
        Self {
            path: path.into(),
            media_type: "application/json",
            bytes: serde_json::to_vec(&value).expect("JSON value serialization cannot fail"),
            compression: zip::CompressionMethod::Deflated,
        }
    }

    fn text(path: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            path: path.into(),
            media_type: "text/plain; charset=utf-8",
            bytes: bytes.to_vec(),
            compression: zip::CompressionMethod::Deflated,
        }
    }
}

#[derive(Serialize)]
struct SelectionManifest {
    mode: &'static str,
    event_ids: Vec<String>,
    statistics_from: String,
    statistics_until: String,
}

#[derive(Serialize)]
struct EntryManifest {
    path: String,
    media_type: &'static str,
    uncompressed_size: u64,
    blake3: String,
}

#[derive(Serialize)]
struct Omission {
    code: &'static str,
    safe_detail: String,
}

impl Omission {
    fn new(code: &'static str, safe_detail: impl Into<String>) -> Self {
        Self {
            code,
            safe_detail: safe_detail.into(),
        }
    }
}

#[derive(Serialize)]
struct Manifest {
    format: &'static str,
    version: u16,
    generated_at: String,
    organization_id: String,
    project_id: String,
    issue_id: String,
    selection: SelectionManifest,
    entries: Vec<EntryManifest>,
    omissions: Vec<Omission>,
}

fn manifest_entry(
    generated_at: String,
    context: &AuthContext,
    request: &IncidentCapsuleRequest,
    selection: SelectionManifest,
    entries: &[CapsuleEntry],
    omissions: Vec<Omission>,
) -> Result<CapsuleEntry, IncidentCapsuleError> {
    let entries = entries
        .iter()
        .map(|entry| EntryManifest {
            path: entry.path.clone(),
            media_type: entry.media_type,
            uncompressed_size: entry.bytes.len() as u64,
            blake3: blake3::hash(&entry.bytes).to_hex().to_string(),
        })
        .collect();
    let bytes = serde_json::to_vec(&Manifest {
        format: "incident-capsule",
        version: INCIDENT_CAPSULE_VERSION,
        generated_at,
        organization_id: context.organization_id.get().to_string(),
        project_id: request.project_id.get().to_string(),
        issue_id: request.issue_id.to_string(),
        selection,
        entries,
        omissions,
    })
    .map_err(|_| IncidentCapsuleError::Unavailable)?;
    Ok(CapsuleEntry {
        path: "manifest.json".to_owned(),
        media_type: "application/json",
        bytes,
        compression: zip::CompressionMethod::Deflated,
    })
}

fn validate_request(
    request: &IncidentCapsuleRequest,
    max_events: usize,
) -> Result<(), IncidentCapsuleError> {
    if let IncidentEventSelection::Explicit(ids) = &request.selection {
        let unique = ids.iter().map(|id| id.as_bytes()).collect::<BTreeSet<_>>();
        if ids.is_empty() || ids.len() > max_events || unique.len() != ids.len() {
            return Err(IncidentCapsuleError::InvalidRequest);
        }
    }
    if request.statistics_from.is_some() != request.statistics_until.is_some() {
        return Err(IncidentCapsuleError::InvalidRequest);
    }
    Ok(())
}

fn validate_entries(
    entries: &[CapsuleEntry],
    config: IncidentCapsuleConfig,
) -> Result<(), IncidentCapsuleError> {
    let mut paths = BTreeSet::new();
    let mut total = 0_u64;
    for entry in entries {
        if !safe_path(&entry.path)
            || !paths.insert(entry.path.as_str())
            || entry.bytes.len() as u64 > config.max_entry_bytes
        {
            return Err(IncidentCapsuleError::LimitExceeded);
        }
        total = total
            .checked_add(entry.bytes.len() as u64)
            .ok_or(IncidentCapsuleError::LimitExceeded)?;
        if total > config.max_total_uncompressed_bytes {
            return Err(IncidentCapsuleError::LimitExceeded);
        }
    }
    Ok(())
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 128
        && path.is_ascii()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !path.bytes().any(|byte| byte.is_ascii_control())
}

fn statistics_range(
    request: &IncidentCapsuleRequest,
    issue: &IssueSnapshot,
) -> Result<(Timestamp, Timestamp), IncidentCapsuleError> {
    let until = request
        .statistics_until
        .unwrap_or(add_millis(issue.last_seen, 1)?);
    let from = request.statistics_from.unwrap_or_else(|| {
        let bounded = until
            .unix_millis()
            .saturating_sub(MAX_STATISTICS_RANGE.as_millis() as i64);
        Timestamp::from_unix_millis(bounded)
            .unwrap_or(issue.first_seen)
            .max(issue.first_seen)
    });
    let range = until.unix_millis().saturating_sub(from.unix_millis());
    if from >= until || range > MAX_STATISTICS_RANGE.as_millis() as i64 {
        return Err(IncidentCapsuleError::InvalidRequest);
    }
    Ok((from, until))
}

fn issue_value(issue: &IssueSnapshot) -> Result<Value, IncidentCapsuleError> {
    let workflow = issue
        .workflow
        .as_ref()
        .map(|workflow| {
            Ok(json!({
                "at": timestamp_string(workflow.at)?,
                "actor": actor_value(workflow.actor),
            }))
        })
        .transpose()?;
    let regression = issue
        .regression
        .as_ref()
        .map(|regression| {
            Ok(json!({
                "at": timestamp_string(regression.at)?,
                "event_id": regression.event_id.to_string(),
                "count": regression.count.get(),
            }))
        })
        .transpose()?;
    Ok(json!({
        "id": issue.issue_id.to_string(),
        "project_id": issue.project_id.get().to_string(),
        "title": issue.title.as_str(),
        "culprit": issue.culprit.as_ref().map(|value| value.as_str()),
        "status": match issue.status {
            IssueStatus::Open => "open",
            IssueStatus::Resolved => "resolved",
            IssueStatus::Ignored => "ignored",
        },
        "first_seen": timestamp_string(issue.first_seen)?,
        "last_seen": timestamp_string(issue.last_seen)?,
        "first_event_id": issue.first_event_id.to_string(),
        "latest_event_id": issue.latest_event_id.to_string(),
        "representative_event_id": issue.representative_event_id.to_string(),
        "occurrence_count": issue.occurrence_count.get(),
        "occurrence_count_approximate": true,
        "assignee": issue.assignee.map(actor_value),
        "workflow": workflow,
        "regression": regression,
        "first_release": issue.first_release.as_ref().map(|value| value.as_str()),
        "last_release": issue.last_release.as_ref().map(|value| value.as_str()),
        "grouping": {
            "revision": issue.grouping_key.revision(),
            "strategy": issue.grouping.strategy.as_str(),
            "summary": issue.grouping.explanation.summary,
            "components": issue.grouping.explanation.components.iter().map(|component| json!({
                "kind": grouping_component_name(component.kind),
                "value": component.value,
            })).collect::<Vec<_>>(),
        },
    }))
}

fn event_value(event: &EventView) -> Result<Value, IncidentCapsuleError> {
    let payload: Value = serde_json::from_slice(event.payload.as_bytes())
        .map_err(|_| IncidentCapsuleError::Unavailable)?;
    let body = allowlisted_event_body(payload)?;
    Ok(json!({
        "event_id": event.key.event_id().to_string(),
        "project_id": event.key.project_id().get().to_string(),
        "issue_id": event.issue_id.to_string(),
        "received_at": timestamp_string(event.received_at)?,
        "occurred_at": timestamp_string(event.occurred_at)?,
        "level": event.level.as_str(),
        "platform": event.platform.as_str(),
        "body": body,
    }))
}

fn allowlisted_event_body(payload: Value) -> Result<Value, IncidentCapsuleError> {
    const ALLOWED: [&str; 16] = [
        "timestamp",
        "platform",
        "level",
        "logger",
        "message",
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
    ];
    let Value::Object(mut source) = payload else {
        return Err(IncidentCapsuleError::Unavailable);
    };
    let mut output = Map::new();
    for key in ALLOWED {
        if let Some(value) = source.remove(key) {
            output.insert(key.to_owned(), value);
        }
    }
    if let Some(value) = source.remove("breadcrumbs") {
        output.insert("breadcrumbs".to_owned(), value);
    }
    if let Some(Value::Object(mut diagnostics)) = source.remove("_faultkeep") {
        let mut allowed = Map::new();
        if let Some(value) = diagnostics.remove("normalization") {
            allowed.insert("normalization".to_owned(), value);
        }
        if let Some(value) = diagnostics.remove("symbolication") {
            allowed.insert("symbolication".to_owned(), value);
        }
        if !allowed.is_empty() {
            output.insert("processing_diagnostics".to_owned(), Value::Object(allowed));
        }
    }
    Ok(Value::Object(output))
}

fn statistics_value(
    range: (Timestamp, Timestamp),
    statistics: &[IssueStatBucket],
) -> Result<Value, IncidentCapsuleError> {
    Ok(json!({
        "from": timestamp_string(range.0)?,
        "until": timestamp_string(range.1)?,
        "buckets": statistics.iter().map(|bucket| Ok(json!({
            "bucket_start": timestamp_string(bucket.bucket_start)?,
            "occurrence_count": bucket.occurrence_count.get(),
        }))).collect::<Result<Vec<_>, IncidentCapsuleError>>()?,
    }))
}

fn activity_value(activity: &[IssueActivityView]) -> Result<Value, IncidentCapsuleError> {
    Ok(json!({
        "items": activity.iter().map(|item| Ok(json!({
            "id": hex::encode(item.id.as_bytes()),
            "issue_id": item.issue_id.to_string(),
            "kind": match item.kind {
                IssueActivityKind::Resolved => "resolved",
                IssueActivityKind::Ignored => "ignored",
                IssueActivityKind::Reopened => "reopened",
                IssueActivityKind::Assigned => "assigned",
                IssueActivityKind::Unassigned => "unassigned",
                IssueActivityKind::Regressed => "regressed",
            },
            "actor": actor_value(item.actor),
            "event_id": item.event_key.map(|key| key.event_id().to_string()),
            "at": timestamp_string(item.at)?,
        }))).collect::<Result<Vec<_>, IncidentCapsuleError>>()?,
    }))
}

fn capabilities_value() -> Value {
    json!({
        "format": "incident-capsule",
        "version": INCIDENT_CAPSULE_VERSION,
        "scrubbed_event_dtos": true,
        "raw_and_symbolicated_frames": true,
        "attachment_metadata": false,
        "attachment_bytes": false,
        "minidump_bytes": false,
        "debug_files": false,
        "artifact_bundles": false,
        "source_archives": false,
        "network_source_fetch": false,
        "production_import": false,
    })
}

fn actor_value(actor: ActorRef) -> Value {
    json!({
        "kind": match actor.kind() {
            ActorKind::User => "user",
            ActorKind::ApiCredential => "api_credential",
            ActorKind::System => "system",
        },
        "id": hex::encode(actor.id()),
    })
}

fn grouping_component_name(kind: GroupingComponentKind) -> &'static str {
    match kind {
        GroupingComponentKind::SdkFingerprint => "sdk_fingerprint",
        GroupingComponentKind::DefaultStrategy => "default_strategy",
        GroupingComponentKind::DefaultDigest => "default_digest",
        GroupingComponentKind::ExceptionType => "exception_type",
        GroupingComponentKind::Frame => "frame",
        GroupingComponentKind::FrameFunction => "frame_function",
        GroupingComponentKind::FrameModule => "frame_module",
        GroupingComponentKind::FramePath => "frame_path",
        GroupingComponentKind::FrameLine => "frame_line",
        GroupingComponentKind::NativeModule => "native_module",
        GroupingComponentKind::NativeRelativeAddress => "native_relative_address",
        GroupingComponentKind::Logger => "logger",
        GroupingComponentKind::Message => "message",
    }
}

fn timestamp_string(timestamp: Timestamp) -> Result<String, IncidentCapsuleError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp.unix_millis()) * 1_000_000)
        .map_err(|_| IncidentCapsuleError::Unavailable)?
        .format(&Rfc3339)
        .map_err(|_| IncidentCapsuleError::Unavailable)
}

fn add_millis(timestamp: Timestamp, millis: i64) -> Result<Timestamp, IncidentCapsuleError> {
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_add(millis))
        .map_err(|_| IncidentCapsuleError::InvalidRequest)
}

fn size_class(bytes: u64) -> &'static str {
    if bytes <= 1024 * 1024 {
        "small"
    } else if bytes <= 16 * 1024 * 1024 {
        "medium"
    } else {
        "large"
    }
}

fn map_auth_error(error: AuthError) -> IncidentCapsuleError {
    match error {
        AuthError::Forbidden | AuthError::InvalidCredentials => IncidentCapsuleError::Forbidden,
        _ => IncidentCapsuleError::Unavailable,
    }
}

fn map_issue_error(error: IssueServiceError) -> IncidentCapsuleError {
    match error {
        IssueServiceError::NotFound => IncidentCapsuleError::NotFound,
        IssueServiceError::InvalidGroupingIdentity
        | IssueServiceError::InvalidSummary
        | IssueServiceError::InvalidData
        | IssueServiceError::IdentityCollision => IncidentCapsuleError::Unavailable,
        IssueServiceError::Unavailable => IncidentCapsuleError::Unavailable,
    }
}

fn map_store_error(error: InvestigationStoreError) -> IncidentCapsuleError {
    match error {
        InvestigationStoreError::NotFound => IncidentCapsuleError::NotFound,
        InvestigationStoreError::InvalidData | InvestigationStoreError::Unavailable => {
            IncidentCapsuleError::Unavailable
        }
    }
}

fn write_archive(
    entries: Vec<CapsuleEntry>,
    sender: mpsc::Sender<Result<Vec<u8>, IncidentCapsuleError>>,
    chunk_bytes: usize,
    shutdown: ShutdownSignal,
    deadline: Instant,
) -> Result<(), IncidentCapsuleError> {
    let output = ChannelWriter::new(sender, chunk_bytes, shutdown, deadline);
    let mut archive = zip::ZipWriter::new_stream(output);
    for entry in entries {
        let options = SimpleFileOptions::default()
            .compression_method(entry.compression)
            .large_file(true)
            .last_modified_time(zip::DateTime::default())
            .unix_permissions(0o100644);
        archive
            .start_file(&entry.path, options)
            .map_err(|_| IncidentCapsuleError::Unavailable)?;
        for chunk in entry.bytes.chunks(chunk_bytes) {
            archive.write_all(chunk).map_err(map_stream_write_error)?;
        }
    }
    archive
        .finish()
        .map_err(|_| IncidentCapsuleError::Unavailable)?
        .into_inner()
        .finish()
}

fn map_stream_write_error(error: io::Error) -> IncidentCapsuleError {
    if error.kind() == io::ErrorKind::Interrupted {
        IncidentCapsuleError::Cancelled
    } else if error.kind() == io::ErrorKind::TimedOut {
        IncidentCapsuleError::GenerationTimeout
    } else if error.kind() == io::ErrorKind::BrokenPipe {
        metrics::counter!("faultkeep_incident_capsule_stream_disconnects_total").increment(1);
        IncidentCapsuleError::Cancelled
    } else {
        IncidentCapsuleError::Unavailable
    }
}

struct ChannelWriter {
    sender: mpsc::Sender<Result<Vec<u8>, IncidentCapsuleError>>,
    buffer: Vec<u8>,
    chunk_bytes: usize,
    shutdown: ShutdownSignal,
    deadline: Instant,
}

impl ChannelWriter {
    fn new(
        sender: mpsc::Sender<Result<Vec<u8>, IncidentCapsuleError>>,
        chunk_bytes: usize,
        shutdown: ShutdownSignal,
        deadline: Instant,
    ) -> Self {
        Self {
            sender,
            buffer: Vec::with_capacity(chunk_bytes),
            chunk_bytes,
            shutdown,
            deadline,
        }
    }

    fn send_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        if self.shutdown.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "capsule generation cancelled",
            ));
        }
        let bytes = std::mem::replace(&mut self.buffer, Vec::with_capacity(self.chunk_bytes));
        let mut message = Ok(bytes);
        loop {
            if self.shutdown.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "capsule generation cancelled",
                ));
            }
            if Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "capsule generation deadline exceeded",
                ));
            }
            match self.sender.try_send(message) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "capsule receiver closed",
                    ));
                }
                Err(mpsc::error::TrySendError::Full(returned)) => {
                    message = returned;
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    fn finish(mut self) -> Result<(), IncidentCapsuleError> {
        self.send_buffer().map_err(map_stream_write_error)
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let original = bytes.len();
        while !bytes.is_empty() {
            let available = self.chunk_bytes - self.buffer.len();
            let take = available.min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() == self.chunk_bytes {
                self.send_buffer()?;
            }
        }
        Ok(original)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_buffer()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::shutdown::ShutdownRoot;

    #[test]
    fn configuration_and_paths_are_bounded() {
        assert!(IncidentCapsuleConfig::default().is_valid());
        assert!(
            !IncidentCapsuleConfig {
                max_events: 11,
                ..IncidentCapsuleConfig::default()
            }
            .is_valid()
        );
        assert!(safe_path("events/00112233445566778899aabbccddeeff.json"));
        assert!(!safe_path("../manifest.json"));
        assert!(!safe_path("events\\event.json"));
    }

    #[test]
    fn export_requires_issue_event_and_incident_permissions_together() {
        use faultkeep_domain::{
            OrganizationId,
            auth::{Actor, CredentialId, OrganizationRole, PermissionSet, UserId},
        };

        let context = |permissions| AuthContext {
            actor: Actor::PersonalApiToken,
            user_id: UserId::new(1).unwrap(),
            organization_id: OrganizationId::new(7).unwrap(),
            role: OrganizationRole::Viewer,
            permissions,
            credential_id: CredentialId::new(2).unwrap(),
        };
        assert!(has_export_permissions(&context(
            PermissionSet::from_permissions([
                Permission::IssueRead,
                Permission::EventRead,
                Permission::IncidentExport,
            ])
        )));
        for missing in [
            Permission::IssueRead,
            Permission::EventRead,
            Permission::IncidentExport,
        ] {
            let permissions = PermissionSet::from_permissions(
                [
                    Permission::IssueRead,
                    Permission::EventRead,
                    Permission::IncidentExport,
                ]
                .into_iter()
                .filter(|permission| *permission != missing),
            );
            assert!(!has_export_permissions(&context(permissions)));
        }
    }

    #[test]
    fn export_body_drops_unknown_and_internal_storage_fields() {
        let value = allowlisted_event_body(json!({
            "message": "safe",
            "new_storage_field": "must not leak",
            "_faultkeep": {
                "normalization": [],
                "symbolication": {"status": "complete"},
                "attachments": [{"blob_key": "event/secret"}],
            }
        }))
        .unwrap();
        assert_eq!(value["message"], "safe");
        assert!(value.get("new_storage_field").is_none());
        assert!(value["processing_diagnostics"].get("attachments").is_none());
    }

    #[tokio::test]
    async fn zip64_stream_is_deterministic_ordered_and_manifest_is_last() {
        let first = render(vec![
            CapsuleEntry::json("issue.json", json!({"id": "golden"})),
            CapsuleEntry::text("README.txt", b"golden\n"),
            CapsuleEntry::json(
                "manifest.json",
                json!({"format": "incident-capsule", "version": 1}),
            ),
        ])
        .await;
        let second = render(vec![
            CapsuleEntry::json("issue.json", json!({"id": "golden"})),
            CapsuleEntry::text("README.txt", b"golden\n"),
            CapsuleEntry::json(
                "manifest.json",
                json!({"format": "incident-capsule", "version": 1}),
            ),
        ])
        .await;
        assert_eq!(first, second);
        let mut archive = zip::ZipArchive::new(Cursor::new(first)).unwrap();
        let names = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["issue.json", "README.txt", "manifest.json"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_channel_applies_backpressure_and_disconnect_cancels_writer() {
        let root = ShutdownRoot::new();
        let (sender, mut receiver) = mpsc::channel(1);
        let mut entry = CapsuleEntry::text("README.txt", &vec![b'x'; 1024 * 1024]);
        entry.compression = zip::CompressionMethod::Stored;
        let task = tokio::task::spawn_blocking(move || {
            write_archive(
                vec![entry],
                sender,
                4 * 1024,
                root.signal(),
                Instant::now() + Duration::from_secs(2),
            )
        });
        receiver.recv().await.expect("first bounded chunk").unwrap();
        drop(receiver);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap(),
            Err(IncidentCapsuleError::Cancelled)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_backpressure_is_cut_off_by_the_generation_deadline() {
        let root = ShutdownRoot::new();
        let (sender, mut receiver) = mpsc::channel(1);
        let mut entry = CapsuleEntry::text("README.txt", &vec![b'x'; 1024 * 1024]);
        entry.compression = zip::CompressionMethod::Stored;
        let task = tokio::task::spawn_blocking(move || {
            write_archive(
                vec![entry],
                sender,
                4 * 1024,
                root.signal(),
                Instant::now() + Duration::from_millis(25),
            )
        });
        receiver.recv().await.expect("first bounded chunk").unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap(),
            Err(IncidentCapsuleError::GenerationTimeout)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "Phase 19 retained Incident Capsule streaming RPS baseline"]
    async fn performance_incident_capsule_streaming_rps() {
        const SAMPLES: usize = 300;
        let payload = "x".repeat(8 * 1024);
        let started = Instant::now();
        let mut output_bytes = 0_u64;
        for _ in 0..SAMPLES {
            let mut entries = vec![
                CapsuleEntry::json("issue.json", json!({"title": "Golden issue"})),
                CapsuleEntry::json(
                    "statistics/hourly.json",
                    json!({"buckets": [{"occurrence_count": 42}]}),
                ),
                CapsuleEntry::json("activity.json", json!({"items": []})),
            ];
            for index in 0..7_u8 {
                entries.push(CapsuleEntry::json(
                    format!("events/{index:032x}.json"),
                    json!({"message": payload.as_str(), "index": index}),
                ));
            }
            entries.extend([
                CapsuleEntry::json("diagnostics/capabilities.json", capabilities_value()),
                CapsuleEntry::text("README.txt", b"Faultkeep Incident Capsule v1\n"),
                CapsuleEntry::json(
                    "manifest.json",
                    json!({"format": "incident-capsule", "version": 1}),
                ),
            ]);
            output_bytes = output_bytes.saturating_add(render(entries).await.len() as u64);
        }
        let elapsed = started.elapsed().as_secs_f64();
        let rps = SAMPLES as f64 / elapsed;
        let mib_per_second = output_bytes as f64 / (1024.0 * 1024.0) / elapsed;
        println!(
            "Phase19 Incident Capsule: samples={SAMPLES},capsule_rps={rps:.0},mib_per_second={mib_per_second:.2},fixture_events=7"
        );
        assert!(rps >= 20.0);
    }

    #[test]
    fn aggregate_and_entry_limits_fail_before_archive_streaming() {
        let config = IncidentCapsuleConfig {
            max_total_uncompressed_bytes: 2 * 1024 * 1024,
            max_entry_bytes: 1024 * 1024,
            ..IncidentCapsuleConfig::default()
        };
        let at_limit = vec![CapsuleEntry::text("README.txt", &vec![b'x'; 1024 * 1024])];
        assert!(validate_entries(&at_limit, config).is_ok());
        let over_limit = vec![CapsuleEntry::text(
            "README.txt",
            &vec![b'x'; 1024 * 1024 + 1],
        )];
        assert_eq!(
            validate_entries(&over_limit, config),
            Err(IncidentCapsuleError::LimitExceeded)
        );
    }

    async fn render(entries: Vec<CapsuleEntry>) -> Vec<u8> {
        let root = ShutdownRoot::new();
        let (sender, mut receiver) = mpsc::channel(4);
        let task = tokio::task::spawn_blocking(move || {
            write_archive(
                entries,
                sender,
                4 * 1024,
                root.signal(),
                Instant::now() + Duration::from_secs(30),
            )
        });
        let mut bytes = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            bytes.extend_from_slice(&chunk.unwrap());
        }
        task.await.unwrap().unwrap();
        bytes
    }
}
