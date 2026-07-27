//! Compact MongoDB User Feedback persistence.

use std::{collections::BTreeSet, time::Duration};

use futures_util::TryStreamExt;
use metric_domain::{
    EventId, EventKey, ProjectId, Timestamp,
    blob::{
        AttachmentFilename, BlobChecksum, BlobContentType, BlobKey, BlobKind, BlobObject,
        BlobObjectId, EventAttachment,
    },
    feedback::{
        FeedbackAnchor, FeedbackPage, FeedbackRecord, FeedbackStatus, MAX_FEEDBACK_ATTACHMENTS,
        MAX_FEEDBACK_CONTACT_BYTES, MAX_FEEDBACK_MESSAGE_BYTES, MAX_FEEDBACK_NAME_BYTES,
        MAX_FEEDBACK_URL_BYTES,
    },
    signals::TraceId,
};
use metric_ports::{
    DurableOutcome, FeedbackQuery, FeedbackSink, FeedbackStore, FeedbackStoreError, PortFuture,
};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    error::{ErrorKind, WriteFailure},
    options::{FindOneAndUpdateOptions, FindOptions, IndexOptions, ReturnDocument},
};

#[derive(Clone)]
pub struct MongoFeedbackStore {
    database: Database,
}

impl MongoFeedbackStore {
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }

    async fn persist(
        &self,
        feedback: FeedbackRecord,
    ) -> Result<DurableOutcome, FeedbackStoreError> {
        let document = encode_feedback(&feedback)?;
        match self
            .database
            .collection::<Document>("feedback")
            .insert_one(document.clone())
            .await
        {
            Ok(_) => Ok(DurableOutcome::Accepted),
            Err(error) if duplicate_key(error.kind.as_ref()) => {
                let id = document
                    .get("_id")
                    .cloned()
                    .ok_or(FeedbackStoreError::InvalidData)?;
                let existing = self
                    .database
                    .collection::<Document>("feedback")
                    .find_one(doc! { "_id": id })
                    .await
                    .map_err(|_| FeedbackStoreError::Unavailable)?
                    .ok_or(FeedbackStoreError::Unavailable)?;
                let existing = decode_feedback(&existing)?;
                if same_submission(&existing, &feedback) {
                    Ok(DurableOutcome::Duplicate)
                } else {
                    Err(FeedbackStoreError::Conflict)
                }
            }
            Err(_) => Err(FeedbackStoreError::Unavailable),
        }
    }

    async fn list(
        &self,
        project_id: ProjectId,
        query: FeedbackQuery,
    ) -> Result<FeedbackPage, FeedbackStoreError> {
        if query.limit == 0 || query.limit > 100 {
            return Err(FeedbackStoreError::InvalidData);
        }
        let mut filter = doc! { "p": project_id.get() };
        if let Some(status) = query.status {
            filter.insert("s", status.as_str());
        }
        if let Some(before) = query.before {
            let key = EventKey::new(project_id, before.feedback_id);
            filter.insert(
                "$or",
                vec![
                    doc! { "a": { "$lt": date(before.received_at) } },
                    doc! {
                        "a": date(before.received_at),
                        "_id": { "$lt": binary(key.as_bytes()) },
                    },
                ],
            );
        }
        let options = FindOptions::builder()
            .sort(doc! { "a": -1, "_id": -1 })
            .limit(i64::try_from(query.limit + 1).map_err(|_| FeedbackStoreError::InvalidData)?)
            .build();
        let mut cursor = self
            .database
            .collection::<Document>("feedback")
            .find(filter)
            .with_options(options)
            .await
            .map_err(|_| FeedbackStoreError::Unavailable)?;
        let mut items = Vec::with_capacity(query.limit + 1);
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| FeedbackStoreError::Unavailable)?
        {
            items.push(decode_feedback(&document)?);
        }
        let next = if items.len() > query.limit {
            items.pop();
            items.last().map(|feedback| FeedbackAnchor {
                received_at: feedback.received_at,
                feedback_id: feedback.feedback_id,
            })
        } else {
            None
        };
        Ok(FeedbackPage { items, next })
    }

    async fn load(
        &self,
        project_id: ProjectId,
        feedback_id: EventId,
    ) -> Result<FeedbackRecord, FeedbackStoreError> {
        let key = EventKey::new(project_id, feedback_id);
        self.database
            .collection::<Document>("feedback")
            .find_one(doc! { "_id": binary(key.as_bytes()), "p": project_id.get() })
            .await
            .map_err(|_| FeedbackStoreError::Unavailable)?
            .ok_or(FeedbackStoreError::NotFound)
            .and_then(|document| decode_feedback(&document))
    }

    async fn update_status(
        &self,
        project_id: ProjectId,
        feedback_id: EventId,
        status: FeedbackStatus,
        changed_at: Timestamp,
    ) -> Result<FeedbackRecord, FeedbackStoreError> {
        let current = self.load(project_id, feedback_id).await?;
        if current.status == status {
            return Ok(current);
        }
        if changed_at.unix_millis() < current.status_changed_at.unix_millis() {
            return Err(FeedbackStoreError::Conflict);
        }
        let key = EventKey::new(project_id, feedback_id);
        let options = FindOneAndUpdateOptions::builder()
            .return_document(ReturnDocument::After)
            .build();
        self.database
            .collection::<Document>("feedback")
            .find_one_and_update(
                doc! {
                    "_id": binary(key.as_bytes()),
                    "p": project_id.get(),
                    "u": date(current.status_changed_at),
                },
                doc! { "$set": { "s": status.as_str(), "u": date(changed_at) } },
            )
            .with_options(options)
            .await
            .map_err(|_| FeedbackStoreError::Unavailable)?
            .ok_or(FeedbackStoreError::Conflict)
            .and_then(|document| decode_feedback(&document))
    }
}

