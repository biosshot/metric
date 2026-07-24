use std::{collections::BTreeSet, time::Duration};

use faultkeep_domain::{
    AcceptedEvent, EventId, EventKey, ProjectAcceptanceState, ProjectId, ScrubbedEventPayload,
    Timestamp,
    blob::BlobKey,
    processing::{
        PendingEvent, ProcessingFailure, ProcessingFailureDisposition, ProcessingProject,
        ProcessingStateChange,
    },
};
use faultkeep_ports::{
    BacklogObservation, BlobReference, BlobReferenceStore, BlobStoreError, EventBacklog,
    EventBacklogError, EventPrepareError, EventStore, EventStoreError, EventWriteStatus,
    PortFuture, PreparedEvent, ProcessingProjectError, ProcessingProjectStore,
    ProcessingStateError, ProcessingStateStore,
};
use futures_util::TryStreamExt;
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    error::ErrorKind,
    options::{Hint, IndexOptions},
};
use serde_json::Value;
use thiserror::Error;

const BODY_FORMAT_VERSION: u8 = 1;
const BODY_CODEC_JSON: u8 = 0;
const BODY_CODEC_ZSTD: u8 = 1;
const DUPLICATE_KEY_CODE: i32 = 11000;

#[derive(Debug, Clone, Copy)]
pub struct EventCodecConfig {
    pub compression_level: i32,
    pub compression_min_savings: usize,
    pub max_decoded_body_bytes: usize,
    pub max_encoded_document_bytes: usize,
}

