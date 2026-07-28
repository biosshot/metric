//! MongoDB archive-manifest adapter and bounded due-Event selection.

use std::collections::BTreeMap;

use futures_util::TryStreamExt;
use metric_domain::{
    EventKey, ProjectId, Timestamp,
    archive::{
        ArchiveBatch, ArchiveBatchState, ArchiveEvent, ArchiveKind, ArchiveRecords,
        ArchiveSegmentId, ArchiveSignal, ArchiveSourceId, EVENT_ARCHIVE_SCHEMA_VERSION,
        LOG_ARCHIVE_SCHEMA_VERSION, METRIC_ARCHIVE_SCHEMA_VERSION, SESSION_ARCHIVE_SCHEMA_VERSION,
        SPAN_ARCHIVE_SCHEMA_VERSION,
    },
    blob::{BlobKey, BlobKind},
    grouping::IssueId,
    sessions::SessionId,
    signals::{LogId, SpanRecordId},
};
use metric_ports::{
    ArchiveClaimRequest, ArchiveCompleteRequest, ArchiveSourceCommitRequest, ArchiveStore,
    ArchiveStoreError, PortFuture,
};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::{Hint, IndexOptions},
};
use time::OffsetDateTime;

use crate::{EventCodecConfig, event, signals};

const MAXIMUM_EVENTS: usize = 10_000;
const MAXIMUM_TARGET_BYTES: usize = 512 * 1024 * 1024;
const MAXIMUM_DECODED_EVENT_BYTES: usize = 4 * 1024 * 1024;
const DAY_MILLIS: i64 = 24 * 60 * 60 * 1_000;

enum ArchiveRecord {
    Event(ArchiveEvent),
    Signal(ArchiveSignal),
}

#[derive(Clone)]
pub struct MongoArchiveStore {
    database: Database,
    event_codec: EventCodecConfig,
}

impl MongoArchiveStore {
    #[must_use]
    pub const fn from_database(database: Database, event_codec: EventCodecConfig) -> Self {
        Self {
            database,
            event_codec,
        }
    }

    async fn claim_inner(
        &self,
        request: ArchiveClaimRequest,
    ) -> Result<Option<ArchiveBatch>, ArchiveStoreError> {
        validate_claim(request)?;
        let manifests = self.database.collection::<Document>("archive_manifests");
        if let Some(existing) = manifests
            .find_one(doc! {
                "kind": request.kind.name(),
                "source_committed": false,
            })
            .sort(doc! { "state": 1, "created_at": 1, "_id": 1 })
            .hint(Hint::Name("archive_resume".to_owned()))
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
        {
            return self.decode_manifest_batch(&existing).await.map(Some);
        }
        match request.kind {
            ArchiveKind::Event => self.claim_events(request).await,
            ArchiveKind::Log | ArchiveKind::Span | ArchiveKind::Session | ArchiveKind::Metric => {
                self.claim_signals(request).await
            }
        }
    }

    async fn claim_events(
        &self,
        request: ArchiveClaimRequest,
    ) -> Result<Option<ArchiveBatch>, ArchiveStoreError> {
        let events = self.database.collection::<Document>("error_events");
        let terminal = doc! {
            "$or": [
                { "q": { "$exists": false } },
                { "q.s": 1_i32 },
            ],
        };
        let mut first_filter = doc! {
            "h": { "$lte": date(request.now) },
            "z": { "$exists": false },
        };
        first_filter.extend(terminal.clone());
        let Some(first) = events
            .find_one(first_filter)
            .sort(doc! { "h": 1, "_id": 1 })
            .projection(event_projection())
            .hint(Hint::Name("event_archive_due".to_owned()))
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
        else {
            return Ok(None);
        };
        let first_event = decode_event(&first, self.event_codec)?;
        let (day_start, day_end) = day_bounds(first_event.received_at)?;
        let mut batch_filter = doc! {
            "p": first_event.project_id.get(),
            "r": {
                "$gte": DateTime::from_millis(day_start),
                "$lt": DateTime::from_millis(day_end),
            },
            "h": { "$lte": date(request.now) },
            "z": { "$exists": false },
        };
        batch_filter.extend(terminal);
        let mut cursor = events
            .find(batch_filter)
            .sort(doc! { "r": 1, "_id": 1 })
            .projection(event_projection())
            .limit(
                i64::try_from(request.maximum_events)
                    .map_err(|_| ArchiveStoreError::InvalidData)?,
            )
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?;
        let mut selected = Vec::with_capacity(request.maximum_events);
        let mut selected_bytes = 0_usize;
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
        {
            let event = decode_event(&document, self.event_codec)?;
            let estimated = event
                .canonical_payload
                .len()
                .checked_add(96)
                .ok_or(ArchiveStoreError::InvalidData)?;
            if !selected.is_empty()
                && selected_bytes.saturating_add(estimated) > request.target_uncompressed_bytes
            {
                break;
            }
            selected_bytes = selected_bytes.saturating_add(estimated);
            selected.push(event);
        }
        if selected.is_empty() {
            return Err(ArchiveStoreError::InvalidData);
        }
        let source_ids = selected
            .iter()
            .map(|event| ArchiveSourceId::Event(event.key))
            .collect::<Vec<_>>();
        let segment_id =
            ArchiveSegmentId::derive_sources(request.kind, first_event.project_id, &source_ids);
        let received_from = selected
            .first()
            .map(|event| event.received_at)
            .ok_or(ArchiveStoreError::InvalidData)?;
        let received_to = selected
            .last()
            .map(|event| event.received_at)
            .ok_or(ArchiveStoreError::InvalidData)?;
        let datetime = OffsetDateTime::from_unix_timestamp_nanos(i128::from(day_start) * 1_000_000)
            .map_err(|_| ArchiveStoreError::InvalidData)?;
        let object_key = BlobKey::event_archive(
            first_event.project_id,
            datetime.year(),
            u8::from(datetime.month()),
            datetime.day(),
            segment_id,
        );
        let batch = ArchiveBatch {
            kind: request.kind,
            segment_id,
            project_id: first_event.project_id,
            received_from,
            received_to,
            object_key,
            source_ids,
            records: ArchiveRecords::Events(selected),
            state: ArchiveBatchState::Writing,
        };
        self.insert_manifest(batch, request.now).await.map(Some)
    }