fn same_submission(existing: &FeedbackRecord, submitted: &FeedbackRecord) -> bool {
    existing.project_id == submitted.project_id
        && existing.feedback_id == submitted.feedback_id
        && existing.message == submitted.message
        && existing.name == submitted.name
        && existing.contact_email == submitted.contact_email
        && existing.url == submitted.url
        && existing.associated_event_id == submitted.associated_event_id
        && existing.trace_id == submitted.trace_id
        && existing.replay_id == submitted.replay_id
        && existing.attachments.len() == submitted.attachments.len()
        && existing
            .attachments
            .iter()
            .zip(&submitted.attachments)
            .all(|(left, right)| {
                left.attachment_id == right.attachment_id
                    && left.blob.key == right.blob.key
                    && left.blob.kind == right.blob.kind
                    && left.blob.size == right.blob.size
                    && left.blob.checksum == right.blob.checksum
                    && left.filename == right.filename
                    && left.content_type == right.content_type
                    && left.attachment_type == right.attachment_type
            })
}

impl FeedbackSink for MongoFeedbackStore {
    fn persist_feedback(
        &self,
        feedback: FeedbackRecord,
    ) -> PortFuture<'_, Result<DurableOutcome, FeedbackStoreError>> {
        Box::pin(self.persist(feedback))
    }
}

impl FeedbackStore for MongoFeedbackStore {
    fn list_feedback(
        &self,
        project_id: ProjectId,
        query: FeedbackQuery,
    ) -> PortFuture<'_, Result<FeedbackPage, FeedbackStoreError>> {
        Box::pin(self.list(project_id, query))
    }

    fn load_feedback(
        &self,
        project_id: ProjectId,
        feedback_id: EventId,
    ) -> PortFuture<'_, Result<FeedbackRecord, FeedbackStoreError>> {
        Box::pin(self.load(project_id, feedback_id))
    }

    fn update_feedback_status(
        &self,
        project_id: ProjectId,
        feedback_id: EventId,
        status: FeedbackStatus,
        changed_at: Timestamp,
    ) -> PortFuture<'_, Result<FeedbackRecord, FeedbackStoreError>> {
        Box::pin(self.update_status(project_id, feedback_id, status, changed_at))
    }
}