impl Default for EventCodecConfig {
    fn default() -> Self {
        Self {
            compression_level: 3,
            compression_min_savings: 64,
            max_decoded_body_bytes: 1024 * 1024,
            max_encoded_document_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EventCodecError {
    #[error("Event BSON uses an unknown or malformed physical format")]
    InvalidDocument,
    #[error("Event body uses an unknown or malformed codec")]
    InvalidBody,
    #[error("Event body exceeds the configured decoded size")]
    BodyTooLarge,
}

pub struct MongoPreparedEvent {
    event: AcceptedEvent,
    key: EventKey,
    document: Document,
    encoded_len: usize,
}

impl PreparedEvent for MongoPreparedEvent {
    fn key(&self) -> EventKey {
        self.key
    }

    fn encoded_len(&self) -> usize {
        self.encoded_len
    }

    fn into_event(self) -> AcceptedEvent {
        self.event
    }
}

#[derive(Clone)]
pub struct MongoEventStore {
    database: Database,
    codec: EventCodecConfig,
}

impl MongoEventStore {
    #[must_use]
    pub const fn from_database(database: Database, codec: EventCodecConfig) -> Self {
        Self { database, codec }
    }

    #[must_use]
    pub const fn codec_config(&self) -> EventCodecConfig {
        self.codec
    }

    fn prepare_inner(&self, event: AcceptedEvent) -> Result<MongoPreparedEvent, EventPrepareError> {
        prepare_event(event, self.codec)
    }
}

fn prepare_event(
    event: AcceptedEvent,
    codec: EventCodecConfig,
) -> Result<MongoPreparedEvent, EventPrepareError> {
    if event.payload.as_bytes().len() > codec.max_decoded_body_bytes {
        return Err(EventPrepareError::TooLarge);
    }
    let value: Value = serde_json::from_slice(event.payload.as_bytes())
        .map_err(|_| EventPrepareError::InvalidEvent)?;
    let payload_event_id = value
        .get("event_id")
        .and_then(Value::as_str)
        .and_then(|value| EventId::parse(value).ok())
        .ok_or(EventPrepareError::InvalidEvent)?;
    if payload_event_id != event.event_id {
        return Err(EventPrepareError::InvalidEvent);
    }
    let canonical = serde_json::to_vec(&value).map_err(|_| EventPrepareError::InvalidEvent)?;
    if canonical.len() > codec.max_decoded_body_bytes {
        return Err(EventPrepareError::TooLarge);
    }
    let body = encode_body(&canonical, codec)?;
    let key = EventKey::new(event.project_id, event.event_id);
    let received = DateTime::from_millis(event.received_at.unix_millis());
    let occurred = occurred_at(&value).unwrap_or(received);
    let mut document = doc! {
        "_id": binary(key.as_bytes()),
        "p": event.project_id.get(),
        "r": received,
        "o": occurred,
        "a": platform_code(&value),
        "q": {
            "s": 0_i32,
            "a": 0_i32,
            "n": received,
        },
        "b": Binary {
            subtype: BinarySubtype::Generic,
            bytes: body,
        },
    };
    if let Some(level) = non_default_level_code(&value) {
        document.insert("l", level);
    }
    if event.policy_revision != 1 {
        document.insert(
            "s",
            i64::try_from(event.policy_revision).map_err(|_| EventPrepareError::InvalidEvent)?,
        );
    }
    let encoded_len = mongodb::bson::to_vec(&document)
        .map_err(|_| EventPrepareError::InvalidEvent)?
        .len();
    if encoded_len > codec.max_encoded_document_bytes {
        return Err(EventPrepareError::TooLarge);
    }
    Ok(MongoPreparedEvent {
        event: AcceptedEvent {
            payload: ScrubbedEventPayload::new(canonical),
            ..event
        },
        key,
        document,
        encoded_len,
    })
}

impl EventStore for MongoEventStore {
    type Prepared = MongoPreparedEvent;

    fn prepare(&self, event: AcceptedEvent) -> Result<Self::Prepared, EventPrepareError> {
        self.prepare_inner(event)
    }

    fn insert_batch<'a>(
        &'a self,
        events: &'a [Self::Prepared],
    ) -> PortFuture<'a, Result<Vec<EventWriteStatus>, EventStoreError>> {
        Box::pin(async move {
            if events.is_empty() {
                return Ok(Vec::new());
            }
            let started = std::time::Instant::now();
            let result = self
                .database
                .collection::<Document>("error_events")
                .insert_many(events.iter().map(|event| &event.document))
                .ordered(false)
                .await;
            let classified = match result {
                Ok(_) => Ok(vec![EventWriteStatus::Inserted; events.len()]),
                Err(error) => classify_insert_many(error.kind.as_ref(), events.len()),
            };
            let outcome = match &classified {
                Ok(statuses) if statuses.contains(&EventWriteStatus::Rejected) => "partial",
                Ok(statuses) if statuses.contains(&EventWriteStatus::Duplicate) => "duplicate",
                Ok(_) => "inserted",
                Err(EventStoreError::Ambiguous) => "ambiguous",
                Err(EventStoreError::Unavailable) => "unavailable",
            };
            metrics::histogram!(
                "faultkeep_mongodb_operation_duration_seconds",
                "operation" => "event_insert_batch",
                "outcome" => outcome
            )
            .record(started.elapsed().as_secs_f64());
            classified
        })
    }
}

impl BlobReferenceStore for MongoEventStore {
    fn is_referenced(
        &self,
        reference: BlobReference,
    ) -> PortFuture<'_, Result<bool, BlobStoreError>> {
        Box::pin(async move {
            let key = EventKey::new(reference.project_id, reference.event_id);
            let Some(document) = self
                .database
                .collection::<Document>("error_events")
                .find_one(doc! {
                    "_id": binary(key.as_bytes()),
                    "p": reference.project_id.get(),
                })
                .projection(doc! { "b": 1 })
                .await
                .map_err(|_| BlobStoreError::Unavailable)?
            else {
                return Ok(false);
            };
            let body = document
                .get_binary_generic("b")
                .map_err(|_| BlobStoreError::Corrupt)?;
            let decoded = decode_body(body, self.codec.max_decoded_body_bytes)
                .map_err(|_| BlobStoreError::Corrupt)?;
            let event: Value =
                serde_json::from_slice(&decoded).map_err(|_| BlobStoreError::Corrupt)?;
            let expected = BlobKey::event_owned(
                reference.project_id,
                reference.event_id,
                reference.object_id,
            );
            let attachment_reference = event
                .get("attachments")
                .and_then(Value::as_array)
                .is_some_and(|attachments| {
                    attachments.iter().any(|attachment| {
                        attachment.get("blob_key").and_then(Value::as_str)
                            == Some(expected.as_str())
                    })
                });
            let native_reference = event
                .get("native_crash")
                .and_then(|value| value.get("blob_key"))
                .and_then(Value::as_str)
                == Some(expected.as_str());
            Ok(attachment_reference || native_reference)
        })
    }
}

