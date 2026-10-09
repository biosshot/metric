use crate::{PostgresDatabase, PostgresError, codec};
use metric_domain::{
    EventId, ProjectId, Timestamp,
    blob::{BlobChecksum, BlobKey, BlobKind, BlobObject},
    replays::{
        MAX_REPLAY_SEGMENTS, ReplayCursor, ReplayMetadata, ReplayPage, ReplayRecord, ReplaySegment,
        ReplaySegmentCommit,
    },
    signals::TraceId,
};
use metric_ports::{DurableOutcome, PortFuture, ReplayQuery, ReplayStore, SignalStoreError};
use sqlx::{Postgres, QueryBuilder, Row, postgres::PgRow};
use std::collections::BTreeMap;

const DAY: i64 = 86_400_000;
const MAX_PAGE: usize = 100;

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
pub struct PostgresReplayStore {
    database: PostgresDatabase,
    retention: ReplayRetention,
}
impl PostgresReplayStore {
    pub fn new(
        database: PostgresDatabase,
        retention: ReplayRetention,
    ) -> Result<Self, PostgresError> {
        if !(1..=3650).contains(&retention.days) {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self {
            database,
            retention,
        })
    }
    async fn persist(
        &self,
        commit: ReplaySegmentCommit,
    ) -> Result<DurableOutcome, SignalStoreError> {
        let metadata = &commit.metadata;
        let segment = &commit.segment;
        metadata.validate().map_err(invalid)?;
        if segment.segment_id != metadata.segment_id
            || segment.object.kind != BlobKind::ReplayRecording
            || segment.object.key.replay_relation().map_err(invalid)?
                != (metadata.project_id, metadata.replay_id, metadata.segment_id)
        {
            return Err(SignalStoreError::InvalidData);
        }
        let size = i64::try_from(segment.object.size).map_err(invalid)?;
        let decompressed = i64::try_from(segment.decompressed_bytes).map_err(invalid)?;
        let expire = metadata
            .ended_at
            .unix_millis()
            .checked_add(i64::from(self.retention.days) * DAY)
            .and_then(|value| Timestamp::from_unix_millis(value).ok())
            .ok_or(SignalStoreError::InvalidData)?
            .unix_millis();
        let errors: Vec<_> = metadata
            .error_ids
            .iter()
            .map(|id| id.as_bytes().to_vec())
            .collect();
        let traces: Vec<_> = metadata
            .trace_ids
            .iter()
            .map(|id| id.as_bytes().to_vec())
            .collect();
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        let state: Option<i16> =
            sqlx::query_scalar("SELECT state FROM metric.projects WHERE id=$1 FOR SHARE")
                .bind(metadata.project_id.get())
                .fetch_optional(&mut *transaction)
                .await
                .map_err(unavailable)?;
        if !matches!(state, Some(0 | 1)) {
            return Err(SignalStoreError::NotFound);
        }
        // Inserting the manifest and locking its row serializes even concurrent first
        // segments. Unrelated Replay identities and signal lanes remain independent.
        sqlx::query("INSERT INTO metric.replays(project_id,replay_id,started_at,ended_at,received_at,environment,release,url,error_ids,trace_ids,expire_at,archive_due) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT(project_id,replay_id) DO NOTHING")
            .bind(metadata.project_id.get()).bind(metadata.replay_id.as_bytes().as_slice())
            .bind(metadata.started_at.unix_millis()).bind(metadata.ended_at.unix_millis()).bind(metadata.received_at.unix_millis())
            .bind(metadata.environment.as_deref().map(str::as_bytes)).bind(metadata.release.as_deref().map(str::as_bytes)).bind(metadata.url.as_deref().map(str::as_bytes))
            .bind(&errors).bind(&traces).bind(if self.retention.archive {None}else{Some(expire)}).bind(if self.retention.archive {Some(expire)}else{None})
            .execute(&mut *transaction).await.map_err(write_error)?;
        sqlx::query(
            "SELECT replay_id FROM metric.replays WHERE project_id=$1 AND replay_id=$2 FOR UPDATE",
        )
        .bind(metadata.project_id.get())
        .bind(metadata.replay_id.as_bytes().as_slice())
        .fetch_one(&mut *transaction)
        .await
        .map_err(unavailable)?;
        let existing = sqlx::query("SELECT blob_key,size,checksum FROM metric.replay_segments WHERE project_id=$1 AND replay_id=$2 AND segment_id=$3")
            .bind(metadata.project_id.get()).bind(metadata.replay_id.as_bytes().as_slice()).bind(metadata.segment_id as i32)
            .fetch_optional(&mut *transaction).await.map_err(unavailable)?;
        if let Some(existing) = existing {
            if existing.try_get::<String, _>("blob_key").map_err(invalid)?
                != segment.object.key.as_str()
                || existing.try_get::<i64, _>("size").map_err(invalid)? != size
                || existing
                    .try_get::<Vec<u8>, _>("checksum")
                    .map_err(invalid)?
                    != segment.object.checksum.as_bytes()
            {
                return Err(SignalStoreError::Conflict);
            }
            transaction.commit().await.map_err(unavailable)?;
            return Ok(DurableOutcome::Duplicate);
        }
        sqlx::query("INSERT INTO metric.replay_segments(project_id,replay_id,segment_id,blob_key,size,checksum,created_at,decompressed_bytes,event_count) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(metadata.project_id.get()).bind(metadata.replay_id.as_bytes().as_slice()).bind(metadata.segment_id as i32)
            .bind(segment.object.key.as_str()).bind(size).bind(segment.object.checksum.as_bytes().as_slice()).bind(segment.object.created_at.unix_millis())
            .bind(decompressed).bind(i64::from(segment.event_count)).execute(&mut *transaction).await.map_err(write_error)?;
        // Catalog fields follow the existing accepted-segment semantics. Missing
        // optional text preserves a prior value; correlations are replaced, and
        // retention stays anchored to the first accepted segment.
        sqlx::query("UPDATE metric.replays SET started_at=LEAST(started_at,$3),ended_at=GREATEST(ended_at,$4),received_at=$5,environment=COALESCE($6,environment),release=COALESCE($7,release),url=COALESCE($8,url),error_ids=$9,trace_ids=$10 WHERE project_id=$1 AND replay_id=$2")
            .bind(metadata.project_id.get()).bind(metadata.replay_id.as_bytes().as_slice()).bind(metadata.started_at.unix_millis()).bind(metadata.ended_at.unix_millis()).bind(metadata.received_at.unix_millis())
            .bind(metadata.environment.as_deref().map(str::as_bytes)).bind(metadata.release.as_deref().map(str::as_bytes)).bind(metadata.url.as_deref().map(str::as_bytes))
            .bind(&errors).bind(&traces).execute(&mut *transaction).await.map_err(write_error)?;
        // An unknown COMMIT result is Unavailable under the unchanged signal port.
        // Retry checks the immutable recording instead of appending another segment.
        transaction.commit().await.map_err(unavailable)?;
        Ok(DurableOutcome::Accepted)
    }
    async fn read(
        &self,
        project: ProjectId,
        query: Option<ReplayQuery>,
        id: Option<EventId>,
    ) -> Result<Vec<ReplayRecord>, SignalStoreError> {
        let mut selection = QueryBuilder::<Postgres>::new(
            "SELECT project_id,replay_id,started_at,ended_at,received_at,environment,release,url,error_ids,trace_ids,COALESCE(expire_at,archive_due) AS expires_at FROM metric.replays WHERE project_id=",
        );
        selection.push_bind(project.get());
        if let Some(id) = id {
            selection
                .push(" AND replay_id=")
                .push_bind(id.as_bytes().to_vec());
        }
        if let Some(query) = query {
            if query.limit == 0 || query.limit > MAX_PAGE {
                return Err(SignalStoreError::InvalidData);
            }
            if let Some(from) = query.from {
                selection
                    .push(" AND received_at>=")
                    .push_bind(from.unix_millis());
            }
            if let Some(until) = query.until {
                selection
                    .push(" AND received_at<")
                    .push_bind(until.unix_millis());
            }
            if let Some(error) = query.error_id {
                selection
                    .push(" AND error_ids @> ")
                    .push_bind(vec![error.as_bytes().to_vec()])
                    .push("::bytea[]");
            }
            if let Some(trace) = query.trace_id {
                selection
                    .push(" AND trace_ids @> ")
                    .push_bind(vec![trace.as_bytes().to_vec()])
                    .push("::bytea[]");
            }
            if let Some(before) = query.before {
                selection
                    .push(" AND (received_at,replay_id)<(")
                    .push_bind(before.received_at.unix_millis())
                    .push(",")
                    .push_bind(before.replay_id.as_bytes().to_vec())
                    .push(")");
            }
        }
        selection
            .push(" ORDER BY received_at DESC,replay_id DESC LIMIT ")
            .push_bind(query.map_or(1, |query| query.limit + 1) as i64);
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        // One snapshot covers the bounded page and its bounded segment sets.
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
        let rows = selection
            .build()
            .fetch_all(&mut *transaction)
            .await
            .map_err(unavailable)?;
        let mut records = rows
            .iter()
            .map(decode_record)
            .collect::<Result<Vec<_>, _>>()?;
        if !records.is_empty() {
            let ids: Vec<_> = records
                .iter()
                .map(|record| record.replay_id.as_bytes().to_vec())
                .collect();
            let rows=sqlx::query("SELECT project_id,replay_id,segment_id,blob_key,size,checksum,created_at,decompressed_bytes,event_count FROM metric.replay_segments WHERE project_id=$1 AND replay_id=ANY($2::bytea[]) ORDER BY replay_id,segment_id LIMIT 10100")
                .bind(project.get()).bind(&ids).fetch_all(&mut *transaction).await.map_err(unavailable)?;
            let mut segments = BTreeMap::<[u8; 16], Vec<ReplaySegment>>::new();
            for row in rows {
                let id = fixed(&row, "replay_id")?;
                segments.entry(id).or_default().push(decode_segment(&row)?);
            }
            for record in &mut records {
                record.segments = segments
                    .remove(&record.replay_id.as_bytes())
                    .ok_or(SignalStoreError::InvalidData)?;
            }
            if !segments.is_empty() {
                return Err(SignalStoreError::InvalidData);
            }
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(records)
    }
}
impl ReplayStore for PostgresReplayStore {
    fn persist_replay_segment(
        &self,
        commit: ReplaySegmentCommit,
    ) -> PortFuture<'_, Result<DurableOutcome, SignalStoreError>> {
        Box::pin(self.persist(commit))
    }
    fn list_replays(
        &self,
        project: ProjectId,
        query: ReplayQuery,
    ) -> PortFuture<'_, Result<ReplayPage, SignalStoreError>> {
        Box::pin(async move {
            let mut items = self.read(project, Some(query), None).await?;
            let next = if items.len() > query.limit {
                items.pop();
                items.last().map(|record| ReplayCursor {
                    received_at: record.received_at,
                    replay_id: record.replay_id,
                })
            } else {
                None
            };
            Ok(ReplayPage { items, next })
        })
    }
    fn load_replay(
        &self,
        project: ProjectId,
        id: EventId,
    ) -> PortFuture<'_, Result<ReplayRecord, SignalStoreError>> {
        Box::pin(async move {
            self.read(project, None, Some(id))
                .await?
                .pop()
                .ok_or(SignalStoreError::NotFound)
        })
    }
    fn references_replay_blob(
        &self,
        key: &BlobKey,
    ) -> PortFuture<'_, Result<bool, SignalStoreError>> {
        let key = key.as_str().to_owned();
        Box::pin(async move {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM metric.replay_segments WHERE blob_key=$1)",
            )
            .bind(key)
            .fetch_one(&self.database.pool)
            .await
            .map_err(unavailable)
        })
    }
}
fn decode_record(row: &PgRow) -> Result<ReplayRecord, SignalStoreError> {
    let project_id =
        ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?;
    let replay_id = EventId::from_bytes(fixed(row, "replay_id")?);
    let started_at = timestamp(row, "started_at")?;
    let ended_at = timestamp(row, "ended_at")?;
    let received_at = timestamp(row, "received_at")?;
    let environment = text(row, "environment")?;
    let release = text(row, "release")?;
    let url = text(row, "url")?;
    let errors: Vec<Vec<u8>> = row.try_get("error_ids").map_err(invalid)?;
    let traces: Vec<Vec<u8>> = row.try_get("trace_ids").map_err(invalid)?;
    let error_ids = errors
        .into_iter()
        .map(|id| id.try_into().map(EventId::from_bytes).map_err(invalid))
        .collect::<Result<Vec<_>, _>>()?;
    let trace_ids = traces
        .into_iter()
        .map(|id| id.try_into().map(TraceId::from_bytes).map_err(invalid))
        .collect::<Result<Vec<_>, _>>()?;
    // Validate bounded metadata independently of the total extent across accepted
    // segments; each individual submission owns the existing 24-hour bound.
    ReplayMetadata {
        project_id,
        replay_id,
        segment_id: 0,
        started_at,
        ended_at: started_at,
        received_at,
        environment: environment.clone(),
        release: release.clone(),
        url: url.clone(),
        error_ids: error_ids.clone(),
        trace_ids: trace_ids.clone(),
    }
    .validate()
    .map_err(invalid)?;
    if ended_at < started_at {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(ReplayRecord {
        project_id,
        replay_id,
        started_at,
        ended_at,
        received_at,
        environment,
        release,
        url,
        error_ids,
        trace_ids,
        segments: vec![],
        expires_at: Some(timestamp(row, "expires_at")?),
    })
}
fn decode_segment(row: &PgRow) -> Result<ReplaySegment, SignalStoreError> {
    let project = ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?;
    let replay = EventId::from_bytes(fixed(row, "replay_id")?);
    let segment_id =
        u32::try_from(row.try_get::<i32, _>("segment_id").map_err(invalid)?).map_err(invalid)?;
    let key =
        BlobKey::new(row.try_get::<String, _>("blob_key").map_err(invalid)?).map_err(invalid)?;
    if segment_id >= MAX_REPLAY_SEGMENTS
        || key.replay_relation().map_err(invalid)? != (project, replay, segment_id)
    {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(ReplaySegment {
        segment_id,
        object: BlobObject {
            key,
            kind: BlobKind::ReplayRecording,
            size: number(row, "size")?,
            checksum: BlobChecksum::from_bytes(fixed(row, "checksum")?),
            created_at: timestamp(row, "created_at")?,
        },
        decompressed_bytes: number(row, "decompressed_bytes")?,
        event_count: u32::try_from(number(row, "event_count")?).map_err(invalid)?,
    })
}
fn number(row: &PgRow, key: &str) -> Result<u64, SignalStoreError> {
    u64::try_from(row.try_get::<i64, _>(key).map_err(invalid)?).map_err(invalid)
}
fn fixed<const N: usize>(row: &PgRow, key: &str) -> Result<[u8; N], SignalStoreError> {
    row.try_get::<Vec<u8>, _>(key)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
fn timestamp(row: &PgRow, key: &str) -> Result<Timestamp, SignalStoreError> {
    Timestamp::from_unix_millis(row.try_get(key).map_err(invalid)?).map_err(invalid)
}
fn text(row: &PgRow, key: &str) -> Result<Option<Box<str>>, SignalStoreError> {
    row.try_get::<Option<Vec<u8>>, _>(key)
        .map_err(invalid)?
        .map(|bytes| {
            codec::decode_utf8(bytes)
                .map(String::into_boxed_str)
                .map_err(invalid)
        })
        .transpose()
}
fn invalid<T>(_: T) -> SignalStoreError {
    SignalStoreError::InvalidData
}
fn unavailable(_: sqlx::Error) -> SignalStoreError {
    SignalStoreError::Unavailable
}
fn write_error(error: sqlx::Error) -> SignalStoreError {
    match error {
        sqlx::Error::Database(ref db)
            if db
                .code()
                .as_deref()
                .is_some_and(|code| code.starts_with("23") || code == "22003") =>
        {
            SignalStoreError::InvalidData
        }
        _ => SignalStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