pub(crate) fn encode_feedback(feedback: &FeedbackRecord) -> Result<Document, FeedbackStoreError> {
    feedback
        .validate()
        .map_err(|_| FeedbackStoreError::InvalidData)?;
    let key = EventKey::new(feedback.project_id, feedback.feedback_id);
    let mut document = doc! {
        "_id": binary(key.as_bytes()),
        "p": feedback.project_id.get(),
        "a": date(feedback.received_at),
        "s": feedback.status.as_str(),
        "u": date(feedback.status_changed_at),
        "m": feedback.message.as_ref(),
        "x": date(feedback.expires_at),
    };
    optional_string(&mut document, "n", &feedback.name);
    optional_string(&mut document, "c", &feedback.contact_email);
    optional_string(&mut document, "w", &feedback.url);
    if let Some(event_id) = feedback.associated_event_id {
        document.insert("e", binary(event_id.as_bytes()));
    }
    if let Some(trace_id) = feedback.trace_id {
        document.insert("t", binary(trace_id.as_bytes()));
    }
    if let Some(replay_id) = feedback.replay_id {
        document.insert("r", binary(replay_id.as_bytes()));
    }
    if !feedback.attachments.is_empty() {
        let mut attachments = Vec::with_capacity(feedback.attachments.len());
        for attachment in &feedback.attachments {
            let (project_id, event_id, object_id) = attachment
                .blob
                .key
                .event_relation()
                .map_err(|_| FeedbackStoreError::InvalidData)?;
            if project_id != feedback.project_id
                || event_id != feedback.feedback_id
                || object_id != attachment.attachment_id
                || attachment.blob.kind != BlobKind::EventAttachment
            {
                return Err(FeedbackStoreError::InvalidData);
            }
            attachments.push(Bson::Document(doc! {
                "i": binary(attachment.attachment_id.as_bytes()),
                "k": attachment.blob.key.as_str(),
                "f": attachment.filename.as_str(),
                "c": attachment.content_type.as_str(),
                "t": attachment.attachment_type.as_ref(),
                "s": i64::try_from(attachment.blob.size).map_err(|_| FeedbackStoreError::InvalidData)?,
                "d": binary(attachment.blob.checksum.as_bytes()),
                "a": date(attachment.blob.created_at),
            }));
        }
        document.insert("b", attachments);
    }
    Ok(document)
}