impl EventBacklog for MongoEventStore {
    fn load_due<'a>(
        &'a self,
        now: Timestamp,
        limit: usize,
        excluded: &'a [EventKey],
    ) -> PortFuture<'a, Result<Vec<PendingEvent>, EventBacklogError>> {
        Box::pin(async move {
            if limit == 0 {
                return Ok(Vec::new());
            }
            let mut filter = doc! {
                "q.s": 0_i32,
                "q.n": { "$lte": DateTime::from_millis(now.unix_millis()) },
            };
            if !excluded.is_empty() {
                filter.insert(
                    "_id",
                    doc! {
                        "$nin": excluded
                            .iter()
                            .map(|key| Bson::Binary(binary(key.as_bytes())))
                            .collect::<Vec<_>>()
                    },
                );
            }
            let scan_limit = limit.saturating_mul(8).min(32_768).max(limit);
            let mut cursor = self
                .database
                .collection::<Document>("error_events")
                .find(filter)
                .sort(doc! { "q.n": 1, "r": 1, "_id": 1 })
                .limit(i64::try_from(scan_limit).map_err(|_| EventBacklogError::InvalidData)?)
                .await
                .map_err(|_| EventBacklogError::Unavailable)?;
            let mut decoded = Vec::with_capacity(scan_limit);
            let mut project_ids = BTreeSet::new();
            while let Some(document) = cursor
                .try_next()
                .await
                .map_err(|_| EventBacklogError::Unavailable)?
            {
                let event = decode_pending_event(&document, self.codec)
                    .map_err(|_| EventBacklogError::InvalidData)?;
                let attempts = u32::try_from(
                    document
                        .get_document("q")
                        .and_then(|pipeline| pipeline.get_i32("a"))
                        .map_err(|_| EventBacklogError::InvalidData)?,
                )
                .map_err(|_| EventBacklogError::InvalidData)?;
                project_ids.insert(event.project_id.get());
                decoded.push(PendingEvent { event, attempts });
            }
            if decoded.is_empty() {
                return Ok(decoded);
            }
            let mut projects = self
                .database
                .collection::<Document>("projects")
                .find(doc! {
                    "_id": { "$in": project_ids.into_iter().collect::<Vec<_>>() },
                    "state": { "$in": ["active", "disabled"] },
                })
                .projection(doc! { "_id": 1 })
                .await
                .map_err(|_| EventBacklogError::Unavailable)?;
            let mut processable = BTreeSet::new();
            while let Some(project) = projects
                .try_next()
                .await
                .map_err(|_| EventBacklogError::Unavailable)?
            {
                processable.insert(
                    project
                        .get_i32("_id")
                        .map_err(|_| EventBacklogError::InvalidData)?,
                );
            }
            Ok(decoded
                .into_iter()
                .filter(|pending| processable.contains(&pending.event.project_id.get()))
                .take(limit)
                .collect())
        })
    }

    fn observe(
        &self,
        count_limit: u64,
    ) -> PortFuture<'_, Result<BacklogObservation, EventBacklogError>> {
        Box::pin(async move {
            if count_limit == 0 {
                return Err(EventBacklogError::InvalidData);
            }
            let events = self.database.collection::<Document>("error_events");
            let pending_count = events
                .count_documents(doc! { "q.s": 0_i32 })
                .limit(count_limit)
                .hint(Hint::Name("event_pending_due".to_owned()))
                .await
                .map_err(|_| EventBacklogError::Unavailable)?;
            let oldest = events
                .find_one(doc! { "q.s": 0_i32 })
                .sort(doc! { "q.n": 1, "r": 1, "_id": 1 })
                .projection(doc! { "r": 1 })
                .await
                .map_err(|_| EventBacklogError::Unavailable)?;
            let oldest_pending_at = oldest
                .map(|document| {
                    let received = document
                        .get_datetime("r")
                        .map_err(|_| EventBacklogError::InvalidData)?;
                    Timestamp::from_unix_millis(received.timestamp_millis())
                        .map_err(|_| EventBacklogError::InvalidData)
                })
                .transpose()?;
            Ok(BacklogObservation {
                pending_count,
                oldest_pending_at,
            })
        })
    }
}