    async fn claim_signals(
        &self,
        request: ArchiveClaimRequest,
    ) -> Result<Option<ArchiveBatch>, ArchiveStoreError> {
        let collection = self
            .database
            .collection::<Document>(source_collection(request.kind));
        let Some(first) = collection
            .find_one(doc! {
                "h": { "$lte": date(request.now) },
                "z": { "$exists": false },
            })
            .sort(doc! { "h": 1, "_id": 1 })
            .projection(source_projection(request.kind))
            .hint(Hint::Name(archive_index(request.kind).to_owned()))
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
        else {
            return Ok(None);
        };
        let first_signal = decode_signal(&first, request.kind)?;
        let (day_start, day_end) = day_bounds(first_signal.received_at)?;
        let time_field = source_time_field(request.kind);
        let mut filter = doc! {
            "p": first_signal.project_id.get(),
            "h": { "$lte": date(request.now) },
            "z": { "$exists": false },
        };
        filter.insert(
            time_field,
            doc! {
                "$gte": DateTime::from_millis(day_start),
                "$lt": DateTime::from_millis(day_end),
            },
        );
        let mut sort = doc! { "_id": 1 };
        sort.insert(time_field, 1);
        let mut cursor = collection
            .find(filter)
            .sort(sort)
            .projection(source_projection(request.kind))
            .limit(
                i64::try_from(request.maximum_events)
                    .map_err(|_| ArchiveStoreError::InvalidData)?,
            )
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?;
        let mut selected = Vec::with_capacity(request.maximum_events);
        let mut selected_bytes = 0_usize;
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
        {
            let signal = decode_signal(&document, request.kind)?;
            let estimated = signal
                .canonical_payload
                .len()
                .checked_add(64)
                .ok_or(ArchiveStoreError::InvalidData)?;
            if !selected.is_empty()
                && selected_bytes.saturating_add(estimated) > request.target_uncompressed_bytes
            {
                break;
            }
            selected_bytes = selected_bytes.saturating_add(estimated);
            selected.push(signal);
        }
        if selected.is_empty() {
            return Err(ArchiveStoreError::InvalidData);
        }
        let source_ids = selected
            .iter()
            .map(|signal| match request.kind {
                ArchiveKind::Log => ArchiveSourceId::Log(LogId::from_bytes(signal.id)),
                ArchiveKind::Span => ArchiveSourceId::Span(SpanRecordId::from_bytes(signal.id)),
                ArchiveKind::Session => ArchiveSourceId::Session(SessionId::from_bytes(signal.id)),
                ArchiveKind::Metric => ArchiveSourceId::Metric(signal.id),
                ArchiveKind::Event => unreachable!("Event uses claim_events"),
            })
            .collect::<Vec<_>>();
        let segment_id =
            ArchiveSegmentId::derive_sources(request.kind, first_signal.project_id, &source_ids);
        let received_from = selected
            .first()
            .map(|signal| signal.received_at)
            .ok_or(ArchiveStoreError::InvalidData)?;
        let received_to = selected
            .last()
            .map(|signal| signal.received_at)
            .ok_or(ArchiveStoreError::InvalidData)?;
        let datetime = OffsetDateTime::from_unix_timestamp_nanos(i128::from(day_start) * 1_000_000)
            .map_err(|_| ArchiveStoreError::InvalidData)?;
        let object_key = BlobKey::archive(
            request.kind,
            first_signal.project_id,
            datetime.year(),
            u8::from(datetime.month()),
            datetime.day(),
            segment_id,
        );
        let records = match request.kind {
            ArchiveKind::Log => ArchiveRecords::Logs(selected),
            ArchiveKind::Span => ArchiveRecords::Spans(selected),
            ArchiveKind::Session => ArchiveRecords::Sessions(selected),
            ArchiveKind::Metric => ArchiveRecords::Metrics(selected),
            ArchiveKind::Event => unreachable!("Event uses claim_events"),
        };
        let batch = ArchiveBatch {
            kind: request.kind,
            segment_id,
            project_id: first_signal.project_id,
            received_from,
            received_to,
            object_key,
            source_ids,
            records,
            state: ArchiveBatchState::Writing,
        };
        self.insert_manifest(batch, request.now).await.map(Some)
    }

