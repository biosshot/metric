use crate::{PostgresDatabase, PostgresError};
use metric_domain::{
    AcceptedEvent, EventId, EventKey, ProjectAcceptanceState, ProjectId, ScrubbedEventPayload,
    Timestamp,
    blob::BlobKey,
    processing::{
        PendingEvent, ProcessingFailure, ProcessingFailureDisposition, ProcessingProject,
        ProcessingStateChange,
    },
};
use metric_ports::{
    BacklogObservation, BlobReference, BlobReferenceStore, BlobStoreError, EventBacklog,
    EventBacklogError, EventPrepareError, EventStore, EventStoreError, EventWriteStatus,
    PortFuture, PreparedEvent, ProcessingProjectError, ProcessingProjectStore,
    ProcessingStateError, ProcessingStateStore,
};
use serde_json::Value;
use sqlx::{Row, postgres::PgRow};
use std::collections::{BTreeMap, BTreeSet};

// Matches EventWriter's maximum document count. Arrays keep parameter count constant.
const MAX_BATCH: usize = 500;
const MAX_BACKLOG_PAGE: usize = 32_768;
// Dispatcher permits 100,000 queued keys plus 4,096 running workers. This
// bound is deliberately independent of its smaller maximum refill page.
const MAX_BACKLOG_EXCLUSIONS: usize = 104_096;
const ROW_METADATA_BYTES: usize = 96;
const MAX_BLOB_REFERENCES: usize = 1024;

/// Adapter physical encoding settings; no SQLx type reaches the application.
#[derive(Debug, Clone, Copy)]
pub struct EventCodecConfig {
    pub compression_level: i32,
    pub compression_min_savings: usize,
    pub max_decoded_body_bytes: usize,
    pub max_encoded_row_bytes: usize,
}

impl Default for EventCodecConfig {
    fn default() -> Self {
        Self {
            compression_level: 3,
            compression_min_savings: 64,
            max_decoded_body_bytes: 1024 * 1024,
            max_encoded_row_bytes: 2 * 1024 * 1024,
        }
    }
}

pub struct PostgresPreparedEvent {
    event: AcceptedEvent,
    body: Vec<u8>,
    references: Vec<BlobKey>,
}

impl PreparedEvent for PostgresPreparedEvent {
    fn key(&self) -> EventKey {
        EventKey::new(self.event.project_id, self.event.event_id)
    }
    fn encoded_len(&self) -> usize {
        self.references.iter().fold(
            self.body.len().saturating_add(ROW_METADATA_BYTES),
            |bytes, key| bytes.saturating_add(64).saturating_add(key.as_str().len()),
        )
    }
    fn into_event(self) -> AcceptedEvent {
        self.event
    }
}

#[derive(Clone)]
pub struct PostgresEventStore {
    database: PostgresDatabase,
    codec: EventCodecConfig,
}

impl PostgresEventStore {
    pub fn new(database: PostgresDatabase, codec: EventCodecConfig) -> Result<Self, PostgresError> {
        if !(-7..=22).contains(&codec.compression_level)
            || codec.max_decoded_body_bytes == 0
            || codec.max_encoded_row_bytes <= ROW_METADATA_BYTES + 2
        {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self { database, codec })
    }
}

impl EventStore for PostgresEventStore {
    type Prepared = PostgresPreparedEvent;