pub(crate) fn decode_feedback(document: &Document) -> Result<FeedbackRecord, FeedbackStoreError> {
    let project_id = ProjectId::new(
        document
            .get_i32("p")
            .map_err(|_| FeedbackStoreError::InvalidData)?,
    )
    .map_err(|_| FeedbackStoreError::InvalidData)?;
    let key = document
        .get_binary_generic("_id")
        .map_err(|_| FeedbackStoreError::InvalidData)?;
    if key.len() != 20 || key[..4] != project_id.get().to_be_bytes() {
        return Err(FeedbackStoreError::InvalidData);
    }
    let feedback_id = EventId::from_bytes(
        key[4..]
            .try_into()
            .map_err(|_| FeedbackStoreError::InvalidData)?,
    );
    let mut feedback = FeedbackRecord {
        project_id,
        feedback_id,
        received_at: timestamp(document, "a")?,
        status: FeedbackStatus::parse(
            document
                .get_str("s")
                .map_err(|_| FeedbackStoreError::InvalidData)?,
        )
        .map_err(|_| FeedbackStoreError::InvalidData)?,
        status_changed_at: timestamp(document, "u")?,
        message: document
            .get_str("m")
            .map_err(|_| FeedbackStoreError::InvalidData)?
            .into(),
        name: optional_boxed(document, "n")?,
        contact_email: optional_boxed(document, "c")?,
        url: optional_boxed(document, "w")?,
        associated_event_id: optional_id(document, "e")?,
        issue_id: None,
        trace_id: optional_trace_id(document, "t")?,
        replay_id: optional_id(document, "r")?,
        attachments: Vec::new(),
        expires_at: timestamp(document, "x")?,
    };
    if let Ok(items) = document.get_array("b") {
        for item in items {
            let item = item.as_document().ok_or(FeedbackStoreError::InvalidData)?;
            let attachment_id = BlobObjectId::from_bytes(binary_array::<16>(item, "i")?);
            feedback.attachments.push(EventAttachment {
                attachment_id,
                blob: BlobObject {
                    key: BlobKey::new(
                        item.get_str("k")
                            .map_err(|_| FeedbackStoreError::InvalidData)?
                            .to_owned(),
                    )
                    .map_err(|_| FeedbackStoreError::InvalidData)?,
                    kind: BlobKind::EventAttachment,
                    size: u64::try_from(
                        item.get_i64("s")
                            .map_err(|_| FeedbackStoreError::InvalidData)?,
                    )
                    .map_err(|_| FeedbackStoreError::InvalidData)?,
                    checksum: BlobChecksum::from_bytes(binary_array::<32>(item, "d")?),
                    created_at: timestamp(item, "a")?,
                },
                filename: AttachmentFilename::sanitized(
                    item.get_str("f")
                        .map_err(|_| FeedbackStoreError::InvalidData)?,
                )
                .map_err(|_| FeedbackStoreError::InvalidData)?,
                content_type: BlobContentType::new(
                    item.get_str("c")
                        .map_err(|_| FeedbackStoreError::InvalidData)?,
                )
                .map_err(|_| FeedbackStoreError::InvalidData)?,
                attachment_type: item
                    .get_str("t")
                    .map_err(|_| FeedbackStoreError::InvalidData)?
                    .into(),
            });
        }
    }
    feedback
        .validate()
        .map_err(|_| FeedbackStoreError::InvalidData)?;
    encode_feedback(&feedback)?;
    Ok(feedback)
}

pub(crate) fn feedback_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "additionalProperties": false,
            "required": ["_id", "p", "a", "s", "u", "m", "x"],
            "properties": {
                "_id": { "bsonType": "binData" },
                "p": { "bsonType": "int", "minimum": 1 },
                "a": { "bsonType": "date" },
                "s": { "enum": ["open", "resolved", "spam"] },
                "u": { "bsonType": "date" },
                "m": { "bsonType": "string", "minLength": 1, "maxLength": i32::try_from(MAX_FEEDBACK_MESSAGE_BYTES).unwrap_or(16_384) },
                "n": { "bsonType": "string", "minLength": 1, "maxLength": i32::try_from(MAX_FEEDBACK_NAME_BYTES).unwrap_or(512) },
                "c": { "bsonType": "string", "minLength": 1, "maxLength": i32::try_from(MAX_FEEDBACK_CONTACT_BYTES).unwrap_or(512) },
                "w": { "bsonType": "string", "minLength": 1, "maxLength": i32::try_from(MAX_FEEDBACK_URL_BYTES).unwrap_or(2_048) },
                "e": { "bsonType": "binData" },
                "t": { "bsonType": "binData" },
                "r": { "bsonType": "binData" },
                "b": {
                    "bsonType": "array",
                    "maxItems": i32::try_from(MAX_FEEDBACK_ATTACHMENTS).unwrap_or(10),
                    "items": {
                        "bsonType": "object",
                        "additionalProperties": false,
                        "required": ["i", "k", "f", "c", "t", "s", "d", "a"],
                        "properties": {
                            "i": { "bsonType": "binData" },
                            "k": { "bsonType": "string", "minLength": 1, "maxLength": 512 },
                            "f": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
                            "c": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
                            "t": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
                            "s": { "bsonType": "long", "minimum": 0 },
                            "d": { "bsonType": "binData" },
                            "a": { "bsonType": "date" },
                        },
                    },
                },
                "x": { "bsonType": "date" },
            },
        },
    }
}