    async fn insert_manifest(
        &self,
        batch: ArchiveBatch,
        created_at: Timestamp,
    ) -> Result<ArchiveBatch, ArchiveStoreError> {
        let manifests = self.database.collection::<Document>("archive_manifests");
        let manifest = doc! {
            "_id": binary(batch.segment_id.as_bytes()),
            "kind": batch.kind.name(),
            "project_id": batch.project_id.get(),
            "received_from": date(batch.received_from),
            "received_to": date(batch.received_to),
            "object_key": batch.object_key.as_str(),
            "format": "parquet",
            "compression": "zstd",
            "schema_version": i32::from(schema_version(batch.kind)),
            "record_count": i64::try_from(batch.source_ids.len()).map_err(|_| ArchiveStoreError::InvalidData)?,
            "state": "writing",
            "source_ids": batch.source_ids.iter().map(|id| Bson::Binary(binary_slice(&id.as_bytes()))).collect::<Vec<_>>(),
            "source_committed": false,
            "created_at": date(created_at),
        };
        match manifests.insert_one(manifest).await {
            Ok(_) => Ok(batch),
            Err(error) if is_duplicate(&error) => {
                let existing = manifests
                    .find_one(doc! { "_id": binary(batch.segment_id.as_bytes()) })
                    .await
                    .map_err(|_| ArchiveStoreError::Unavailable)?
                    .ok_or(ArchiveStoreError::Unavailable)?;
                self.decode_manifest_batch(&existing).await
            }
            Err(_) => Err(ArchiveStoreError::Unavailable),
        }
    }