    fn prepare(&self, mut event: AcceptedEvent) -> Result<Self::Prepared, EventPrepareError> {
        if event.payload.as_bytes().len() > self.codec.max_decoded_body_bytes {
            return Err(EventPrepareError::TooLarge);
        }
        if event.policy_revision == 0 || i64::try_from(event.policy_revision).is_err() {
            return Err(EventPrepareError::InvalidEvent);
        }
        let value: Value = serde_json::from_slice(event.payload.as_bytes())
            .map_err(|_| EventPrepareError::InvalidEvent)?;
        validate_identity(&value, event.event_id)?;
        let canonical = serde_json::to_vec(&value).map_err(|_| EventPrepareError::InvalidEvent)?;
        if canonical.len() > self.codec.max_decoded_body_bytes {
            return Err(EventPrepareError::TooLarge);
        }
        let references = owned_references(&value, EventKey::new(event.project_id, event.event_id))?;
        let compressed = zstd::bulk::compress(&canonical, self.codec.compression_level)
            .map_err(|_| EventPrepareError::InvalidEvent)?;
        let compressed_wins = compressed
            .len()
            .saturating_add(self.codec.compression_min_savings)
            <= canonical.len();
        // PostgreSQL-owned immutable version 1: canonical JSON (0), or bounded zstd (1).
        let bytes = if compressed_wins {
            &compressed
        } else {
            &canonical
        };
        let mut body = Vec::with_capacity(bytes.len() + 2);
        body.extend_from_slice(&[1, u8::from(compressed_wins)]);
        body.extend_from_slice(bytes);
        event.payload = ScrubbedEventPayload::new(canonical);
        let prepared = PostgresPreparedEvent {
            event,
            body,
            references,
        };
        if prepared.encoded_len() > self.codec.max_encoded_row_bytes {
            return Err(EventPrepareError::TooLarge);
        }
        Ok(prepared)
    }

    fn insert_batch<'a>(
        &'a self,
        events: &'a [Self::Prepared],
    ) -> PortFuture<'a, Result<Vec<EventWriteStatus>, EventStoreError>> {
        Box::pin(async move {
            if events.is_empty() {
                return Ok(Vec::new());
            }
            if events.len() > MAX_BATCH {
                return Err(EventStoreError::Unavailable);
            }
            let unique: BTreeMap<_, _> = events
                .iter()
                .rev()
                .map(|event| (event.key(), event))
                .collect();
            let projects: Vec<_> = unique
                .keys()
                .map(|key| key.project_id().get())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let mut transaction = self
                .database
                .pool
                .begin()
                .await
                .map_err(|_| EventStoreError::Unavailable)?;
            // Share-lock projects before events in ascending order. Deletion/state changes
            // take the conflicting project lock, so an acknowledged insert cannot cross a fence.
            let rows = sqlx::query(
                "SELECT id,state FROM metric.projects WHERE id=ANY($1) ORDER BY id FOR SHARE",
            )
            .bind(&projects)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|_| EventStoreError::Unavailable)?;
            let mut eligible = BTreeSet::new();
            for row in rows {
                let state: i16 = row
                    .try_get("state")
                    .map_err(|_| EventStoreError::Unavailable)?;
                if state < 2 {
                    eligible.insert(
                        row.try_get::<i32, _>("id")
                            .map_err(|_| EventStoreError::Unavailable)?,
                    );
                }
            }
            let selected: Vec<_> = unique
                .values()
                .filter(|record| eligible.contains(&record.event.project_id.get()))
                .copied()
                .collect();
            let projects: Vec<_> = selected
                .iter()
                .map(|record| record.event.project_id.get())
                .collect();
            let ids: Vec<_> = selected
                .iter()
                .map(|record| record.event.event_id.as_bytes().to_vec())
                .collect();
            let received: Vec<_> = selected
                .iter()
                .map(|record| record.event.received_at.unix_millis())
                .collect();
            let revisions: Vec<_> = selected
                .iter()
                .map(|record| record.event.policy_revision as i64)
                .collect();
            let bodies: Vec<_> = selected
                .iter()
                .map(|record| record.body.as_slice())
                .collect();
            let inserted: Vec<Vec<u8>> = sqlx::query_scalar(
                "INSERT INTO metric.error_events(project_id,event_id,received_at,policy_revision,body,next_attempt_at) SELECT p,e,r,v,b,r FROM UNNEST($1::integer[],$2::bytea[],$3::bigint[],$4::bigint[],$5::bytea[]) AS input(p,e,r,v,b) ORDER BY p,e ON CONFLICT(project_id,event_id) DO NOTHING RETURNING event_key"
            ).bind(&projects).bind(&ids).bind(&received).bind(&revisions).bind(&bodies)
                .fetch_all(&mut *transaction).await.map_err(|_| EventStoreError::Unavailable)?;
            let inserted = inserted
                .into_iter()
                .map(|bytes| decode_key(&bytes))
                .collect::<Result<BTreeSet<_>, _>>()
                .map_err(|_| EventStoreError::Unavailable)?;
            let mut reference_projects = Vec::new();
            let mut reference_events = Vec::new();
            let mut reference_objects = Vec::new();
            let mut reference_keys = Vec::new();
            for record in selected
                .iter()
                .filter(|record| inserted.contains(&record.key()))
            {
                for key in &record.references {
                    let (project, event, object) = key
                        .event_relation()
                        .map_err(|_| EventStoreError::Unavailable)?;
                    reference_projects.push(project.get());
                    reference_events.push(event.as_bytes().to_vec());
                    reference_objects.push(object.as_bytes().to_vec());
                    reference_keys.push(key.as_str());
                }
            }
            if !reference_keys.is_empty() {
                sqlx::query("INSERT INTO metric.blob_references(project_id,event_id,object_id,source_kind,blob_key) SELECT p,e,o,0,k FROM UNNEST($1::integer[],$2::bytea[],$3::bytea[],$4::text[]) AS input(p,e,o,k) ON CONFLICT DO NOTHING")
                    .bind(&reference_projects).bind(&reference_events).bind(&reference_objects).bind(&reference_keys)
                    .execute(&mut *transaction).await.map_err(|_| EventStoreError::Unavailable)?;
            }
            // A lost COMMIT reply cannot be classified as a definite failure. A retry
            // sees the same event keys; body/reference writes are committed together.
            transaction
                .commit()
                .await
                .map_err(|_| EventStoreError::Ambiguous)?;
            let mut seen = BTreeSet::new();
            Ok(events
                .iter()
                .map(|record| {
                    if !eligible.contains(&record.event.project_id.get()) {
                        EventWriteStatus::Rejected
                    } else if seen.insert(record.key()) && inserted.contains(&record.key()) {
                        EventWriteStatus::Inserted
                    } else {
                        EventWriteStatus::Duplicate
                    }
                })
                .collect())
        })
    }
}