impl ProcessingProjectStore for MongoEventStore {
    fn load_processing_project(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<ProcessingProject, ProcessingProjectError>> {
        Box::pin(async move {
            let document = self
                .database
                .collection::<Document>("projects")
                .find_one(doc! { "_id": project_id.get() })
                .projection(doc! { "state": 1, "items.error": 1, "grouping_revision": 1 })
                .await
                .map_err(|_| ProcessingProjectError::Unavailable)?
                .ok_or(ProcessingProjectError::NotFound)?;
            let state = match document
                .get_str("state")
                .map_err(|_| ProcessingProjectError::InvalidData)?
            {
                "active" => ProjectAcceptanceState::Active,
                "disabled" => ProjectAcceptanceState::Disabled,
                "pending_delete" => ProjectAcceptanceState::PendingDelete,
                "purging" => ProjectAcceptanceState::Purging,
                "deleted" => ProjectAcceptanceState::Deleted,
                _ => return Err(ProcessingProjectError::InvalidData),
            };
            let error_events_enabled = document
                .get_document("items")
                .and_then(|items| items.get_bool("error"))
                .map_err(|_| ProcessingProjectError::InvalidData)?;
            let grouping_revision = u64::try_from(
                document
                    .get_i64("grouping_revision")
                    .map_err(|_| ProcessingProjectError::InvalidData)?,
            )
            .map_err(|_| ProcessingProjectError::InvalidData)?;
            let debug_file_revision = match document.get_i64("dr") {
                Ok(value) => {
                    u64::try_from(value).map_err(|_| ProcessingProjectError::InvalidData)?
                }
                Err(_) if !document.contains_key("dr") => 0,
                Err(_) => return Err(ProcessingProjectError::InvalidData),
            };
            let artifact_revision = match document.get_i64("ar") {
                Ok(value) => {
                    u64::try_from(value).map_err(|_| ProcessingProjectError::InvalidData)?
                }
                Err(_) if !document.contains_key("ar") => 0,
                Err(_) => return Err(ProcessingProjectError::InvalidData),
            };
            Ok(ProcessingProject {
                project_id,
                state,
                error_events_enabled,
                grouping_revision,
                debug_file_revision,
                artifact_revision,
            })
        })
    }
}

impl ProcessingStateStore for MongoEventStore {
    fn record_processing_failure(
        &self,
        failure: ProcessingFailure,
    ) -> PortFuture<'_, Result<ProcessingStateChange, ProcessingStateError>> {
        Box::pin(async move {
            let expected = i32::try_from(failure.expected_attempts)
                .map_err(|_| ProcessingStateError::InvalidData)?;
            let next = i32::try_from(failure.new_attempts)
                .map_err(|_| ProcessingStateError::InvalidData)?;
            if failure.new_attempts != failure.expected_attempts.saturating_add(1) {
                return Err(ProcessingStateError::InvalidData);
            }
            let (set, unset) = match failure.disposition {
                ProcessingFailureDisposition::RetryAt(at) => (
                    doc! {
                        "q.s": 0_i32,
                        "q.a": next,
                        "q.n": DateTime::from_millis(at.unix_millis()),
                        "q.c": failure.code.stored(),
                    },
                    None,
                ),
                ProcessingFailureDisposition::PermanentlyFailed => (
                    doc! {
                        "q.s": 1_i32,
                        "q.a": next,
                        "q.c": failure.code.stored(),
                    },
                    Some(doc! { "q.n": "" }),
                ),
            };
            let mut update = doc! { "$set": set };
            if let Some(unset) = unset {
                update.insert("$unset", unset);
            }
            let result = self
                .database
                .collection::<Document>("error_events")
                .update_one(
                    doc! {
                        "_id": binary(failure.key.as_bytes()),
                        "p": failure.key.project_id().get(),
                        "q.s": 0_i32,
                        "q.a": expected,
                    },
                    update,
                )
                .await
                .map_err(|_| ProcessingStateError::Unavailable)?;
            Ok(if result.matched_count == 1 {
                ProcessingStateChange::Updated
            } else {
                ProcessingStateChange::StaleOrCompleted
            })
        })
    }
}