    async fn decode_manifest_batch(
        &self,
        document: &Document,
    ) -> Result<ArchiveBatch, ArchiveStoreError> {
        let segment_id = ArchiveSegmentId::from_bytes(fixed_binary::<16>(document, "_id")?);
        let kind = ArchiveKind::from_name(
            document
                .get_str("kind")
                .map_err(|_| ArchiveStoreError::InvalidData)?,
        )
        .map_err(|_| ArchiveStoreError::InvalidData)?;
        let project_id = ProjectId::new(
            document
                .get_i32("project_id")
                .map_err(|_| ArchiveStoreError::InvalidData)?,
        )
        .map_err(|_| ArchiveStoreError::InvalidData)?;
        let source_ids = document
            .get_array("source_ids")
            .map_err(|_| ArchiveStoreError::InvalidData)?
            .iter()
            .map(|value| decode_source_id(value, kind))
            .collect::<Result<Vec<_>, _>>()?;
        if source_ids.is_empty() || source_ids.len() > MAXIMUM_EVENTS {
            return Err(ArchiveStoreError::InvalidData);
        }
        let state = match document
            .get_str("state")
            .map_err(|_| ArchiveStoreError::InvalidData)?
        {
            "writing" => ArchiveBatchState::Writing,
            "complete" => ArchiveBatchState::Complete,
            _ => return Err(ArchiveStoreError::InvalidData),
        };
        let records = if state == ArchiveBatchState::Writing {
            let ids = source_ids
                .iter()
                .map(|id| Bson::Binary(binary_slice(&id.as_bytes())))
                .collect::<Vec<_>>();
            let mut cursor = self
                .database
                .collection::<Document>(source_collection(kind))
                .find(doc! { "_id": { "$in": ids } })
                .projection(source_projection(kind))
                .await
                .map_err(|_| ArchiveStoreError::Unavailable)?;
            let mut by_key = BTreeMap::new();
            while let Some(document) = cursor
                .try_next()
                .await
                .map_err(|_| ArchiveStoreError::Unavailable)?
            {
                match kind {
                    ArchiveKind::Event => {
                        let event = decode_event(&document, self.event_codec)?;
                        by_key.insert(
                            ArchiveSourceId::Event(event.key),
                            ArchiveRecord::Event(event),
                        );
                    }
                    ArchiveKind::Log
                    | ArchiveKind::Span
                    | ArchiveKind::Session
                    | ArchiveKind::Metric => {
                        let signal = decode_signal(&document, kind)?;
                        let id = match kind {
                            ArchiveKind::Log => ArchiveSourceId::Log(LogId::from_bytes(signal.id)),
                            ArchiveKind::Span => {
                                ArchiveSourceId::Span(SpanRecordId::from_bytes(signal.id))
                            }
                            ArchiveKind::Session => {
                                ArchiveSourceId::Session(SessionId::from_bytes(signal.id))
                            }
                            ArchiveKind::Metric => ArchiveSourceId::Metric(signal.id),
                            ArchiveKind::Event => unreachable!(),
                        };
                        by_key.insert(id, ArchiveRecord::Signal(signal));
                    }
                }
            }
            match kind {
                ArchiveKind::Event => {
                    let mut values = Vec::with_capacity(source_ids.len());
                    for id in &source_ids {
                        let ArchiveRecord::Event(value) =
                            by_key.remove(id).ok_or(ArchiveStoreError::InvalidData)?
                        else {
                            return Err(ArchiveStoreError::InvalidData);
                        };
                        values.push(value);
                    }
                    ArchiveRecords::Events(values)
                }
                ArchiveKind::Log
                | ArchiveKind::Span
                | ArchiveKind::Session
                | ArchiveKind::Metric => {
                    let mut values = Vec::with_capacity(source_ids.len());
                    for id in &source_ids {
                        let ArchiveRecord::Signal(value) =
                            by_key.remove(id).ok_or(ArchiveStoreError::InvalidData)?
                        else {
                            return Err(ArchiveStoreError::InvalidData);
                        };
                        values.push(value);
                    }
                    match kind {
                        ArchiveKind::Log => ArchiveRecords::Logs(values),
                        ArchiveKind::Span => ArchiveRecords::Spans(values),
                        ArchiveKind::Session => ArchiveRecords::Sessions(values),
                        ArchiveKind::Metric => ArchiveRecords::Metrics(values),
                        ArchiveKind::Event => unreachable!(),
                    }
                }
            }
        } else {
            empty_records(kind)
        };
        Ok(ArchiveBatch {
            kind,
            segment_id,
            project_id,
            received_from: timestamp(document, "received_from")?,
            received_to: timestamp(document, "received_to")?,
            object_key: BlobKey::new(
                document
                    .get_str("object_key")
                    .map_err(|_| ArchiveStoreError::InvalidData)?
                    .to_owned(),
            )
            .map_err(|_| ArchiveStoreError::InvalidData)?,
            source_ids,
            records,
            state,
        })
    }

    async fn complete_inner(
        &self,
        request: ArchiveCompleteRequest,
    ) -> Result<(), ArchiveStoreError> {
        let manifests = self.database.collection::<Document>("archive_manifests");
        let id = binary(request.segment_id.as_bytes());
        let existing = manifests
            .find_one(doc! { "_id": &id })
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
            .ok_or(ArchiveStoreError::InvalidData)?;
        let kind = manifest_kind(&existing)?;
        if request.object.kind != BlobKind::archive(kind) {
            return Err(ArchiveStoreError::InvalidData);
        }
        if existing.get_str("object_key") != Ok(request.object.key.as_str()) {
            return Err(ArchiveStoreError::Conflict);
        }
        if existing.get_str("state") == Ok("complete") {
            return manifest_object_matches(&existing, &request)
                .then_some(())
                .ok_or(ArchiveStoreError::Conflict);
        }
        if existing.get_str("state") != Ok("writing") {
            return Err(ArchiveStoreError::InvalidData);
        }
        let result = manifests
            .update_one(
                doc! { "_id": id, "state": "writing", "source_committed": false },
                doc! { "$set": {
                    "state": "complete",
                    "stored_bytes": i64::try_from(request.object.size).map_err(|_| ArchiveStoreError::InvalidData)?,
                    "checksum": binary(request.object.checksum.as_bytes()),
                    "completed_at": date(request.completed_at),
                } },
            )
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?;
        if result.modified_count != 1 {
            return Err(ArchiveStoreError::Unavailable);
        }
        Ok(())
    }