impl EventBacklog for PostgresEventStore {
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
            if limit > MAX_BACKLOG_PAGE || excluded.len() > MAX_BACKLOG_EXCLUSIONS {
                return Err(EventBacklogError::InvalidData);
            }
            let excluded: Vec<_> = excluded.iter().map(|key| key.as_bytes().to_vec()).collect();
            // Guard encoded size on the server before materializing a corrupt
            // oversized body in this process. NULL is decoded as InvalidData.
            let maximum_body = i64::try_from(
                self.codec
                    .max_encoded_row_bytes
                    .saturating_sub(ROW_METADATA_BYTES),
            )
            .unwrap_or(i64::MAX);
            let rows = sqlx::query("SELECT e.project_id,e.event_id,e.event_key,e.received_at,e.policy_revision,e.attempts,CASE WHEN octet_length(e.body)<=$4 THEN e.body END AS body FROM metric.error_events e JOIN metric.projects p ON p.id=e.project_id WHERE e.processing_state=0 AND e.next_attempt_at<=$1 AND NOT(e.event_key=ANY($2::bytea[])) AND p.state IN (0,1) ORDER BY e.next_attempt_at,e.received_at,e.event_key LIMIT $3")
                .bind(now.unix_millis()).bind(&excluded).bind(limit as i64).bind(maximum_body)
                .fetch_all(&self.database.pool).await.map_err(|_| EventBacklogError::Unavailable)?;
            rows.iter()
                .map(|row| decode_pending(row, self.codec))
                .collect()
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
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT 1 FROM metric.error_events WHERE processing_state=0 LIMIT $1) AS pending")
                .bind(i64::try_from(count_limit).unwrap_or(i64::MAX)).fetch_one(&self.database.pool)
                .await.map_err(|_| EventBacklogError::Unavailable)?;
            let oldest: Option<i64> = sqlx::query_scalar("SELECT received_at FROM metric.error_events WHERE processing_state=0 ORDER BY next_attempt_at,received_at,event_key LIMIT 1")
                .fetch_optional(&self.database.pool).await.map_err(|_| EventBacklogError::Unavailable)?;
            Ok(BacklogObservation {
                pending_count: u64::try_from(count).map_err(|_| EventBacklogError::InvalidData)?,
                oldest_pending_at: oldest
                    .map(Timestamp::from_unix_millis)
                    .transpose()
                    .map_err(|_| EventBacklogError::InvalidData)?,
            })
        })
    }
}