fn classify_insert_many(
    error: &ErrorKind,
    count: usize,
) -> Result<Vec<EventWriteStatus>, EventStoreError> {
    let ErrorKind::InsertMany(failure) = error else {
        return Err(EventStoreError::Ambiguous);
    };
    if failure.write_concern_error.is_some() {
        return Err(EventStoreError::Ambiguous);
    }
    let errors = failure
        .write_errors
        .as_ref()
        .ok_or(EventStoreError::Ambiguous)?;
    let mut statuses = vec![EventWriteStatus::Inserted; count];
    for error in errors {
        let status = statuses
            .get_mut(error.index)
            .ok_or(EventStoreError::Ambiguous)?;
        *status = if error.code == DUPLICATE_KEY_CODE {
            EventWriteStatus::Duplicate
        } else {
            EventWriteStatus::Rejected
        };
    }
    Ok(statuses)
}

pub(crate) fn encode_body(
    canonical: &[u8],
    config: EventCodecConfig,
) -> Result<Vec<u8>, EventPrepareError> {
    let compressed = zstd::bulk::compress(canonical, config.compression_level)
        .map_err(|_| EventPrepareError::InvalidEvent)?;
    let use_compressed = compressed
        .len()
        .saturating_add(config.compression_min_savings)
        <= canonical.len();
    let (codec, bytes) = if use_compressed {
        (BODY_CODEC_ZSTD, compressed.as_slice())
    } else {
        (BODY_CODEC_JSON, canonical)
    };
    let mut body = Vec::with_capacity(bytes.len() + 2);
    body.extend_from_slice(&[BODY_FORMAT_VERSION, codec]);
    body.extend_from_slice(bytes);
    Ok(body)
}

pub(crate) fn decode_body(body: &[u8], max_size: usize) -> Result<Vec<u8>, EventCodecError> {
    let (&version, rest) = body.split_first().ok_or(EventCodecError::InvalidBody)?;
    let (&codec, payload) = rest.split_first().ok_or(EventCodecError::InvalidBody)?;
    if version != BODY_FORMAT_VERSION {
        return Err(EventCodecError::InvalidBody);
    }
    let decoded = match codec {
        BODY_CODEC_JSON => {
            if payload.len() > max_size {
                return Err(EventCodecError::BodyTooLarge);
            }
            payload.to_vec()
        }
        BODY_CODEC_ZSTD => {
            zstd::bulk::decompress(payload, max_size).map_err(|_| EventCodecError::InvalidBody)?
        }
        _ => return Err(EventCodecError::InvalidBody),
    };
    let value: Value =
        serde_json::from_slice(&decoded).map_err(|_| EventCodecError::InvalidBody)?;
    let canonical = serde_json::to_vec(&value).map_err(|_| EventCodecError::InvalidBody)?;
    if canonical != decoded {
        return Err(EventCodecError::InvalidBody);
    }
    Ok(decoded)
}