    async fn commit_sources_inner(
        &self,
        request: ArchiveSourceCommitRequest,
    ) -> Result<usize, ArchiveStoreError> {
        if request.source_ids.is_empty()
            || request.source_ids.len() > MAXIMUM_EVENTS
            || request
                .source_ids
                .iter()
                .any(|source_id| source_id.kind() != request.kind)
        {
            return Err(ArchiveStoreError::InvalidData);
        }
        let manifests = self.database.collection::<Document>("archive_manifests");
        let id = binary(request.segment_id.as_bytes());
        let manifest = manifests
            .find_one(doc! { "_id": &id, "state": "complete" })
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
            .ok_or(ArchiveStoreError::InvalidData)?;
        if manifest_kind(&manifest)? != request.kind {
            return Err(ArchiveStoreError::Conflict);
        }
        let expected = manifest_source_ids(&manifest)?;
        if expected != request.source_ids {
            return Err(ArchiveStoreError::Conflict);
        }
        if manifest.get_bool("source_committed") == Ok(true) {
            return Ok(0);
        }
        let ids = request
            .source_ids
            .iter()
            .map(|source_id| Bson::Binary(binary_slice(&source_id.as_bytes())))
            .collect::<Vec<_>>();
        let collection = self
            .database
            .collection::<Document>(source_collection(request.kind));
        let conflicting = self
            .database
            .collection::<Document>(source_collection(request.kind))
            .count_documents(doc! {
                "_id": { "$in": &ids },
                "z": { "$exists": true, "$ne": Bson::Binary(id.clone()) },
            })
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?;
        if conflicting > 0 {
            return Err(ArchiveStoreError::Conflict);
        }
        let result = collection
            .update_many(
                doc! { "_id": { "$in": ids } },
                doc! {
                    "$set": {
                        "z": Bson::Binary(id.clone()),
                        "x": date(request.expire_at),
                    },
                    "$unset": { "h": "" },
                },
            )
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?;
        let manifest_result = manifests
            .update_one(
                doc! { "_id": id, "state": "complete", "source_committed": false },
                doc! { "$set": { "source_committed": true } },
            )
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?;
        if manifest_result.modified_count != 1 {
            return Err(ArchiveStoreError::Unavailable);
        }
        usize::try_from(result.modified_count).map_err(|_| ArchiveStoreError::InvalidData)
    }
}

impl ArchiveStore for MongoArchiveStore {
    fn claim(
        &self,
        request: ArchiveClaimRequest,
    ) -> PortFuture<'_, Result<Option<ArchiveBatch>, ArchiveStoreError>> {
        Box::pin(self.claim_inner(request))
    }

    fn complete(
        &self,
        request: ArchiveCompleteRequest,
    ) -> PortFuture<'_, Result<(), ArchiveStoreError>> {
        Box::pin(self.complete_inner(request))
    }

    fn commit_sources(
        &self,
        request: ArchiveSourceCommitRequest,
    ) -> PortFuture<'_, Result<usize, ArchiveStoreError>> {
        Box::pin(self.commit_sources_inner(request))
    }

    fn object_referenced(&self, key: &BlobKey) -> PortFuture<'_, Result<bool, ArchiveStoreError>> {
        let key = key.as_str().to_owned();
        Box::pin(async move {
            let count = self
                .database
                .collection::<Document>("archive_manifests")
                .count_documents(doc! { "object_key": key })
                .limit(1)
                .await
                .map_err(|_| ArchiveStoreError::Unavailable)?;
            Ok(count > 0)
        })
    }
}

pub(crate) fn archive_manifest_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": [
                "_id", "kind", "project_id", "received_from", "received_to", "object_key",
                "format", "compression", "schema_version", "record_count", "state",
                "source_ids", "source_committed", "created_at",
            ],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "kind": { "enum": ["event", "log", "span", "session"] },
                "project_id": { "bsonType": "int", "minimum": 1 },
                "received_from": { "bsonType": "date" },
                "received_to": { "bsonType": "date" },
                "object_key": { "bsonType": "string", "minLength": 1, "maxLength": 512 },
                "format": { "enum": ["parquet"] },
                "compression": { "enum": ["zstd"] },
                "schema_version": { "bsonType": "int", "enum": [1] },
                "record_count": { "bsonType": "long", "minimum": 1, "maximum": 10000 },
                "stored_bytes": { "bsonType": "long", "minimum": 0 },
                "checksum": { "bsonType": "binData" },
                "state": { "enum": ["writing", "complete"] },
                "source_ids": {
                    "bsonType": "array",
                    "minItems": 1,
                    "maxItems": 10000,
                    "items": { "bsonType": "binData" },
                },
                "source_committed": { "bsonType": "bool" },
                "created_at": { "bsonType": "date" },
                "completed_at": { "bsonType": "date" },
            },
        },
        "$expr": {
            "$and": [
                { "$eq": [{ "$binarySize": "$_id" }, 16] },
                { "$eq": ["$record_count", { "$size": "$source_ids" }] },
                { "$or": [
                    { "$and": [
                        { "$eq": ["$state", "writing"] },
                        { "$eq": [{ "$type": "$stored_bytes" }, "missing"] },
                        { "$eq": [{ "$type": "$checksum" }, "missing"] },
                        { "$eq": [{ "$type": "$completed_at" }, "missing"] },
                    ] },
                    { "$and": [
                        { "$eq": ["$state", "complete"] },
                        { "$eq": [{ "$type": "$stored_bytes" }, "long"] },
                        { "$eq": [{ "$binarySize": "$checksum" }, 32] },
                        { "$eq": [{ "$type": "$completed_at" }, "date"] },
                    ] },
                ] },
            ],
        },
    }
}

