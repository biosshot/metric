//! Project-scoped feedback, workflow and Blob references committed together.
use crate::{PostgresDatabase, codec, signal_storage};
use metric_domain::{
    EventId, ProjectId, Timestamp,
    blob::{
        AttachmentFilename, BlobChecksum, BlobContentType, BlobKey, BlobKind, BlobObject,
        BlobObjectId, EventAttachment,
    },
    feedback::{FeedbackAnchor, FeedbackPage, FeedbackRecord, FeedbackStatus},
    signals::TraceId,
};
use metric_ports::{
    DurableOutcome, FeedbackQuery, FeedbackSink, FeedbackStore, FeedbackStoreError, PortFuture,
    SignalStoreError,
};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};
use std::collections::BTreeMap;

const COLUMNS: &str = "SELECT project_id,feedback_id,received_at,status,status_changed_at,message,name,contact_email,url,associated_event_id,trace_id,replay_id,expire_at FROM metric.feedback";

#[derive(Clone)]
pub struct PostgresFeedbackStore {
    database: PostgresDatabase,
}
impl PostgresFeedbackStore {
    #[must_use]
    pub const fn new(database: PostgresDatabase) -> Self {
        Self { database }
    }
    async fn persist(
        &self,
        feedback: FeedbackRecord,
    ) -> Result<DurableOutcome, FeedbackStoreError> {
        validate(&feedback)?;
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        lock_project(&mut transaction, feedback.project_id).await?;
        let inserted=sqlx::query("INSERT INTO metric.feedback(project_id,feedback_id,received_at,status,status_changed_at,message,name,contact_email,url,associated_event_id,trace_id,replay_id,expire_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) ON CONFLICT(project_id,feedback_id) DO NOTHING")
            .bind(feedback.project_id.get()).bind(feedback.feedback_id.as_bytes().as_slice()).bind(feedback.received_at.unix_millis()).bind(status_code(feedback.status)).bind(feedback.status_changed_at.unix_millis())
            .bind(feedback.message.as_bytes()).bind(feedback.name.as_deref().map(str::as_bytes)).bind(feedback.contact_email.as_deref().map(str::as_bytes)).bind(feedback.url.as_deref().map(str::as_bytes))
            .bind(feedback.associated_event_id.map(|id|id.as_bytes().to_vec())).bind(feedback.trace_id.map(|id|id.as_bytes().to_vec())).bind(feedback.replay_id.map(|id|id.as_bytes().to_vec())).bind(feedback.expires_at.unix_millis())
            .execute(&mut *transaction).await.map_err(write_error)?.rows_affected()==1;
        let row = select()
            .push(" WHERE project_id=")
            .push_bind(feedback.project_id.get())
            .push(" AND feedback_id=")
            .push_bind(feedback.feedback_id.as_bytes().to_vec())
            .push(" FOR UPDATE")
            .build()
            .fetch_one(&mut *transaction)
            .await
            .map_err(unavailable)?;
        if !inserted {
            let mut current = decode(&row)?;
            fill_attachments(&mut transaction, std::slice::from_mut(&mut current)).await?;
            // A retry cannot reset workflow, original retention or receive times.
            if !same_submission(&current, &feedback) {
                return Err(FeedbackStoreError::Conflict);
            }
            transaction.commit().await.map_err(unavailable)?;
            return Ok(DurableOutcome::Duplicate);
        }
        for (ordinal, attachment) in feedback.attachments.iter().enumerate() {
            sqlx::query("INSERT INTO metric.feedback_attachments(project_id,feedback_id,ordinal,attachment_id,blob_key,size,checksum,created_at,filename,content_type,attachment_type) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
                .bind(feedback.project_id.get()).bind(feedback.feedback_id.as_bytes().as_slice()).bind(ordinal as i16).bind(attachment.attachment_id.as_bytes().as_slice()).bind(attachment.blob.key.as_str()).bind(i64::try_from(attachment.blob.size).map_err(invalid)?).bind(attachment.blob.checksum.as_bytes().as_slice()).bind(attachment.blob.created_at.unix_millis()).bind(attachment.filename.as_str().as_bytes()).bind(attachment.content_type.as_str().as_bytes()).bind(attachment.attachment_type.as_bytes()).execute(&mut *transaction).await.map_err(write_error)?;
            sqlx::query("INSERT INTO metric.blob_references(project_id,event_id,object_id,source_kind,blob_key) VALUES($1,$2,$3,1,$4) ON CONFLICT(project_id,event_id,object_id,source_kind) DO NOTHING")
                .bind(feedback.project_id.get()).bind(feedback.feedback_id.as_bytes().as_slice()).bind(attachment.attachment_id.as_bytes().as_slice()).bind(attachment.blob.key.as_str()).execute(&mut *transaction).await.map_err(write_error)?;
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(DurableOutcome::Accepted)
    }
    async fn read(
        &self,
        project: ProjectId,
        id: Option<EventId>,
        query: Option<FeedbackQuery>,
    ) -> Result<Vec<FeedbackRecord>, FeedbackStoreError> {
        let mut selection = select();
        selection
            .push(" WHERE project_id=")
            .push_bind(project.get());
        if let Some(id) = id {
            selection
                .push(" AND feedback_id=")
                .push_bind(id.as_bytes().to_vec());
        }
        if let Some(query) = query {
            if query.limit == 0 || query.limit > 100 {
                return Err(FeedbackStoreError::InvalidData);
            }
            if let Some(status) = query.status {
                selection
                    .push(" AND status=")
                    .push_bind(status_code(status));
            }
            if let Some(event) = query.event_id {
                selection
                    .push(" AND associated_event_id=")
                    .push_bind(event.as_bytes().to_vec());
            }
            if let Some(trace) = query.trace_id {
                selection
                    .push(" AND trace_id=")
                    .push_bind(trace.as_bytes().to_vec());
            }
            if let Some(replay) = query.replay_id {
                selection
                    .push(" AND replay_id=")
                    .push_bind(replay.as_bytes().to_vec());
            }
            if let Some(before) = query.before {
                selection
                    .push(" AND (received_at,feedback_id)<(")
                    .push_bind(before.received_at.unix_millis())
                    .push(",")
                    .push_bind(before.feedback_id.as_bytes().to_vec())
                    .push(")");
            }
        }
        selection
            .push(" ORDER BY received_at DESC,feedback_id DESC LIMIT ")
            .push_bind(query.map_or(1, |q| q.limit + 1) as i64);
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
        // Parent rows and their at-most-ten attachments share one snapshot.
        let rows = selection
            .build()
            .fetch_all(&mut *transaction)
            .await
            .map_err(unavailable)?;
        let mut records = rows.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
        fill_attachments(&mut transaction, &mut records).await?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(records)
    }
    async fn status(
        &self,
        project: ProjectId,
        id: EventId,
        status: FeedbackStatus,
        changed: Timestamp,
    ) -> Result<FeedbackRecord, FeedbackStoreError> {
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        lock_project(&mut transaction, project).await?;
        let row = select()
            .push(" WHERE project_id=")
            .push_bind(project.get())
            .push(" AND feedback_id=")
            .push_bind(id.as_bytes().to_vec())
            .push(" FOR UPDATE")
            .build()
            .fetch_optional(&mut *transaction)
            .await
            .map_err(unavailable)?
            .ok_or(FeedbackStoreError::NotFound)?;
        let mut record = decode(&row)?;
        fill_attachments(&mut transaction, std::slice::from_mut(&mut record)).await?;
        if record.status != status {
            if changed < record.status_changed_at {
                return Err(FeedbackStoreError::Conflict);
            }
            sqlx::query("UPDATE metric.feedback SET status=$3,status_changed_at=$4 WHERE project_id=$1 AND feedback_id=$2")
                .bind(project.get()).bind(id.as_bytes().as_slice()).bind(status_code(status)).bind(changed.unix_millis()).execute(&mut *transaction).await.map_err(write_error)?;
            record.status = status;
            record.status_changed_at = changed;
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(record)
    }
}
impl FeedbackSink for PostgresFeedbackStore {
    fn persist_feedback(
        &self,
        feedback: FeedbackRecord,
    ) -> PortFuture<'_, Result<DurableOutcome, FeedbackStoreError>> {
        Box::pin(self.persist(feedback))
    }
}
impl FeedbackStore for PostgresFeedbackStore {
    fn list_feedback(
        &self,
        project: ProjectId,
        query: FeedbackQuery,
    ) -> PortFuture<'_, Result<FeedbackPage, FeedbackStoreError>> {
        Box::pin(async move {
            let mut items = self.read(project, None, Some(query)).await?;
            let next = if items.len() > query.limit {
                items.pop();
                items.last().map(|r| FeedbackAnchor {
                    received_at: r.received_at,
                    feedback_id: r.feedback_id,
                })
            } else {
                None
            };
            Ok(FeedbackPage { items, next })
        })
    }
    fn load_feedback(
        &self,
        project: ProjectId,
        id: EventId,
    ) -> PortFuture<'_, Result<FeedbackRecord, FeedbackStoreError>> {
        Box::pin(async move {
            self.read(project, Some(id), None)
                .await?
                .pop()
                .ok_or(FeedbackStoreError::NotFound)
        })
    }
    fn update_feedback_status(
        &self,
        project: ProjectId,
        id: EventId,
        status: FeedbackStatus,
        changed: Timestamp,
    ) -> PortFuture<'_, Result<FeedbackRecord, FeedbackStoreError>> {
        Box::pin(self.status(project, id, status, changed))
    }
}
fn select() -> QueryBuilder<Postgres> {
    QueryBuilder::new(COLUMNS)
}
async fn lock_project(
    connection: &mut PgConnection,
    project: ProjectId,
) -> Result<(), FeedbackStoreError> {
    signal_storage::lock_projects(connection, &[project.get()])
        .await
        .map_err(|error| match error {
            SignalStoreError::NotFound => FeedbackStoreError::NotFound,
            SignalStoreError::Unavailable => FeedbackStoreError::Unavailable,
            SignalStoreError::Conflict => FeedbackStoreError::Conflict,
            SignalStoreError::InvalidData => FeedbackStoreError::InvalidData,
            SignalStoreError::Capacity => FeedbackStoreError::Capacity,
        })
}
fn validate(record: &FeedbackRecord) -> Result<(), FeedbackStoreError> {
    record.validate().map_err(invalid)?;
    for attachment in &record.attachments {
        if attachment.blob.kind != BlobKind::EventAttachment
            || attachment.blob.key
                != BlobKey::event_owned(
                    record.project_id,
                    record.feedback_id,
                    attachment.attachment_id,
                )
            || attachment.attachment_type.is_empty()
            || attachment.attachment_type.chars().count() > 128
            || i64::try_from(attachment.blob.size).is_err()
        {
            return Err(FeedbackStoreError::InvalidData);
        }
    }
    Ok(())
}
fn same_submission(left: &FeedbackRecord, right: &FeedbackRecord) -> bool {
    left.project_id == right.project_id
        && left.feedback_id == right.feedback_id
        && left.message == right.message
        && left.name == right.name
        && left.contact_email == right.contact_email
        && left.url == right.url
        && left.associated_event_id == right.associated_event_id
        && left.trace_id == right.trace_id
        && left.replay_id == right.replay_id
        && left.attachments.len() == right.attachments.len()
        && left
            .attachments
            .iter()
            .zip(&right.attachments)
            .all(|(a, b)| {
                a.attachment_id == b.attachment_id
                    && a.blob.key == b.blob.key
                    && a.blob.kind == b.blob.kind
                    && a.blob.size == b.blob.size
                    && a.blob.checksum == b.blob.checksum
                    && a.filename == b.filename
                    && a.content_type == b.content_type
                    && a.attachment_type == b.attachment_type
            })
}
async fn fill_attachments(
    connection: &mut PgConnection,
    records: &mut [FeedbackRecord],
) -> Result<(), FeedbackStoreError> {
    if records.is_empty() {
        return Ok(());
    }
    let project = records[0].project_id;
    let ids: Vec<_> = records
        .iter()
        .map(|r| r.feedback_id.as_bytes().to_vec())
        .collect();
    let rows=sqlx::query("SELECT project_id,feedback_id,ordinal,attachment_id,blob_key,size,checksum,created_at,filename,content_type,attachment_type FROM metric.feedback_attachments WHERE project_id=$1 AND feedback_id=ANY($2::bytea[]) ORDER BY feedback_id,ordinal LIMIT 1010").bind(project.get()).bind(&ids).fetch_all(connection).await.map_err(unavailable)?;
    let mut attachments = BTreeMap::<[u8; 16], Vec<EventAttachment>>::new();
    for row in rows {
        attachments
            .entry(fixed(&row, "feedback_id")?)
            .or_default()
            .push(decode_attachment(&row)?);
    }
    for record in records {
        record.attachments = attachments
            .remove(&record.feedback_id.as_bytes())
            .unwrap_or_default();
        validate(record)?;
    }
    if !attachments.is_empty() {
        return Err(FeedbackStoreError::InvalidData);
    }
    Ok(())
}
fn decode(row: &PgRow) -> Result<FeedbackRecord, FeedbackStoreError> {
    let record = FeedbackRecord {
        project_id: ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?,
        feedback_id: EventId::from_bytes(fixed(row, "feedback_id")?),
        received_at: timestamp(row, "received_at")?,
        status: match row.try_get::<i16, _>("status").map_err(invalid)? {
            0 => FeedbackStatus::Open,
            1 => FeedbackStatus::Resolved,
            2 => FeedbackStatus::Spam,
            _ => return Err(FeedbackStoreError::InvalidData),
        },
        status_changed_at: timestamp(row, "status_changed_at")?,
        message: required_text(row, "message")?,
        name: text(row, "name")?,
        contact_email: text(row, "contact_email")?,
        url: text(row, "url")?,
        associated_event_id: optional_id(row, "associated_event_id")?.map(EventId::from_bytes),
        // NativeApi derives the current Issue from the associated scoped Event.
        issue_id: None,
        trace_id: optional_id(row, "trace_id")?.map(TraceId::from_bytes),
        replay_id: optional_id(row, "replay_id")?.map(EventId::from_bytes),
        attachments: vec![],
        expires_at: timestamp(row, "expire_at")?,
    };
    validate(&record)?;
    Ok(record)
}
fn decode_attachment(row: &PgRow) -> Result<EventAttachment, FeedbackStoreError> {
    let raw = required_text(row, "filename")?;
    let filename = AttachmentFilename::sanitized(&raw).map_err(invalid)?;
    if filename.as_str() != raw.as_ref() {
        return Err(FeedbackStoreError::InvalidData);
    }
    Ok(EventAttachment {
        attachment_id: BlobObjectId::from_bytes(fixed(row, "attachment_id")?),
        blob: BlobObject {
            key: BlobKey::new(row.try_get::<String, _>("blob_key").map_err(invalid)?)
                .map_err(invalid)?,
            kind: BlobKind::EventAttachment,
            size: u64::try_from(row.try_get::<i64, _>("size").map_err(invalid)?)
                .map_err(invalid)?,
            checksum: BlobChecksum::from_bytes(fixed(row, "checksum")?),
            created_at: timestamp(row, "created_at")?,
        },
        filename,
        content_type: BlobContentType::new(&required_text(row, "content_type")?)
            .map_err(invalid)?,
        attachment_type: required_text(row, "attachment_type")?,
    })
}
fn status_code(status: FeedbackStatus) -> i16 {
    match status {
        FeedbackStatus::Open => 0,
        FeedbackStatus::Resolved => 1,
        FeedbackStatus::Spam => 2,
    }
}
fn fixed<const N: usize>(row: &PgRow, key: &str) -> Result<[u8; N], FeedbackStoreError> {
    row.try_get::<Vec<u8>, _>(key)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
fn optional_id(row: &PgRow, key: &str) -> Result<Option<[u8; 16]>, FeedbackStoreError> {
    row.try_get::<Option<Vec<u8>>, _>(key)
        .map_err(invalid)?
        .map(|v| v.try_into().map_err(invalid))
        .transpose()
}
fn timestamp(row: &PgRow, key: &str) -> Result<Timestamp, FeedbackStoreError> {
    Timestamp::from_unix_millis(row.try_get(key).map_err(invalid)?).map_err(invalid)
}
fn required_text(row: &PgRow, key: &str) -> Result<Box<str>, FeedbackStoreError> {
    codec::decode_utf8(row.try_get(key).map_err(invalid)?)
        .map(String::into_boxed_str)
        .map_err(invalid)
}
fn text(row: &PgRow, key: &str) -> Result<Option<Box<str>>, FeedbackStoreError> {
    row.try_get::<Option<Vec<u8>>, _>(key)
        .map_err(invalid)?
        .map(|v| {
            codec::decode_utf8(v)
                .map(String::into_boxed_str)
                .map_err(invalid)
        })
        .transpose()
}
fn invalid<T>(_: T) -> FeedbackStoreError {
    FeedbackStoreError::InvalidData
}
fn unavailable(_: sqlx::Error) -> FeedbackStoreError {
    FeedbackStoreError::Unavailable
}
fn write_error(error: sqlx::Error) -> FeedbackStoreError {
    match error {
        sqlx::Error::Database(ref db)
            if db
                .code()
                .as_deref()
                .is_some_and(|code| code.starts_with("23") || code == "22003") =>
        {
            FeedbackStoreError::InvalidData
        }
        _ => FeedbackStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
