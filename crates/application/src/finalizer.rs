//! Bounded successful-processing batch preparation and durable handoff.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use faultkeep_domain::{
    event::{CanonicalValue, NormalizedEvent},
    finalization::{
        FinalizationPolicy, FinalizeBatch, FinalizeEvent, FinalizeResult,
        MAX_SEARCH_TOKENS_PER_EVENT, ProcessedEventPayload, SearchToken,
    },
    grouping::GroupingResult,
    symbolication::{RawTraceOrigin, SymbolicatedFrame, SymbolicationResult},
};
use faultkeep_ports::{FinalizationStore, FinalizationStoreError};
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::{issues::prepare_issue_occurrence, normalizer::canonical_body_value};

const HARD_MAX_BATCH_EVENTS: usize = 10_000;
const HARD_MAX_PROCESSED_BODY_BYTES: usize = 4 * 1024 * 1024;
const HARD_MAX_RETENTION: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

#[derive(Debug, Clone, Copy)]
pub struct FinalizerConfig {
    pub max_batch_events: usize,
    pub max_processed_body_bytes: usize,
    pub event_retention: Duration,
    pub hourly_retention: Duration,
    pub max_implicit_releases_per_project_day: u32,
    pub max_implicit_environments_per_project: u32,
}

impl Default for FinalizerConfig {
    fn default() -> Self {
        Self {
            max_batch_events: 256,
            max_processed_body_bytes: 2 * 1024 * 1024,
            event_retention: Duration::from_secs(30 * 24 * 60 * 60),
            hourly_retention: Duration::from_secs(400 * 24 * 60 * 60),
            max_implicit_releases_per_project_day: 1_000,
            max_implicit_environments_per_project: 100,
        }
    }
}

impl FinalizerConfig {
    pub fn validate(self) -> Result<Self, FinalizerError> {
        let valid = (1..=HARD_MAX_BATCH_EVENTS).contains(&self.max_batch_events)
            && (1..=HARD_MAX_PROCESSED_BODY_BYTES).contains(&self.max_processed_body_bytes)
            && !self.event_retention.is_zero()
            && self.event_retention <= HARD_MAX_RETENTION
            && !self.hourly_retention.is_zero()
            && self.hourly_retention <= HARD_MAX_RETENTION
            && self.max_implicit_releases_per_project_day > 0
            && self.max_implicit_environments_per_project > 0;
        valid.then_some(self).ok_or(FinalizerError::InvalidConfig)
    }