pub(crate) fn archive_indexes() -> [IndexModel; 2] {
    [
        IndexModel::builder()
            .keys(doc! { "kind": 1, "source_committed": 1, "state": 1, "created_at": 1, "_id": 1 })
            .options(
                IndexOptions::builder()
                    .name("archive_resume".to_owned())
                    .partial_filter_expression(doc! { "source_committed": false })
                    .build(),
            )
            .build(),
        IndexModel::builder()
            .keys(doc! { "project_id": 1, "kind": 1, "received_from": 1, "_id": 1 })
            .options(
                IndexOptions::builder()
                    .name("archive_project_range".to_owned())
                    .build(),
            )
            .build(),
    ]
}

pub(crate) async fn validate_archive_indexes(
    database: &Database,
) -> Result<bool, mongodb::error::Error> {
    let expected = BTreeMap::from([
        (
            "archive_project_range".to_owned(),
            doc! { "project_id": 1, "kind": 1, "received_from": 1, "_id": 1 },
        ),
        (
            "archive_resume".to_owned(),
            doc! { "kind": 1, "source_committed": 1, "state": 1, "created_at": 1, "_id": 1 },
        ),
    ]);
    let mut actual = BTreeMap::new();
    let mut indexes = database
        .collection::<Document>("archive_manifests")
        .list_indexes()
        .await?;
    while let Some(index) = indexes.try_next().await? {
        let Some(name) = index
            .options
            .as_ref()
            .and_then(|options| options.name.as_deref())
        else {
            return Ok(false);
        };
        if name != "_id_" {
            actual.insert(name.to_owned(), index.keys);
        }
    }
    Ok(actual == expected)
}

fn decode_event(
    document: &Document,
    codec: EventCodecConfig,
) -> Result<ArchiveEvent, ArchiveStoreError> {
    if document.get_datetime("h").is_err() || document.contains_key("z") {
        return Err(ArchiveStoreError::InvalidData);
    }
    match document.get("q") {
        None => {}
        Some(Bson::Document(state)) if state.get_i32("s") == Ok(1) => {}
        _ => return Err(ArchiveStoreError::InvalidData),
    }
    let key = EventKey::from_bytes(fixed_binary::<20>(document, "_id")?)
        .map_err(|_| ArchiveStoreError::InvalidData)?;
    let project_id = ProjectId::new(
        document
            .get_i32("p")
            .map_err(|_| ArchiveStoreError::InvalidData)?,
    )
    .map_err(|_| ArchiveStoreError::InvalidData)?;
    if key.project_id() != project_id {
        return Err(ArchiveStoreError::InvalidData);
    }
    let issue_id = match document.get("u") {
        None => None,
        Some(Bson::Binary(value)) if value.subtype == BinarySubtype::Generic => {
            Some(IssueId::from_bytes(
                value
                    .bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| ArchiveStoreError::InvalidData)?,
            ))
        }
        Some(_) => return Err(ArchiveStoreError::InvalidData),
    };
    let body = document
        .get_binary_generic("b")
        .map_err(|_| ArchiveStoreError::InvalidData)?;
    let maximum = codec
        .max_decoded_body_bytes
        .min(MAXIMUM_DECODED_EVENT_BYTES);
    let payload = event::decode_body(body, maximum).map_err(|_| ArchiveStoreError::InvalidData)?;
    Ok(ArchiveEvent {
        key,
        project_id,
        received_at: timestamp(document, "r")?,
        occurred_at: timestamp(document, "o")?,
        issue_id,
        canonical_payload: payload.into_boxed_slice(),
    })
}