pub fn decode_pending_event(
    document: &Document,
    config: EventCodecConfig,
) -> Result<AcceptedEvent, EventCodecError> {
    if document.contains_key("v") {
        return Err(EventCodecError::InvalidDocument);
    }
    let key_bytes = document
        .get_binary_generic("_id")
        .map_err(|_| EventCodecError::InvalidDocument)?;
    let key = EventKey::from_bytes(
        key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| EventCodecError::InvalidDocument)?,
    )
    .map_err(|_| EventCodecError::InvalidDocument)?;
    let project_id = ProjectId::new(
        document
            .get_i32("p")
            .map_err(|_| EventCodecError::InvalidDocument)?,
    )
    .map_err(|_| EventCodecError::InvalidDocument)?;
    if key.project_id() != project_id {
        return Err(EventCodecError::InvalidDocument);
    }
    let received_at = Timestamp::from_unix_millis(
        document
            .get_datetime("r")
            .map_err(|_| EventCodecError::InvalidDocument)?
            .timestamp_millis(),
    )
    .map_err(|_| EventCodecError::InvalidDocument)?;
    let pipeline = document
        .get_document("q")
        .map_err(|_| EventCodecError::InvalidDocument)?;
    let pending = pipeline.get_i32("s") == Ok(0)
        && pipeline.get_i32("a").is_ok_and(|attempts| attempts >= 0)
        && pipeline.get_datetime("n").is_ok();
    if !pending {
        return Err(EventCodecError::InvalidDocument);
    }
    let body = document
        .get_binary_generic("b")
        .map_err(|_| EventCodecError::InvalidDocument)?;
    let payload = decode_body(body, config.max_decoded_body_bytes)?;
    let policy_revision = match document.get("s") {
        None => 1,
        Some(Bson::Int64(value)) => {
            u64::try_from(*value).map_err(|_| EventCodecError::InvalidDocument)?
        }
        _ => return Err(EventCodecError::InvalidDocument),
    };
    Ok(AcceptedEvent {
        project_id,
        event_id: key.event_id(),
        received_at,
        policy_revision,
        payload: ScrubbedEventPayload::new(payload),
    })
}

fn occurred_at(value: &Value) -> Option<DateTime> {
    let timestamp = value.get("timestamp")?;
    if let Some(value) = timestamp.as_str() {
        return DateTime::parse_rfc3339_str(value).ok();
    }
    let seconds = timestamp.as_f64()?;
    if !seconds.is_finite() {
        return None;
    }
    let millis = seconds * 1_000.0;
    (millis >= i64::MIN as f64 && millis <= i64::MAX as f64)
        .then(|| DateTime::from_millis(millis.round() as i64))
}

fn platform_code(value: &Value) -> i32 {
    match value.get("platform").and_then(Value::as_str) {
        Some("python") => 1,
        Some("javascript" | "node") => 2,
        Some("native" | "c" | "cocoa") => 3,
        Some("java" | "android") => 4,
        Some("php") => 5,
        Some("ruby") => 6,
        Some("csharp" | "dotnet") => 7,
        Some("go") => 8,
        Some("rust") => 9,
        _ => 0,
    }
}

fn non_default_level_code(value: &Value) -> Option<i32> {
    match value.get("level").and_then(Value::as_str) {
        Some("debug") => Some(1),
        Some("info") => Some(2),
        Some("warning") => Some(3),
        Some("fatal") => Some(4),
        _ => None,
    }
}

