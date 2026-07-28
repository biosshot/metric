use std::collections::BTreeSet;

use futures_util::TryStreamExt;
use metric_domain::{
    EventId, ProjectId, Timestamp,
    blob::{BlobChecksum, BlobKey, BlobKind, BlobObject},
    replays::{
        MAX_REPLAY_CORRELATIONS, MAX_REPLAY_SEGMENTS, MAX_REPLAY_TEXT_BYTES, ReplayCursor,
        ReplayPage, ReplayRecord, ReplaySegment, ReplaySegmentCommit,
    },
    signals::TraceId,
};
use metric_ports::{DurableOutcome, PortFuture, ReplayQuery, ReplayStore, SignalStoreError};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::IndexOptions,
};

#[derive(Debug, Clone, Copy)]
pub struct ReplayRetention {
    pub days: u32,
    pub archive: bool,
}

impl Default for ReplayRetention {
    fn default() -> Self {
        Self {
            days: 30,
            archive: false,
        }
    }
}

#[derive(Clone)]
pub struct MongoReplayStore {
    database: Database,
    retention: ReplayRetention,
}

impl MongoReplayStore {
    #[must_use]
    pub const fn new(database: Database, retention: ReplayRetention) -> Self {
        Self {
            database,
            retention,
        }
    }

    async fn persist(
        &self,
        commit: ReplaySegmentCommit,
    ) -> Result<DurableOutcome, SignalStoreError> {
        commit
            .metadata
            .validate()
            .map_err(|_| SignalStoreError::InvalidData)?;
        if self.retention.days == 0
            || commit.segment.segment_id != commit.metadata.segment_id
            || commit.segment.object.kind != BlobKind::ReplayRecording
        {
            return Err(SignalStoreError::InvalidData);
        }
        let relation = commit
            .segment
            .object
            .key
            .replay_relation()
            .map_err(|_| SignalStoreError::InvalidData)?;
        if relation
            != (
                commit.metadata.project_id,
                commit.metadata.replay_id,
                commit.metadata.segment_id,
            )
        {
            return Err(SignalStoreError::InvalidData);
        }
        let expires_at = Timestamp::from_unix_millis(
            commit.metadata.ended_at.unix_millis().saturating_add(
                i64::from(self.retention.days)
                    .saturating_mul(24 * 60 * 60)
                    .saturating_mul(1_000),
            ),
        )
        .map_err(|_| SignalStoreError::InvalidData)?;
        let identity = replay_key(commit.metadata.project_id, commit.metadata.replay_id);
        let mut set_on_insert = doc! {
            "_id": binary(identity),
            "p": commit.metadata.project_id.get(),
            "i": binary(commit.metadata.replay_id.as_bytes()),
        };
        if self.retention.archive {
            set_on_insert.insert("h", date(expires_at));
        } else {
            set_on_insert.insert("z", date(expires_at));
        }
        let mut current = doc! { "r": date(commit.metadata.received_at) };
        optional_string(&mut current, "v", &commit.metadata.release);
        optional_string(&mut current, "n", &commit.metadata.environment);
        optional_string(&mut current, "u", &commit.metadata.url);
        current.insert("x", id_array(&commit.metadata.error_ids));
        current.insert("t", trace_array(&commit.metadata.trace_ids));
        let segment = encode_segment(&commit.segment)?;
        let result = self
            .database
            .collection::<Document>("replays")
            .update_one(
                doc! {
                    "_id": binary(identity),
                    "sg.i": { "$ne": i64::from(commit.metadata.segment_id) },
                },
                doc! {
                    "$setOnInsert": set_on_insert,
                    "$set": current,
                    "$min": { "s": date(commit.metadata.started_at) },
                    "$max": { "e": date(commit.metadata.ended_at) },
                    "$push": { "sg": segment },
                },
            )
            .upsert(true)
            .await;
        match result {
            Ok(result) if result.upserted_id.is_some() || result.modified_count == 1 => {
                Ok(DurableOutcome::Accepted)
            }
            Ok(_) => Ok(DurableOutcome::Duplicate),
            Err(error)
                if error.contains_label("DuplicateKey") || error.to_string().contains("E11000") =>
            {
                Ok(DurableOutcome::Duplicate)
            }
            Err(_) => Err(SignalStoreError::Unavailable),
        }
    }