    #[must_use]
    pub const fn policy(self) -> FinalizationPolicy {
        FinalizationPolicy {
            event_retention: self.event_retention,
            hourly_retention: self.hourly_retention,
            max_implicit_releases_per_project_day: self.max_implicit_releases_per_project_day,
            max_implicit_environments_per_project: self.max_implicit_environments_per_project,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FinalizerError {
    #[error("Finalizer configuration is invalid")]
    InvalidConfig,
    #[error("FinalizeBatch is empty or exceeds its event bound")]
    InvalidBatch,
    #[error("FinalizeBatch contains duplicate Event identities")]
    DuplicateEvent,
    #[error("processed Event identity or Issue grouping is inconsistent")]
    InvalidIdentity,
    #[error("processed Event body or Search projection exceeds its bound")]
    OutputTooLarge,
    #[error("durable finalization identity collides with stored data")]
    IdentityCollision,
    #[error("durable finalization is temporarily unavailable")]
    Unavailable,
}

pub struct Finalizer {
    store: Arc<dyn FinalizationStore>,
    config: FinalizerConfig,
}

impl Finalizer {
    pub fn new(
        store: Arc<dyn FinalizationStore>,
        config: FinalizerConfig,
    ) -> Result<Self, FinalizerError> {
        Ok(Self {
            store,
            config: config.validate()?,
        })
    }

    pub fn prepare(
        &self,
        event: &NormalizedEvent,
        symbolication: &SymbolicationResult,
        grouping: &GroupingResult,
    ) -> Result<FinalizeEvent, FinalizerError> {
        let issue = prepare_issue_occurrence(event, grouping)
            .map_err(|_| FinalizerError::InvalidIdentity)?;
        let payload = processed_payload(event, symbolication)?;
        if payload.len() > self.config.max_processed_body_bytes {
            return Err(FinalizerError::OutputTooLarge);
        }
        let search_tokens = search_tokens(event);
        if search_tokens.len() > MAX_SEARCH_TOKENS_PER_EVENT {
            return Err(FinalizerError::OutputTooLarge);
        }
        Ok(FinalizeEvent {
            project_id: event.project_id,
            event_id: event.event_id,
            received_at: event.received_at,
            occurred_at: event.body.occurred_at,
            level: event.body.level,
            platform: event.body.platform.clone(),
            issue,
            environment: event.body.environment.clone(),
            search_tokens,
            payload: ProcessedEventPayload::new(payload),
        })
    }

    pub async fn finalize(
        &self,
        events: Vec<FinalizeEvent>,
    ) -> Result<FinalizeResult, FinalizerError> {
        if events.is_empty() || events.len() > self.config.max_batch_events {
            return Err(FinalizerError::InvalidBatch);
        }
        let mut keys = BTreeSet::new();
        for event in &events {
            if !keys.insert(event.key()) {
                return Err(FinalizerError::DuplicateEvent);
            }
            validate_event(event, self.config.max_processed_body_bytes)?;
        }
        self.store
            .finalize(FinalizeBatch { events }, self.config.policy())
            .await
            .map_err(map_store_error)
    }
}

fn validate_event(event: &FinalizeEvent, max_body_bytes: usize) -> Result<(), FinalizerError> {
    let identity_matches = event.project_id == event.issue.project_id
        && event.event_id == event.issue.event_id
        && event.received_at == event.issue.received_at
        && event.occurred_at == event.issue.occurred_at;
    if !identity_matches {
        return Err(FinalizerError::InvalidIdentity);
    }
    if event.payload.as_bytes().len() > max_body_bytes
        || event.search_tokens.len() > MAX_SEARCH_TOKENS_PER_EVENT
    {
        return Err(FinalizerError::OutputTooLarge);
    }
    let mut tokens = BTreeSet::new();
    if event
        .search_tokens
        .iter()
        .any(|token| !tokens.insert(*token))
    {
        return Err(FinalizerError::InvalidIdentity);
    }
    Ok(())
}

fn processed_payload(
    event: &NormalizedEvent,
    symbolication: &SymbolicationResult,
) -> Result<Vec<u8>, FinalizerError> {
    let Value::Object(mut root) = canonical_body_value(&event.body) else {
        return Err(FinalizerError::InvalidIdentity);
    };
    let normalization = event
        .diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code.as_str(),
                "path": diagnostic.path,
            })
        })
        .collect::<Vec<_>>();
    let derived = symbolication
        .derived
        .iter()
        .map(|trace| {
            json!({
                "origin": origin_value(trace.origin),
                "frames": trace.frames.iter().map(frame_value).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let symbolication_diagnostics = symbolication
        .diagnostics
        .iter()
        .map(|diagnostic| Value::String(diagnostic.as_str().to_owned()))
        .collect::<Vec<_>>();
    root.insert(
        "_faultkeep".to_owned(),
        json!({
            "normalization": normalization,
            "symbolication": {
                "kind": symbolication.kind.as_str(),
                "status": symbolication.status.as_str(),
                "disposition": symbolication.disposition.as_str(),
                "derived": derived,
                "missing_debug_ids": symbolication.missing_debug_ids,
                "diagnostics": symbolication_diagnostics,
            },
        }),
    );
    serde_json::to_vec(&Value::Object(root)).map_err(|_| FinalizerError::InvalidIdentity)
}

fn origin_value(origin: RawTraceOrigin) -> Value {
    match origin {
        RawTraceOrigin::Event => json!({ "kind": "event" }),
        RawTraceOrigin::Exception { index } => json!({ "kind": "exception", "index": index }),
        RawTraceOrigin::ExceptionRaw { index } => {
            json!({ "kind": "exception_raw", "index": index })
        }
    }
}

fn frame_value(frame: &SymbolicatedFrame) -> Value {
    let mut value = Map::new();
    value.insert("original_index".to_owned(), json!(frame.original_index));
    optional_string(&mut value, "function", frame.function.as_deref());
    optional_string(&mut value, "filename", frame.filename.as_deref());
    optional_string(&mut value, "module", frame.module.as_deref());
    if let Some(line) = frame.line {
        value.insert("line".to_owned(), json!(line));
    }
    if let Some(column) = frame.column {
        value.insert("column".to_owned(), json!(column));
    }
    Value::Object(value)
}

fn optional_string(target: &mut Map<String, Value>, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        target.insert(name.to_owned(), Value::String(value.to_owned()));
    }
}

fn search_tokens(event: &NormalizedEvent) -> Vec<SearchToken> {
    let mut tokens = BTreeSet::new();
    if let Some(release) = event.body.release.as_deref() {
        tokens.insert(SearchToken::release(release));
    }
    if let Some(environment) = event.body.environment.as_deref() {
        tokens.insert(SearchToken::environment(environment));
    }
    if let Some(user_id) = event.body.user.as_ref().and_then(user_id) {
        tokens.insert(SearchToken::user_id(user_id));
    }
    tokens.into_iter().collect()
}

fn user_id(user: &CanonicalValue) -> Option<&str> {
    let CanonicalValue::Object(fields) = user else {
        return None;
    };
    match fields.get("id")? {
        CanonicalValue::String(value) | CanonicalValue::Number(value) => Some(value),
        _ => None,
    }
}

const fn map_store_error(error: FinalizationStoreError) -> FinalizerError {
    match error {
        FinalizationStoreError::InvalidData => FinalizerError::InvalidIdentity,
        FinalizationStoreError::IdentityCollision => FinalizerError::IdentityCollision,
        FinalizationStoreError::Unavailable => FinalizerError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use faultkeep_domain::{
        EventId, EventKey, ProjectId, Timestamp,
        event::{EventLevel, EventPlatform, NormalizedEventBody},
        finalization::FinalizeResult,
        grouping::group,
        symbolication::{SymbolicationDisposition, SymbolicationKind, SymbolicationStatus},
    };
    use faultkeep_ports::PortFuture;

    use super::*;

    #[derive(Default)]
    struct FakeFinalizationStore {
        batch: Mutex<Option<FinalizeBatch>>,
    }

    impl FinalizationStore for FakeFinalizationStore {
        fn finalize(
            &self,
            batch: FinalizeBatch,
            _policy: FinalizationPolicy,
        ) -> PortFuture<'_, Result<FinalizeResult, FinalizationStoreError>> {
            Box::pin(async move {
                let requested = batch.events.len();
                *self.batch.lock().unwrap() = Some(batch);
                Ok(FinalizeResult {
                    requested,
                    pending: requested,
                    finalized: requested,
                    skipped_completed: 0,
                })
            })
        }
    }

    fn event() -> NormalizedEvent {
        NormalizedEvent {
            project_id: ProjectId::new(7).unwrap(),
            event_id: EventId::from_bytes([4; 16]),
            received_at: Timestamp::from_unix_millis(2_000).unwrap(),
            policy_revision: 1,
            body: NormalizedEventBody {
                occurred_at: Timestamp::from_unix_millis(1_000).unwrap(),
                platform: EventPlatform::Rust,
                level: EventLevel::Error,
                logger: Some("faultkeep".into()),
                message: Some("failure".into()),
                transaction: None,
                release: Some("backend@1.0".into()),
                dist: None,
                environment: Some("production".into()),
                fingerprint: Vec::new(),
                exceptions: Vec::new(),
                stacktrace: Vec::new(),
                tags: Vec::new(),
                request: None,
                user: Some(CanonicalValue::Object(std::collections::BTreeMap::from([
                    ("id".into(), CanonicalValue::String("user-7".into())),
                ]))),
                contexts: Default::default(),
                breadcrumbs: Vec::new(),
                unknown: Default::default(),
            },
            diagnostics: Vec::new(),
        }
    }

    fn symbolication() -> SymbolicationResult {
        SymbolicationResult {
            kind: SymbolicationKind::NotRequired,
            status: SymbolicationStatus::NotRequired,
            disposition: SymbolicationDisposition::Continue,
            raw: Vec::new(),
            derived: Vec::new(),
            missing_debug_ids: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[tokio::test]
    async fn prepares_canonical_derived_body_and_bounded_search_tokens() {
        let store = Arc::new(FakeFinalizationStore::default());
        let finalizer = Finalizer::new(store.clone(), FinalizerConfig::default()).unwrap();
        let event = event();
        let grouping = group(event.project_id, 1, &event.body, None).unwrap();
        let prepared = finalizer
            .prepare(&event, &symbolication(), &grouping)
            .unwrap();
        assert_eq!(prepared.search_tokens.len(), 3);
        let body: Value = serde_json::from_slice(prepared.payload.as_bytes()).unwrap();
        assert_eq!(
            body["_faultkeep"]["symbolication"]["status"],
            "not_required"
        );
        let result = finalizer.finalize(vec![prepared]).await.unwrap();
        assert_eq!(result.finalized, 1);
        assert_eq!(
            store.batch.lock().unwrap().as_ref().unwrap().events.len(),
            1
        );
    }

    #[tokio::test]
    async fn duplicate_event_and_invalid_limits_fail_before_storage() {
        let store = Arc::new(FakeFinalizationStore::default());
        let finalizer = Finalizer::new(store, FinalizerConfig::default()).unwrap();
        let event = event();
        let grouping = group(event.project_id, 1, &event.body, None).unwrap();
        let prepared = finalizer
            .prepare(&event, &symbolication(), &grouping)
            .unwrap();
        assert_eq!(
            finalizer.finalize(vec![prepared.clone(), prepared]).await,
            Err(FinalizerError::DuplicateEvent)
        );
        assert!(
            FinalizerConfig {
                max_batch_events: 0,
                ..FinalizerConfig::default()
            }
            .validate()
            .is_err()
        );
        assert_eq!(
            EventKey::new(event.project_id, event.event_id).project_id(),
            event.project_id
        );
    }
}