impl ProcessingProjectStore for PostgresEventStore {
    fn load_processing_project(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<ProcessingProject, ProcessingProjectError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT state,items,grouping_revision,debug_file_revision,artifact_revision FROM metric.projects WHERE id=$1")
                .bind(project_id.get()).fetch_optional(&self.database.pool).await
                .map_err(|_| ProcessingProjectError::Unavailable)?.ok_or(ProcessingProjectError::NotFound)?;
            let state = match row
                .try_get::<i16, _>("state")
                .map_err(|_| ProcessingProjectError::InvalidData)?
            {
                0 => ProjectAcceptanceState::Active,
                1 => ProjectAcceptanceState::Disabled,
                2 => ProjectAcceptanceState::PendingDelete,
                3 => ProjectAcceptanceState::Purging,
                4 => ProjectAcceptanceState::Deleted,
                _ => return Err(ProcessingProjectError::InvalidData),
            };
            let revision = |name: &str| {
                row.try_get::<i64, _>(name)
                    .ok()
                    .and_then(|value| u64::try_from(value).ok())
                    .ok_or(ProcessingProjectError::InvalidData)
            };
            let grouping_revision = revision("grouping_revision")?;
            if grouping_revision == 0 {
                return Err(ProcessingProjectError::InvalidData);
            }
            Ok(ProcessingProject {
                project_id,
                state,
                error_events_enabled: row
                    .try_get::<i32, _>("items")
                    .map_err(|_| ProcessingProjectError::InvalidData)?
                    & 1
                    != 0,
                grouping_revision,
                debug_file_revision: revision("debug_file_revision")?,
                artifact_revision: revision("artifact_revision")?,
            })
        })
    }
}

impl ProcessingStateStore for PostgresEventStore {
    fn record_processing_failure(
        &self,
        failure: ProcessingFailure,
    ) -> PortFuture<'_, Result<ProcessingStateChange, ProcessingStateError>> {
        Box::pin(async move {
            let expected = i32::try_from(failure.expected_attempts)
                .map_err(|_| ProcessingStateError::InvalidData)?;
            let next = i32::try_from(failure.new_attempts)
                .map_err(|_| ProcessingStateError::InvalidData)?;
            if expected.checked_add(1) != Some(next) {
                return Err(ProcessingStateError::InvalidData);
            }
            let (state, due) = match failure.disposition {
                ProcessingFailureDisposition::RetryAt(at) => (0_i16, Some(at.unix_millis())),
                ProcessingFailureDisposition::PermanentlyFailed => (1_i16, None),
            };
            let changed = sqlx::query("UPDATE metric.error_events SET processing_state=$1,attempts=$2,next_attempt_at=$3,last_error_code=$4 WHERE project_id=$5 AND event_id=$6 AND processing_state=0 AND attempts=$7")
                .bind(state).bind(next).bind(due).bind(failure.code.stored())
                .bind(failure.key.project_id().get()).bind(failure.key.event_id().as_bytes().as_slice()).bind(expected)
                .execute(&self.database.pool).await.map_err(|_| ProcessingStateError::Unavailable)?;
            Ok(if changed.rows_affected() == 1 {
                ProcessingStateChange::Updated
            } else {
                ProcessingStateChange::StaleOrCompleted
            })
        })
    }
}

impl BlobReferenceStore for PostgresEventStore {
    fn is_referenced(
        &self,
        reference: BlobReference,
    ) -> PortFuture<'_, Result<bool, BlobStoreError>> {
        Box::pin(async move {
            let expected = BlobKey::event_owned(
                reference.project_id,
                reference.event_id,
                reference.object_id,
            );
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metric.blob_references WHERE project_id=$1 AND event_id=$2 AND object_id=$3 AND blob_key=$4 AND source_kind IN (0,1))")
                .bind(reference.project_id.get()).bind(reference.event_id.as_bytes().as_slice())
                .bind(reference.object_id.as_bytes().as_slice()).bind(expected.as_str())
                .fetch_one(&self.database.pool).await.map_err(|_| BlobStoreError::Unavailable)
        })
    }
}