fn binary<const N: usize>(bytes: [u8; N]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

pub(crate) fn event_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "p", "r", "o", "a", "b"],
            "oneOf": [
                { "required": ["q"], "not": { "anyOf": [
                    { "required": ["u"] },
                    { "required": ["x"] },
                    { "required": ["z"] },
                ] } },
                { "required": ["q", "x", "z"], "not": { "anyOf": [
                    { "required": ["u"] },
                    { "required": ["h"] },
                ] } },
                { "required": ["u", "x"], "not": { "anyOf": [
                    { "required": ["q"] },
                    { "required": ["h"] },
                ] } },
                { "required": ["u", "h"], "not": { "anyOf": [
                    { "required": ["q"] },
                    { "required": ["x"] },
                    { "required": ["z"] },
                ] } },
            ],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "p": { "bsonType": "int", "minimum": 1 },
                "r": { "bsonType": "date" },
                "o": { "bsonType": "date" },
                "x": { "bsonType": "date" },
                "h": { "bsonType": "date" },
                "z": { "bsonType": "binData" },
                "u": { "bsonType": "binData" },
                "g": { "bsonType": "binData" },
                "n": { "bsonType": "binData" },
                "l": { "bsonType": "int", "minimum": 1, "maximum": 4 },
                "a": { "bsonType": "int", "minimum": 0, "maximum": 9 },
                "s": { "bsonType": "long", "minimum": 1 },
                "q": {
                    "bsonType": "object",
                    "required": ["s", "a"],
                    "additionalProperties": false,
                    "properties": {
                        "s": { "bsonType": "int", "enum": [0, 1] },
                        "a": { "bsonType": "int", "minimum": 0 },
                        "n": { "bsonType": "date" },
                        "c": { "bsonType": "int", "minimum": 1 },
                    },
                },
                "k": {
                    "bsonType": "array",
                    "items": { "bsonType": "long" },
                    "maxItems": 16,
                },
                "b": { "bsonType": "binData" },
                "v": { "bsonType": "int", "minimum": 2 },
            },
        },
    }
}

pub(crate) fn event_index_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "_id_",
        "event_archive_due",
        "event_expiration",
        "event_issue_timeline",
        "event_project_trace",
        "event_pending_due",
        "event_project_timeline",
        "event_search_tokens",
    ])
}

pub(crate) async fn create_event_indexes(database: &Database) -> Result<(), mongodb::error::Error> {
    let collection = database.collection::<Document>("error_events");
    for model in event_indexes() {
        collection.create_index(model).await?;
    }
    Ok(())
}