pub(crate) fn feedback_indexes() -> Vec<IndexModel> {
    vec![
        index(
            doc! { "p": 1, "a": -1, "_id": -1 },
            "feedback_project_timeline",
        ),
        index(
            doc! { "p": 1, "s": 1, "a": -1, "_id": -1 },
            "feedback_project_status_timeline",
        ),
        IndexModel::builder()
            .keys(doc! { "x": 1 })
            .options(
                IndexOptions::builder()
                    .name("feedback_expiry_ttl".to_owned())
                    .expire_after(Duration::ZERO)
                    .build(),
            )
            .build(),
    ]
}

pub(crate) fn feedback_index_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "_id_",
        "feedback_expiry_ttl",
        "feedback_project_status_timeline",
        "feedback_project_timeline",
    ])
}

fn index(keys: Document, name: &str) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(IndexOptions::builder().name(name.to_owned()).build())
        .build()
}

fn duplicate_key(kind: &ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::Write(WriteFailure::WriteError(error)) if error.code == 11000
    )
}

fn optional_string(document: &mut Document, key: &str, value: &Option<Box<str>>) {
    if let Some(value) = value {
        document.insert(key, value.as_ref());
    }
}

fn optional_boxed(document: &Document, key: &str) -> Result<Option<Box<str>>, FeedbackStoreError> {
    match document.get(key) {
        None => Ok(None),
        Some(Bson::String(value)) => Ok(Some(value.as_str().into())),
        _ => Err(FeedbackStoreError::InvalidData),
    }
}

fn optional_id(document: &Document, key: &str) -> Result<Option<EventId>, FeedbackStoreError> {
    match document.get(key) {
        None => Ok(None),
        Some(Bson::Binary(value)) if value.bytes.len() == 16 => Ok(Some(EventId::from_bytes(
            value
                .bytes
                .as_slice()
                .try_into()
                .map_err(|_| FeedbackStoreError::InvalidData)?,
        ))),
        _ => Err(FeedbackStoreError::InvalidData),
    }
}

fn optional_trace_id(
    document: &Document,
    key: &str,
) -> Result<Option<TraceId>, FeedbackStoreError> {
    optional_id(document, key).map(|value| value.map(|id| TraceId::from_bytes(id.as_bytes())))
}

fn binary_array<const N: usize>(
    document: &Document,
    key: &str,
) -> Result<[u8; N], FeedbackStoreError> {
    document
        .get_binary_generic(key)
        .map_err(|_| FeedbackStoreError::InvalidData)?
        .as_slice()
        .try_into()
        .map_err(|_| FeedbackStoreError::InvalidData)
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

fn timestamp(document: &Document, key: &str) -> Result<Timestamp, FeedbackStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(key)
            .map_err(|_| FeedbackStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| FeedbackStoreError::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_is_compact_and_round_trips_optional_fields() {
        let received_at = Timestamp::from_unix_millis(1_700_000_000_000).unwrap();
        let feedback = FeedbackRecord {
            project_id: ProjectId::new(7).unwrap(),
            feedback_id: EventId::from_bytes([3; 16]),
            received_at,
            status: FeedbackStatus::Open,
            status_changed_at: received_at,
            message: "Checkout did not respond".into(),
            name: Some("Ada".into()),
            contact_email: Some("ada@example.com".into()),
            url: None,
            associated_event_id: Some(EventId::from_bytes([4; 16])),
            issue_id: None,
            trace_id: Some(TraceId::from_bytes([5; 16])),
            replay_id: None,
            attachments: Vec::new(),
            expires_at: Timestamp::from_unix_millis(
                received_at.unix_millis() + 90 * 24 * 60 * 60 * 1000,
            )
            .unwrap(),
        };
        let document = encode_feedback(&feedback).unwrap();
        assert!(mongodb::bson::to_vec(&document).unwrap().len() < 512);
        assert_eq!(decode_feedback(&document).unwrap(), feedback);
        assert!(!document.contains_key("w"));
        assert!(!document.contains_key("b"));
    }
}