    async fn list(
        &self,
        project_id: ProjectId,
        query: ReplayQuery,
    ) -> Result<ReplayPage, SignalStoreError> {
        if query.limit == 0 || query.limit > 100 {
            return Err(SignalStoreError::InvalidData);
        }
        let mut filter = doc! { "p": project_id.get() };
        if let Some(before) = query.before {
            filter.insert(
                "$or",
                Bson::Array(vec![
                    Bson::Document(doc! { "r": { "$lt": date(before.received_at) } }),
                    Bson::Document(doc! {
                        "r": date(before.received_at),
                        "_id": { "$lt": binary(replay_key(project_id, before.replay_id)) },
                    }),
                ]),
            );
        }
        let mut cursor = self
            .database
            .collection::<Document>("replays")
            .find(filter)
            .sort(doc! { "r": -1, "_id": -1 })
            .limit(i64::try_from(query.limit.saturating_add(1)).unwrap_or(i64::MAX))
            .await
            .map_err(|_| SignalStoreError::Unavailable)?;
        let mut items = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| SignalStoreError::Unavailable)?
        {
            items.push(decode_replay(&document)?);
        }
        let next = if items.len() > query.limit {
            items.truncate(query.limit);
            items.last().map(|record| ReplayCursor {
                received_at: record.received_at,
                replay_id: record.replay_id,
            })
        } else {
            None
        };
        Ok(ReplayPage { items, next })
    }

    async fn load(
        &self,
        project_id: ProjectId,
        replay_id: EventId,
    ) -> Result<ReplayRecord, SignalStoreError> {
        let document = self
            .database
            .collection::<Document>("replays")
            .find_one(doc! { "_id": binary(replay_key(project_id, replay_id)) })
            .await
            .map_err(|_| SignalStoreError::Unavailable)?
            .ok_or(SignalStoreError::NotFound)?;
        decode_replay(&document)
    }
}

impl ReplayStore for MongoReplayStore {
    fn persist_replay_segment(
        &self,
        commit: ReplaySegmentCommit,
    ) -> PortFuture<'_, Result<DurableOutcome, SignalStoreError>> {
        Box::pin(self.persist(commit))
    }

    fn list_replays(
        &self,
        project_id: ProjectId,
        query: ReplayQuery,
    ) -> PortFuture<'_, Result<ReplayPage, SignalStoreError>> {
        Box::pin(self.list(project_id, query))
    }

    fn load_replay(
        &self,
        project_id: ProjectId,
        replay_id: EventId,
    ) -> PortFuture<'_, Result<ReplayRecord, SignalStoreError>> {
        Box::pin(self.load(project_id, replay_id))
    }

    fn references_replay_blob(
        &self,
        key: &BlobKey,
    ) -> PortFuture<'_, Result<bool, SignalStoreError>> {
        let key = key.as_str().to_owned();
        Box::pin(async move {
            self.database
                .collection::<Document>("replays")
                .find_one(doc! { "sg.k": key })
                .projection(doc! { "_id": 1 })
                .await
                .map(|value| value.is_some())
                .map_err(|_| SignalStoreError::Unavailable)
        })
    }
}

fn encode_segment(segment: &ReplaySegment) -> Result<Document, SignalStoreError> {
    Ok(doc! {
        "i": i64::from(segment.segment_id),
        "k": segment.object.key.as_str(),
        "b": i64::try_from(segment.object.size).map_err(|_| SignalStoreError::InvalidData)?,
        "d": binary(segment.object.checksum.as_bytes()),
        "a": date(segment.object.created_at),
        "o": i64::try_from(segment.decompressed_bytes).map_err(|_| SignalStoreError::InvalidData)?,
        "c": i64::from(segment.event_count),
    })
}