fn validate_identity(value: &Value, expected: EventId) -> Result<(), EventPrepareError> {
    let actual = value
        .get("event_id")
        .and_then(Value::as_str)
        .and_then(|value| EventId::parse(value).ok());
    if actual != Some(expected) {
        return Err(EventPrepareError::InvalidEvent);
    }
    Ok(())
}

fn owned_references(value: &Value, expected: EventKey) -> Result<Vec<BlobKey>, EventPrepareError> {
    let attachment_keys = value
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|attachment| attachment.get("blob_key").and_then(Value::as_str));
    let native_key = value
        .get("native_crash")
        .and_then(|native| native.get("blob_key"))
        .and_then(Value::as_str);
    let mut keys = BTreeSet::new();
    for text in attachment_keys.chain(native_key) {
        if let Ok(key) = BlobKey::new(text)
            && let Ok((project, event, _)) = key.event_relation()
            && EventKey::new(project, event) == expected
        {
            keys.insert(key);
        }
        if keys.len() > MAX_BLOB_REFERENCES {
            return Err(EventPrepareError::InvalidEvent);
        }
    }
    Ok(keys.into_iter().collect())
}

pub(crate) fn decode_body(body: &[u8], maximum: usize) -> Result<Vec<u8>, PostgresError> {
    if body.first() != Some(&1) || body.len() < 2 {
        return Err(PostgresError::InvalidData);
    }
    let bytes = match body[1] {
        0 if body.len() - 2 <= maximum => body[2..].to_vec(),
        1 => zstd::bulk::decompress(&body[2..], maximum).map_err(|_| PostgresError::InvalidData)?,
        _ => return Err(PostgresError::InvalidData),
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| PostgresError::InvalidData)?;
    if serde_json::to_vec(&value).map_err(|_| PostgresError::InvalidData)? != bytes {
        return Err(PostgresError::InvalidData);
    }
    Ok(bytes)
}

fn decode_key(bytes: &[u8]) -> Result<EventKey, PostgresError> {
    EventKey::from_bytes(bytes.try_into().map_err(|_| PostgresError::InvalidData)?)
        .map_err(|_| PostgresError::InvalidData)
}

fn decode_pending(
    row: &PgRow,
    config: EventCodecConfig,
) -> Result<PendingEvent, EventBacklogError> {
    fn get_error<T>(_: T) -> EventBacklogError {
        EventBacklogError::InvalidData
    }
    let project_id =
        ProjectId::new(row.try_get("project_id").map_err(get_error)?).map_err(get_error)?;
    let bytes: Vec<u8> = row.try_get("event_id").map_err(get_error)?;
    let event_id = EventId::from_bytes(bytes.try_into().map_err(get_error)?);
    let key: Vec<u8> = row.try_get("event_key").map_err(get_error)?;
    if decode_key(&key).map_err(get_error)? != EventKey::new(project_id, event_id) {
        return Err(EventBacklogError::InvalidData);
    }
    let body: Vec<u8> = row.try_get("body").map_err(get_error)?;
    if body.len().saturating_add(ROW_METADATA_BYTES) > config.max_encoded_row_bytes {
        return Err(EventBacklogError::InvalidData);
    }
    let payload = decode_body(&body, config.max_decoded_body_bytes).map_err(get_error)?;
    let value: Value = serde_json::from_slice(&payload).map_err(get_error)?;
    validate_identity(&value, event_id).map_err(get_error)?;
    let revision: i64 = row.try_get("policy_revision").map_err(get_error)?;
    let policy_revision = u64::try_from(revision).map_err(get_error)?;
    if policy_revision == 0 {
        return Err(EventBacklogError::InvalidData);
    }
    Ok(PendingEvent {
        event: AcceptedEvent {
            project_id,
            event_id,
            policy_revision,
            received_at: Timestamp::from_unix_millis(
                row.try_get("received_at").map_err(get_error)?,
            )
            .map_err(get_error)?,
            payload: ScrubbedEventPayload::new(payload),
        },
        attempts: u32::try_from(row.try_get::<i32, _>("attempts").map_err(get_error)?)
            .map_err(get_error)?,
    })
}

#[cfg(test)]
mod tests;