pub(crate) async fn validate_event_indexes(
    database: &Database,
) -> Result<bool, mongodb::error::Error> {
    let mut expected = event_indexes()
        .into_iter()
        .map(|model| {
            let name = model
                .options
                .as_ref()
                .and_then(|options| options.name.clone())
                .expect("owned Event index has a name");
            (name, model)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut indexes = database
        .collection::<Document>("error_events")
        .list_indexes()
        .await?;
    while let Some(actual) = indexes.try_next().await? {
        let Some(options) = actual.options.as_ref() else {
            return Ok(false);
        };
        let Some(name) = options.name.as_deref() else {
            return Ok(false);
        };
        if name == "_id_" {
            continue;
        }
        let Some(expected_model) = expected.remove(name) else {
            return Ok(false);
        };
        let expected_options = expected_model
            .options
            .as_ref()
            .expect("owned Event index has options");
        let compatible = actual.keys == expected_model.keys
            && options.partial_filter_expression == expected_options.partial_filter_expression
            && options.expire_after == expected_options.expire_after;
        if !compatible {
            return Ok(false);
        }
    }
    Ok(expected.is_empty())
}

fn event_indexes() -> [IndexModel; 7] {
    [
        event_index(
            doc! { "q.n": 1, "r": 1, "_id": 1 },
            "event_pending_due",
            Some(doc! { "q.s": 0 }),
            None,
        ),
        event_index(
            doc! { "p": 1, "o": -1, "_id": -1 },
            "event_project_timeline",
            None,
            None,
        ),
        event_index(
            doc! { "p": 1, "u": 1, "o": -1, "_id": -1 },
            "event_issue_timeline",
            None,
            None,
        ),
        event_index(
            doc! { "p": 1, "k": 1, "o": -1, "_id": -1 },
            "event_search_tokens",
            Some(doc! { "k.0": { "$exists": true } }),
            None,
        ),
        event_index(
            doc! { "p": 1, "g": 1, "o": 1 },
            "event_project_trace",
            Some(doc! { "g": { "$exists": true } }),
            None,
        ),
        event_index(
            doc! { "x": 1 },
            "event_expiration",
            None,
            Some(Duration::ZERO),
        ),
        event_index(
            doc! { "h": 1, "_id": 1 },
            "event_archive_due",
            Some(doc! { "h": { "$exists": true } }),
            None,
        ),
    ]
}

fn event_index(
    keys: Document,
    name: &str,
    partial_filter_expression: Option<Document>,
    expire_after: Option<Duration>,
) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .partial_filter_expression(partial_filter_expression)
                .expire_after(expire_after)
                .build(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use faultkeep_domain::ProjectId;

    fn event(payload: &[u8]) -> AcceptedEvent {
        let value: Value = serde_json::from_slice(payload).unwrap();
        let event_id = EventId::parse(value["event_id"].as_str().unwrap()).unwrap();
        AcceptedEvent {
            project_id: ProjectId::new(0x0102_0304).unwrap(),
            event_id,
            received_at: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
            policy_revision: 1,
            payload: ScrubbedEventPayload::new(payload),
        }
    }

    #[test]
    fn payload_event_id_must_match_the_storage_key() {
        let mut mismatched = event(br#"{"event_id":"05050505050505050505050505050505"}"#);
        mismatched.event_id = EventId::from_bytes([6; 16]);
        assert!(matches!(
            prepare_event(mismatched, EventCodecConfig::default()),
            Err(EventPrepareError::InvalidEvent)
        ));
    }

    #[test]
    fn pending_codec_round_trips_and_has_canonical_composite_id() {
        let prepared = prepare_event(
            event(br#"{"z":1,"event_id":"05050505050505050505050505050505","a":2}"#),
            EventCodecConfig::default(),
        )
        .unwrap();
        assert_eq!(&prepared.key().as_bytes()[..4], &[1, 2, 3, 4]);
        let decoded =
            decode_pending_event(&prepared.document, EventCodecConfig::default()).unwrap();
        assert_eq!(decoded, prepared.event);
        assert!(prepared.encoded_len() < 512);
    }

    #[test]
    fn adaptive_body_codec_and_malformed_inputs_fail_closed() {
        let large = format!(
            r#"{{"event_id":"{}","message":"{}"}}"#,
            "05".repeat(16),
            "x".repeat(4_096)
        );
        let prepared = prepare_event(event(large.as_bytes()), EventCodecConfig::default()).unwrap();
        let body = prepared.document.get_binary_generic("b").unwrap();
        assert_eq!(&body[..2], &[BODY_FORMAT_VERSION, BODY_CODEC_ZSTD]);

        let mut malformed = prepared.document.clone();
        malformed.insert("b", binary([99, 0]));
        assert_eq!(
            decode_pending_event(&malformed, EventCodecConfig::default()),
            Err(EventCodecError::InvalidBody)
        );
        malformed = prepared.document.clone();
        malformed.insert("p", 7_i32);
        assert_eq!(
            decode_pending_event(&malformed, EventCodecConfig::default()),
            Err(EventCodecError::InvalidDocument)
        );

        let mut retry = prepared.document.clone();
        retry.insert(
            "q",
            doc! {
                "s": 0_i32,
                "a": 3_i32,
                "n": DateTime::from_millis(1_700_000_010_000),
            },
        );
        assert!(decode_pending_event(&retry, EventCodecConfig::default()).is_ok());
    }

    #[test]
    fn golden_pending_bson_byte_sizes_are_bounded() {
        let python = include_str!("../../server/tests/fixtures/python-2.32.0-error-event-v1.json");
        let compact = prepare_event(event(python.as_bytes()), EventCodecConfig::default()).unwrap();
        let large = format!(
            r#"{{"event_id":"{}","platform":"rust","message":"{}"}}"#,
            "05".repeat(16),
            "bounded-frame;".repeat(8_000)
        );
        let large = prepare_event(event(large.as_bytes()), EventCodecConfig::default()).unwrap();
        assert_eq!(compact.encoded_len(), 628);
        assert_eq!(large.encoded_len(), 200);
    }
}