fn decode_replay(document: &Document) -> Result<ReplayRecord, SignalStoreError> {
    let project_id = ProjectId::new(
        document
            .get_i32("p")
            .map_err(|_| SignalStoreError::InvalidData)?,
    )
    .map_err(|_| SignalStoreError::InvalidData)?;
    let replay_id = EventId::from_bytes(binary_array::<16>(document, "i")?);
    if document
        .get_binary_generic("_id")
        .map_err(|_| SignalStoreError::InvalidData)?
        .as_slice()
        != replay_key(project_id, replay_id)
    {
        return Err(SignalStoreError::InvalidData);
    }
    let mut segments = document
        .get_array("sg")
        .map_err(|_| SignalStoreError::InvalidData)?
        .iter()
        .map(|value| {
            let value = value.as_document().ok_or(SignalStoreError::InvalidData)?;
            let segment_id = u32::try_from(
                value
                    .get_i64("i")
                    .map_err(|_| SignalStoreError::InvalidData)?,
            )
            .map_err(|_| SignalStoreError::InvalidData)?;
            let key = BlobKey::new(
                value
                    .get_str("k")
                    .map_err(|_| SignalStoreError::InvalidData)?
                    .to_owned(),
            )
            .map_err(|_| SignalStoreError::InvalidData)?;
            if key
                .replay_relation()
                .map_err(|_| SignalStoreError::InvalidData)?
                != (project_id, replay_id, segment_id)
            {
                return Err(SignalStoreError::InvalidData);
            }
            Ok(ReplaySegment {
                segment_id,
                object: BlobObject {
                    key,
                    kind: BlobKind::ReplayRecording,
                    size: u64::try_from(
                        value
                            .get_i64("b")
                            .map_err(|_| SignalStoreError::InvalidData)?,
                    )
                    .map_err(|_| SignalStoreError::InvalidData)?,
                    checksum: BlobChecksum::from_bytes(binary_array::<32>(value, "d")?),
                    created_at: timestamp(value, "a")?,
                },
                decompressed_bytes: u64::try_from(
                    value
                        .get_i64("o")
                        .map_err(|_| SignalStoreError::InvalidData)?,
                )
                .map_err(|_| SignalStoreError::InvalidData)?,
                event_count: u32::try_from(
                    value
                        .get_i64("c")
                        .map_err(|_| SignalStoreError::InvalidData)?,
                )
                .map_err(|_| SignalStoreError::InvalidData)?,
            })
        })
        .collect::<Result<Vec<_>, SignalStoreError>>()?;
    segments.sort_by_key(|segment| segment.segment_id);
    if segments.len() > MAX_REPLAY_SEGMENTS as usize {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(ReplayRecord {
        project_id,
        replay_id,
        started_at: timestamp(document, "s")?,
        ended_at: timestamp(document, "e")?,
        received_at: timestamp(document, "r")?,
        environment: optional_string_value(document, "n")?,
        release: optional_string_value(document, "v")?,
        url: optional_string_value(document, "u")?,
        error_ids: decode_ids(document.get_array("x").unwrap_or(&Vec::new()))?,
        trace_ids: decode_traces(document.get_array("t").unwrap_or(&Vec::new()))?,
        segments,
        expires_at: document
            .get_datetime("z")
            .or_else(|_| document.get_datetime("h"))
            .ok()
            .map(|value| Timestamp::from_unix_millis(value.timestamp_millis()))
            .transpose()
            .map_err(|_| SignalStoreError::InvalidData)?,
    })
}

pub(crate) fn replay_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "additionalProperties": false,
            "required": ["_id", "p", "i", "s", "e", "r", "sg"],
            "anyOf": [{ "required": ["z"] }, { "required": ["h"] }],
            "properties": {
                "_id": { "bsonType": "binData" },
                "p": { "bsonType": "int", "minimum": 1 },
                "i": { "bsonType": "binData" },
                "s": { "bsonType": "date" },
                "e": { "bsonType": "date" },
                "r": { "bsonType": "date" },
                "z": { "bsonType": "date" },
                "h": { "bsonType": "date" },
                "n": { "bsonType": "string", "maxLength": i32::try_from(MAX_REPLAY_TEXT_BYTES).unwrap_or(256) },
                "v": { "bsonType": "string", "maxLength": i32::try_from(MAX_REPLAY_TEXT_BYTES).unwrap_or(256) },
                "u": { "bsonType": "string", "maxLength": i32::try_from(MAX_REPLAY_TEXT_BYTES).unwrap_or(256) },
                "x": { "bsonType": "array", "maxItems": i32::try_from(MAX_REPLAY_CORRELATIONS).unwrap_or(100), "items": { "bsonType": "binData" } },
                "t": { "bsonType": "array", "maxItems": i32::try_from(MAX_REPLAY_CORRELATIONS).unwrap_or(100), "items": { "bsonType": "binData" } },
                "sg": {
                    "bsonType": "array",
                    "maxItems": i32::try_from(MAX_REPLAY_SEGMENTS).unwrap_or(100),
                    "items": {
                        "bsonType": "object",
                        "additionalProperties": false,
                        "required": ["i", "k", "b", "d", "a", "o", "c"],
                        "properties": {
                            "i": { "bsonType": "long", "minimum": 0, "maximum": i64::from(MAX_REPLAY_SEGMENTS - 1) },
                            "k": { "bsonType": "string", "maxLength": 512 },
                            "b": { "bsonType": "long", "minimum": 1 },
                            "d": { "bsonType": "binData" },
                            "a": { "bsonType": "date" },
                            "o": { "bsonType": "long", "minimum": 1 },
                            "c": { "bsonType": "long", "minimum": 0 }
                        }
                    }
                }
            }
        }
    }
}

