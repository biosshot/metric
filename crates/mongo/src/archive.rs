//! MongoDB archive-manifest adapter and bounded due-Event selection.

use std::collections::BTreeMap;

use faultkeep_domain::{
    EventKey, ProjectId, Timestamp,
    archive::{
        ArchiveBatch, ArchiveBatchState, ArchiveEvent, ArchiveSegmentId,
        EVENT_ARCHIVE_SCHEMA_VERSION,
    },
    blob::{BlobKey, BlobKind},
    grouping::IssueId,
};
use faultkeep_ports::{
    ArchiveClaimRequest, ArchiveCompleteRequest, ArchiveSourceCommitRequest, ArchiveStore,
    ArchiveStoreError, PortFuture,
};
use futures_util::TryStreamExt;
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::{Hint, IndexOptions},
};
use time::OffsetDateTime;

use crate::{EventCodecConfig, event};

const MAXIMUM_EVENTS: usize = 10_000;
const MAXIMUM_TARGET_BYTES: usize = 512 * 1024 * 1024;
const MAXIMUM_DECODED_EVENT_BYTES: usize = 4 * 1024 * 1024;
const DAY_MILLIS: i64 = 24 * 60 * 60 * 1_000;

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
            .find_one(doc! { "source_committed": false })
            .sort(doc! { "state": 1, "created_at": 1, "_id": 1 })
            .hint(Hint::Name("archive_resume".to_owned()))
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
        {
            return self.decode_manifest_batch(&existing).await.map(Some);
        }

        let events = self.database.collection::<Document>("events");
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
        let day_start = first_event.received_at.unix_millis().div_euclid(DAY_MILLIS) * DAY_MILLIS;
        let day_end = day_start
            .checked_add(DAY_MILLIS)
            .ok_or(ArchiveStoreError::InvalidData)?;
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
        let event_keys = selected.iter().map(|event| event.key).collect::<Vec<_>>();
        let segment_id = ArchiveSegmentId::derive(first_event.project_id, &event_keys);
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
        let manifest = doc! {
            "_id": binary(segment_id.as_bytes()),
            "project_id": first_event.project_id.get(),
            "received_from": date(received_from),
            "received_to": date(received_to),
            "object_key": object_key.as_str(),
            "format": "parquet",
            "compression": "zstd",
            "schema_version": i32::from(EVENT_ARCHIVE_SCHEMA_VERSION),
            "event_count": i64::try_from(selected.len()).map_err(|_| ArchiveStoreError::InvalidData)?,
            "state": "writing",
            "event_ids": event_keys.iter().map(|key| Bson::Binary(binary(key.as_bytes()))).collect::<Vec<_>>(),
            "source_committed": false,
            "created_at": date(request.now),
        };
        match manifests.insert_one(manifest).await {
            Ok(_) => Ok(Some(ArchiveBatch {
                segment_id,
                project_id: first_event.project_id,
                received_from,
                received_to,
                object_key,
                event_keys,
                events: selected,
                state: ArchiveBatchState::Writing,
            })),
            Err(error) if is_duplicate(&error) => {
                let existing = manifests
                    .find_one(doc! { "_id": binary(segment_id.as_bytes()) })
                    .await
                    .map_err(|_| ArchiveStoreError::Unavailable)?
                    .ok_or(ArchiveStoreError::Unavailable)?;
                self.decode_manifest_batch(&existing).await.map(Some)
            }
            Err(_) => Err(ArchiveStoreError::Unavailable),
        }
    }

    async fn decode_manifest_batch(
        &self,
        document: &Document,
    ) -> Result<ArchiveBatch, ArchiveStoreError> {
        let segment_id = ArchiveSegmentId::from_bytes(fixed_binary::<16>(document, "_id")?);
        let project_id = ProjectId::new(
            document
                .get_i32("project_id")
                .map_err(|_| ArchiveStoreError::InvalidData)?,
        )
        .map_err(|_| ArchiveStoreError::InvalidData)?;
        let event_keys = document
            .get_array("event_ids")
            .map_err(|_| ArchiveStoreError::InvalidData)?
            .iter()
            .map(|value| match value {
                Bson::Binary(value) if value.subtype == BinarySubtype::Generic => {
                    let bytes: [u8; 20] = value
                        .bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| ArchiveStoreError::InvalidData)?;
                    EventKey::from_bytes(bytes).map_err(|_| ArchiveStoreError::InvalidData)
                }
                _ => Err(ArchiveStoreError::InvalidData),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if event_keys.is_empty() || event_keys.len() > MAXIMUM_EVENTS {
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
        let mut decoded = Vec::new();
        if state == ArchiveBatchState::Writing {
            let ids = event_keys
                .iter()
                .map(|key| Bson::Binary(binary(key.as_bytes())))
                .collect::<Vec<_>>();
            let mut cursor = self
                .database
                .collection::<Document>("events")
                .find(doc! { "_id": { "$in": ids } })
                .projection(event_projection())
                .await
                .map_err(|_| ArchiveStoreError::Unavailable)?;
            let mut by_key = BTreeMap::new();
            while let Some(event) = cursor
                .try_next()
                .await
                .map_err(|_| ArchiveStoreError::Unavailable)?
            {
                let event = decode_event(&event, self.event_codec)?;
                by_key.insert(event.key, event);
            }
            for key in &event_keys {
                decoded.push(by_key.remove(key).ok_or(ArchiveStoreError::InvalidData)?);
            }
        }
        Ok(ArchiveBatch {
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
            event_keys,
            events: decoded,
            state,
        })
    }

    async fn complete_inner(
        &self,
        request: ArchiveCompleteRequest,
    ) -> Result<(), ArchiveStoreError> {
        if request.object.kind != BlobKind::EventArchive {
            return Err(ArchiveStoreError::InvalidData);
        }
        let manifests = self.database.collection::<Document>("archive_manifests");
        let id = binary(request.segment_id.as_bytes());
        let existing = manifests
            .find_one(doc! { "_id": &id })
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
            .ok_or(ArchiveStoreError::InvalidData)?;
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
        if request.event_keys.is_empty() || request.event_keys.len() > MAXIMUM_EVENTS {
            return Err(ArchiveStoreError::InvalidData);
        }
        let manifests = self.database.collection::<Document>("archive_manifests");
        let id = binary(request.segment_id.as_bytes());
        let manifest = manifests
            .find_one(doc! { "_id": &id, "state": "complete" })
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?
            .ok_or(ArchiveStoreError::InvalidData)?;
        let expected = manifest_event_keys(&manifest)?;
        if expected != request.event_keys {
            return Err(ArchiveStoreError::Conflict);
        }
        if manifest.get_bool("source_committed") == Ok(true) {
            return Ok(0);
        }
        let ids = request
            .event_keys
            .iter()
            .map(|key| Bson::Binary(binary(key.as_bytes())))
            .collect::<Vec<_>>();
        let conflicting = self
            .database
            .collection::<Document>("events")
            .count_documents(doc! {
                "_id": { "$in": &ids },
                "z": { "$exists": true, "$ne": Bson::Binary(id.clone()) },
            })
            .await
            .map_err(|_| ArchiveStoreError::Unavailable)?;
        if conflicting > 0 {
            return Err(ArchiveStoreError::Conflict);
        }
        let result = self
            .database
            .collection::<Document>("events")
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
                "_id", "project_id", "received_from", "received_to", "object_key",
                "format", "compression", "schema_version", "event_count", "state",
                "event_ids", "source_committed", "created_at",
            ],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "project_id": { "bsonType": "int", "minimum": 1 },
                "received_from": { "bsonType": "date" },
                "received_to": { "bsonType": "date" },
                "object_key": { "bsonType": "string", "minLength": 1, "maxLength": 512 },
                "format": { "enum": ["parquet"] },
                "compression": { "enum": ["zstd"] },
                "schema_version": { "bsonType": "int", "enum": [1] },
                "event_count": { "bsonType": "long", "minimum": 1, "maximum": 10000 },
                "stored_bytes": { "bsonType": "long", "minimum": 0 },
                "checksum": { "bsonType": "binData" },
                "state": { "enum": ["writing", "complete"] },
                "event_ids": {
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
                { "$eq": ["$event_count", { "$size": "$event_ids" }] },
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
            .keys(doc! { "source_committed": 1, "state": 1, "created_at": 1, "_id": 1 })
            .options(
                IndexOptions::builder()
                    .name("archive_resume".to_owned())
                    .partial_filter_expression(doc! { "source_committed": false })
                    .build(),
            )
            .build(),
        IndexModel::builder()
            .keys(doc! { "project_id": 1, "received_from": 1, "_id": 1 })
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
            doc! { "project_id": 1, "received_from": 1, "_id": 1 },
        ),
        (
            "archive_resume".to_owned(),
            doc! { "source_committed": 1, "state": 1, "created_at": 1, "_id": 1 },
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

fn manifest_event_keys(document: &Document) -> Result<Vec<EventKey>, ArchiveStoreError> {
    document
        .get_array("event_ids")
        .map_err(|_| ArchiveStoreError::InvalidData)?
        .iter()
        .map(|value| match value {
            Bson::Binary(value) if value.subtype == BinarySubtype::Generic => {
                let bytes: [u8; 20] = value
                    .bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| ArchiveStoreError::InvalidData)?;
                EventKey::from_bytes(bytes).map_err(|_| ArchiveStoreError::InvalidData)
            }
            _ => Err(ArchiveStoreError::InvalidData),
        })
        .collect()
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

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn is_duplicate(error: &mongodb::error::Error) -> bool {
    error.contains_label("DuplicateKey") || error.to_string().contains("E11000")
}