fn decode_signal(
    document: &Document,
    kind: ArchiveKind,
) -> Result<ArchiveSignal, ArchiveStoreError> {
    if !matches!(
        kind,
        ArchiveKind::Log | ArchiveKind::Span | ArchiveKind::Session | ArchiveKind::Metric
    ) || document.get_datetime("h").is_err()
        || document.contains_key("z")
    {
        return Err(ArchiveStoreError::InvalidData);
    }
    let project_id = ProjectId::new(
        document
            .get_i32("p")
            .map_err(|_| ArchiveStoreError::InvalidData)?,
    )
    .map_err(|_| ArchiveStoreError::InvalidData)?;
    let payload = if kind == ArchiveKind::Session {
        serde_json::to_vec(&serde_json::json!({
            "session_id": hex::encode(fixed_binary::<16>(document, "_id")?),
            "project_id": project_id.get(),
            "release_id": hex::encode(fixed_binary::<16>(document, "r")?),
            "environment_id": hex::encode(fixed_binary::<16>(document, "e")?),
            "started_at_unix_ms": timestamp(document, "s")?.unix_millis(),
            "last_update_unix_ms": timestamp(document, "l")?.unix_millis(),
            "state": document.get_i32("q").map_err(|_| ArchiveStoreError::InvalidData)?,
            "sequence": document.get_i64("n").ok(),
            "finished_at_unix_ms": document.get_datetime("f").ok().map(|value| value.timestamp_millis()),
            "duration_ms": document.get_i64("d").ok(),
            "user_digest": document.get_binary_generic("u").ok().map(hex::encode),
        }))
        .map_err(|_| ArchiveStoreError::InvalidData)?
        .into_boxed_slice()
    } else if kind == ArchiveKind::Metric {
        serde_json::to_vec(document)
            .map_err(|_| ArchiveStoreError::InvalidData)?
            .into_boxed_slice()
    } else {
        signals::decode_body(document).map_err(|_| ArchiveStoreError::InvalidData)?
    };
    Ok(ArchiveSignal {
        id: fixed_binary::<16>(document, "_id")?,
        project_id,
        received_at: timestamp(
            document,
            if kind == ArchiveKind::Session {
                "f"
            } else {
                "r"
            },
        )?,
        occurred_at_ns: if kind == ArchiveKind::Session {
            timestamp(document, "s")?
                .unix_millis()
                .checked_mul(1_000_000)
                .ok_or(ArchiveStoreError::InvalidData)?
        } else if kind == ArchiveKind::Metric {
            timestamp(document, "t")?
                .unix_millis()
                .checked_mul(1_000_000)
                .ok_or(ArchiveStoreError::InvalidData)?
        } else {
            document
                .get_i64("o")
                .map_err(|_| ArchiveStoreError::InvalidData)?
        },
        canonical_payload: payload,
    })
}

fn manifest_source_ids(document: &Document) -> Result<Vec<ArchiveSourceId>, ArchiveStoreError> {
    let kind = manifest_kind(document)?;
    document
        .get_array("source_ids")
        .map_err(|_| ArchiveStoreError::InvalidData)?
        .iter()
        .map(|value| decode_source_id(value, kind))
        .collect()
}

fn decode_source_id(value: &Bson, kind: ArchiveKind) -> Result<ArchiveSourceId, ArchiveStoreError> {
    let Bson::Binary(value) = value else {
        return Err(ArchiveStoreError::InvalidData);
    };
    if value.subtype != BinarySubtype::Generic {
        return Err(ArchiveStoreError::InvalidData);
    }
    match kind {
        ArchiveKind::Event => {
            let bytes = value
                .bytes
                .as_slice()
                .try_into()
                .map_err(|_| ArchiveStoreError::InvalidData)?;
            EventKey::from_bytes(bytes)
                .map(ArchiveSourceId::Event)
                .map_err(|_| ArchiveStoreError::InvalidData)
        }
        ArchiveKind::Log => value
            .bytes
            .as_slice()
            .try_into()
            .map(LogId::from_bytes)
            .map(ArchiveSourceId::Log)
            .map_err(|_| ArchiveStoreError::InvalidData),
        ArchiveKind::Span => value
            .bytes
            .as_slice()
            .try_into()
            .map(SpanRecordId::from_bytes)
            .map(ArchiveSourceId::Span)
            .map_err(|_| ArchiveStoreError::InvalidData),
        ArchiveKind::Session => value
            .bytes
            .as_slice()
            .try_into()
            .map(SessionId::from_bytes)
            .map(ArchiveSourceId::Session)
            .map_err(|_| ArchiveStoreError::InvalidData),
        ArchiveKind::Metric => value
            .bytes
            .as_slice()
            .try_into()
            .map(ArchiveSourceId::Metric)
            .map_err(|_| ArchiveStoreError::InvalidData),
    }
}

fn manifest_kind(document: &Document) -> Result<ArchiveKind, ArchiveStoreError> {
    ArchiveKind::from_name(
        document
            .get_str("kind")
            .map_err(|_| ArchiveStoreError::InvalidData)?,
    )
    .map_err(|_| ArchiveStoreError::InvalidData)
}

fn manifest_object_matches(document: &Document, request: &ArchiveCompleteRequest) -> bool {
    document
        .get_i64("stored_bytes")
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        == Some(request.object.size)
        && document
            .get_binary_generic("checksum")
            .is_ok_and(|value| value.as_slice() == request.object.checksum.as_bytes())
}

fn event_projection() -> Document {
    doc! { "_id": 1, "p": 1, "r": 1, "o": 1, "h": 1, "z": 1, "u": 1, "q": 1, "b": 1 }
}