pub(crate) async fn create_replay_indexes(database: &Database) -> mongodb::error::Result<()> {
    let collection = database.collection::<Document>("replays");
    for model in replay_indexes() {
        collection.create_index(model).await?;
    }
    Ok(())
}

pub(crate) fn replay_index_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "_id_",
        "replay_project_received",
        "replay_error_links",
        "replay_trace_links",
        "replay_retention",
        "replay_archive_due",
    ])
}

fn replay_indexes() -> [IndexModel; 5] {
    [
        named_index(
            doc! { "p": 1, "r": -1, "_id": -1 },
            "replay_project_received",
        ),
        named_index(doc! { "p": 1, "x": 1 }, "replay_error_links"),
        named_index(doc! { "p": 1, "t": 1 }, "replay_trace_links"),
        IndexModel::builder()
            .keys(doc! { "z": 1 })
            .options(
                IndexOptions::builder()
                    .name("replay_retention".to_owned())
                    .expire_after(std::time::Duration::ZERO)
                    .build(),
            )
            .build(),
        IndexModel::builder()
            .keys(doc! { "h": 1, "p": 1, "_id": 1 })
            .options(
                IndexOptions::builder()
                    .name("replay_archive_due".to_owned())
                    .partial_filter_expression(doc! { "h": { "$exists": true } })
                    .build(),
            )
            .build(),
    ]
}

fn named_index(keys: Document, name: &str) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(IndexOptions::builder().name(name.to_owned()).build())
        .build()
}

fn replay_key(project_id: ProjectId, replay_id: EventId) -> [u8; 20] {
    let mut bytes = [0_u8; 20];
    bytes[..4].copy_from_slice(&project_id.get().to_be_bytes());
    bytes[4..].copy_from_slice(&replay_id.as_bytes());
    bytes
}

fn id_array(values: &[EventId]) -> Bson {
    Bson::Array(
        values
            .iter()
            .map(|value| Bson::Binary(raw_binary(value.as_bytes())))
            .collect(),
    )
}

fn trace_array(values: &[TraceId]) -> Bson {
    Bson::Array(
        values
            .iter()
            .map(|value| Bson::Binary(raw_binary(value.as_bytes())))
            .collect(),
    )
}

fn decode_ids(values: &[Bson]) -> Result<Vec<EventId>, SignalStoreError> {
    values
        .iter()
        .map(|value| match value {
            Bson::Binary(value) if value.subtype == BinarySubtype::Generic => {
                Ok(EventId::from_bytes(
                    value
                        .bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| SignalStoreError::InvalidData)?,
                ))
            }
            _ => Err(SignalStoreError::InvalidData),
        })
        .collect()
}

fn decode_traces(values: &[Bson]) -> Result<Vec<TraceId>, SignalStoreError> {
    values
        .iter()
        .map(|value| match value {
            Bson::Binary(value) if value.subtype == BinarySubtype::Generic => {
                Ok(TraceId::from_bytes(
                    value
                        .bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| SignalStoreError::InvalidData)?,
                ))
            }
            _ => Err(SignalStoreError::InvalidData),
        })
        .collect()
}

fn optional_string(document: &mut Document, key: &str, value: &Option<Box<str>>) {
    if let Some(value) = value {
        document.insert(key, value.as_ref());
    }
}

fn optional_string_value(
    document: &Document,
    key: &str,
) -> Result<Option<Box<str>>, SignalStoreError> {
    document
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(Into::into)
                .ok_or(SignalStoreError::InvalidData)
        })
        .transpose()
}

fn timestamp(document: &Document, key: &str) -> Result<Timestamp, SignalStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(key)
            .map_err(|_| SignalStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| SignalStoreError::InvalidData)
}

fn binary_array<const N: usize>(
    document: &Document,
    key: &str,
) -> Result<[u8; N], SignalStoreError> {
    document
        .get_binary_generic(key)
        .map_err(|_| SignalStoreError::InvalidData)?
        .as_slice()
        .try_into()
        .map_err(|_| SignalStoreError::InvalidData)
}

fn raw_binary(bytes: impl AsRef<[u8]>) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.as_ref().to_vec(),
    }
}

fn binary(bytes: impl AsRef<[u8]>) -> Bson {
    Bson::Binary(raw_binary(bytes))
}

fn date(value: Timestamp) -> DateTime {
    DateTime::from_millis(value.unix_millis())
}