fn signal_projection() -> Document {
    doc! { "_id": 1, "p": 1, "r": 1, "o": 1, "h": 1, "z": 1, "b": 1 }
}

fn session_projection() -> Document {
    doc! {
        "_id": 1, "p": 1, "r": 1, "e": 1, "s": 1, "l": 1, "q": 1,
        "n": 1, "f": 1, "d": 1, "u": 1, "h": 1, "z": 1
    }
}

fn metric_projection() -> Document {
    doc! {
        "_id": 1, "p": 1, "s": 1, "n": 1, "k": 1, "u": 1, "a": 1,
        "t": 1, "w": 1, "r": 1, "g": 1, "v": 1, "s0": 1, "c": 1,
        "lo": 1, "hi": 1, "q": 1, "h": 1, "z": 1
    }
}

fn source_projection(kind: ArchiveKind) -> Document {
    match kind {
        ArchiveKind::Event => event_projection(),
        ArchiveKind::Log | ArchiveKind::Span => signal_projection(),
        ArchiveKind::Session => session_projection(),
        ArchiveKind::Metric => metric_projection(),
    }
}

fn source_collection(kind: ArchiveKind) -> &'static str {
    match kind {
        ArchiveKind::Event => "error_events",
        ArchiveKind::Log => "logs",
        ArchiveKind::Span => "spans",
        ArchiveKind::Session => "sessions",
        ArchiveKind::Metric => "metric_buckets",
    }
}

fn archive_index(kind: ArchiveKind) -> &'static str {
    match kind {
        ArchiveKind::Event => "event_archive_due",
        ArchiveKind::Log => "log_archive_due",
        ArchiveKind::Span => "span_archive_due",
        ArchiveKind::Session => "session_archive_due",
        ArchiveKind::Metric => "metric_archive_due",
    }
}

fn schema_version(kind: ArchiveKind) -> u16 {
    match kind {
        ArchiveKind::Event => EVENT_ARCHIVE_SCHEMA_VERSION,
        ArchiveKind::Log => LOG_ARCHIVE_SCHEMA_VERSION,
        ArchiveKind::Span => SPAN_ARCHIVE_SCHEMA_VERSION,
        ArchiveKind::Session => SESSION_ARCHIVE_SCHEMA_VERSION,
        ArchiveKind::Metric => METRIC_ARCHIVE_SCHEMA_VERSION,
    }
}

fn empty_records(kind: ArchiveKind) -> ArchiveRecords {
    match kind {
        ArchiveKind::Event => ArchiveRecords::Events(Vec::new()),
        ArchiveKind::Log => ArchiveRecords::Logs(Vec::new()),
        ArchiveKind::Span => ArchiveRecords::Spans(Vec::new()),
        ArchiveKind::Session => ArchiveRecords::Sessions(Vec::new()),
        ArchiveKind::Metric => ArchiveRecords::Metrics(Vec::new()),
    }
}

fn source_time_field(kind: ArchiveKind) -> &'static str {
    match kind {
        ArchiveKind::Session => "f",
        ArchiveKind::Event | ArchiveKind::Log | ArchiveKind::Span | ArchiveKind::Metric => "r",
    }
}

fn day_bounds(timestamp: Timestamp) -> Result<(i64, i64), ArchiveStoreError> {
    let start = timestamp.unix_millis().div_euclid(DAY_MILLIS) * DAY_MILLIS;
    let end = start
        .checked_add(DAY_MILLIS)
        .ok_or(ArchiveStoreError::InvalidData)?;
    Ok((start, end))
}

fn validate_claim(request: ArchiveClaimRequest) -> Result<(), ArchiveStoreError> {
    ((1..=MAXIMUM_EVENTS).contains(&request.maximum_events)
        && (1024..=MAXIMUM_TARGET_BYTES).contains(&request.target_uncompressed_bytes))
    .then_some(())
    .ok_or(ArchiveStoreError::InvalidData)
}

fn timestamp(document: &Document, field: &str) -> Result<Timestamp, ArchiveStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(field)
            .map_err(|_| ArchiveStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| ArchiveStoreError::InvalidData)
}

fn fixed_binary<const N: usize>(
    document: &Document,
    field: &str,
) -> Result<[u8; N], ArchiveStoreError> {
    document
        .get_binary_generic(field)
        .map_err(|_| ArchiveStoreError::InvalidData)?
        .as_slice()
        .try_into()
        .map_err(|_| ArchiveStoreError::InvalidData)
}

fn binary<const N: usize>(bytes: [u8; N]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn binary_slice(bytes: &[u8]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn is_duplicate(error: &mongodb::error::Error) -> bool {
    error.contains_label("DuplicateKey") || error.to_string().contains("E11000")
}
