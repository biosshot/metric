//! Bounded native archive metadata; Blob publication stays outside transactions.
use crate::{EventCodecConfig, PostgresDatabase, PostgresError, events, sessions, signals};
use futures_util::TryStreamExt;
use metric_domain::{
    EventKey, ProjectId, Timestamp,
    archive::{
        ArchiveBatch, ArchiveBatchState, ArchiveEvent, ArchiveKind, ArchiveRecords,
        ArchiveSegmentId, ArchiveSignal, ArchiveSourceId,
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
use serde_json::json;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};
use std::collections::{BTreeMap, BTreeSet};
use time::OffsetDateTime;

const MAX_RECORDS: usize = 10_000;
const MAX_BYTES: usize = 512 * 1024 * 1024;
const MAX_EVENT: usize = 4 * 1024 * 1024;
const DAY: i64 = 86_400_000;

struct SourceRead<'a> {
    ids: Option<&'a [Vec<u8>]>,
    window: Option<(i64, i64, i64)>,
    maximum: usize,
    budget: usize,
}

#[derive(Clone)]
pub struct PostgresArchiveStore {
    database: PostgresDatabase,
    codec: EventCodecConfig,
}
impl PostgresArchiveStore {
    pub fn new(database: PostgresDatabase, codec: EventCodecConfig) -> Result<Self, PostgresError> {
        // Reuse the Event adapter's codec policy instead of maintaining another validator.
        crate::PostgresEventStore::new(database.clone(), codec)?;
        Ok(Self { database, codec })
    }
    async fn claim_inner(
        &self,
        request: ArchiveClaimRequest,
    ) -> Result<Option<ArchiveBatch>, ArchiveStoreError> {
        if !(1..=MAX_RECORDS).contains(&request.maximum_events)
            || !(1024..=MAX_BYTES).contains(&request.target_uncompressed_bytes)
        {
            return Err(ArchiveStoreError::InvalidData);
        }
        // Choose a candidate without locks, then take project-before-lane locks.
        // Recheck after serialization: a competing process can change the candidate.
        for _ in 0..8 {
            let project = candidate(&self.database.pool, request.kind, request.now).await?;
            let Some(project) = project else {
                return Ok(None);
            };
            let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
            if !lock_project(&mut tx, project).await? {
                continue;
            }
            lane(&mut tx, request.kind).await?;
            let pending = sqlx::query("SELECT * FROM metric.archive_manifests WHERE kind=$1 AND NOT source_committed ORDER BY state,created_at,segment_id LIMIT 1")
                .bind(code(request.kind)).fetch_optional(&mut *tx).await.map_err(unavailable)?;
            if let Some(row) = pending {
                if row.try_get::<i32, _>("project_id").map_err(invalid)? != project {
                    continue;
                }
                let mut batch = decode_manifest(&row)?;
                if batch.state == ArchiveBatchState::Writing {
                    let ids = batch
                        .source_ids
                        .iter()
                        .map(|id| local_id(*id))
                        .collect::<Vec<_>>();
                    batch.records = self
                        .read_sources(
                            &mut tx,
                            batch.kind,
                            project,
                            SourceRead {
                                ids: Some(&ids),
                                window: None,
                                maximum: MAX_RECORDS,
                                budget: MAX_BYTES,
                            },
                        )
                        .await?;
                    if record_ids(batch.kind, &batch.records)? != batch.source_ids {
                        return Err(ArchiveStoreError::InvalidData);
                    }
                }
                tx.commit().await.map_err(unavailable)?;
                return Ok(Some(batch));
            }
            let Some(first) = first_due(&mut tx, request.kind, request.now).await? else {
                tx.commit().await.map_err(unavailable)?;
                return Ok(None);
            };
            if first.try_get::<i32, _>("project_id").map_err(invalid)? != project {
                continue;
            }
            let start = first
                .try_get::<i64, _>("received_at")
                .map_err(invalid)?
                .div_euclid(DAY)
                * DAY;
            let records = self
                .read_sources(
                    &mut tx,
                    request.kind,
                    project,
                    SourceRead {
                        ids: None,
                        window: Some((start, start + DAY, request.now.unix_millis())),
                        maximum: request.maximum_events,
                        budget: request.target_uncompressed_bytes,
                    },
                )
                .await?;
            if records.is_empty() {
                continue;
            }
            let source_ids = record_ids(request.kind, &records)?;
            let segment_id = ArchiveSegmentId::derive_sources(
                request.kind,
                ProjectId::new(project).map_err(invalid)?,
                &source_ids,
            );
            let (from, to) = record_times(&records)?;
            let object_key = object_key(
                request.kind,
                ProjectId::new(project).map_err(invalid)?,
                from,
                segment_id,
            )?;
            let bytes: Vec<_> = source_ids.iter().map(|id| id.as_bytes()).collect();
            let inserted=sqlx::query("INSERT INTO metric.archive_manifests(segment_id,kind,project_id,received_from,received_to,object_key,source_ids,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(segment_id) DO NOTHING")
                .bind(segment_id.as_bytes().as_slice()).bind(code(request.kind)).bind(project).bind(from.unix_millis()).bind(to.unix_millis()).bind(object_key.as_str()).bind(bytes).bind(request.now.unix_millis()).execute(&mut *tx).await.map_err(unavailable)?;
            // A previously committed identical ID must never be overwritten or
            // silently reported as a new archive after source recreation.
            if inserted.rows_affected() != 1 {
                return Err(ArchiveStoreError::Conflict);
            }
            tx.commit().await.map_err(unavailable)?;
            return Ok(Some(ArchiveBatch {
                kind: request.kind,
                segment_id,
                project_id: ProjectId::new(project).map_err(invalid)?,
                received_from: from,
                received_to: to,
                object_key,
                source_ids,
                records,
                state: ArchiveBatchState::Writing,
            }));
        }
        Err(ArchiveStoreError::Unavailable)
    }
    async fn read_sources(
        &self,
        connection: &mut PgConnection,
        kind: ArchiveKind,
        project: i32,
        selection: SourceRead<'_>,
    ) -> Result<ArchiveRecords, ArchiveStoreError> {
        let SourceRead {
            ids,
            window,
            maximum,
            budget,
        } = selection;
        let spec = source(kind);
        let mut q = QueryBuilder::<Postgres>::new("SELECT project_id,");
        q.push(spec.id)
            .push(" AS source_id,")
            .push(spec.time)
            .push(" AS archive_received,")
            .push(spec.columns);
        if matches!(
            kind,
            ArchiveKind::Event | ArchiveKind::Log | ArchiveKind::Span
        ) {
            let limit = if kind == ArchiveKind::Event {
                self.codec.max_encoded_row_bytes.min(2 * MAX_EVENT + 2)
            } else {
                1024 * 1024 + 2
            };
            q.push(",CASE WHEN octet_length(body)<=")
                .push_bind(limit as i64)
                .push(" THEN body ELSE NULL END AS body");
        }
        q.push(" FROM ")
            .push(spec.table)
            .push(" WHERE project_id=")
            .push_bind(project)
            .push(" AND archive_due IS NOT NULL AND archive_segment IS NULL")
            .push(spec.terminal);
        if let Some(ids) = ids {
            q.push(" AND ")
                .push(spec.id)
                .push("=ANY(")
                .push_bind(ids)
                .push(")");
        }
        if let Some((start, end, now)) = window {
            q.push(" AND ")
                .push(spec.time)
                .push(">=")
                .push_bind(start)
                .push(" AND ")
                .push(spec.time)
                .push("<")
                .push_bind(end)
                .push(" AND archive_due<=")
                .push_bind(now);
        }
        q.push(" ORDER BY ")
            .push(spec.time)
            .push(",")
            .push(spec.id)
            .push(" LIMIT ")
            .push_bind(maximum as i64);
        let mut rows = q.build().fetch(connection);
        let mut events = Vec::new();
        let mut signals = Vec::new();
        let mut used = 0usize;
        while let Some(row) = rows.try_next().await.map_err(unavailable)? {
            let item = decode_source(&row, kind, self.codec)?;
            let cost = item
                .payload_len()
                .checked_add(128)
                .ok_or(ArchiveStoreError::InvalidData)?;
            if used != 0 && used.saturating_add(cost) > budget {
                if ids.is_some() {
                    return Err(ArchiveStoreError::InvalidData);
                }
                break;
            }
            used = used.saturating_add(cost);
            match item {
                Record::Event(value) => events.push(value),
                Record::Signal(value) => signals.push(value),
            }
        }
        // Resume in persisted order, even if a mutable Metrics received_at changed.
        if let Some(ids) = ids {
            match kind {
                ArchiveKind::Event => {
                    let mut by_id = events
                        .into_iter()
                        .map(|v| (v.key.event_id().as_bytes().to_vec(), v))
                        .collect::<BTreeMap<_, _>>();
                    events = ids
                        .iter()
                        .map(|id| by_id.remove(id).ok_or(ArchiveStoreError::InvalidData))
                        .collect::<Result<_, _>>()?;
                }
                _ => {
                    let mut by_id = signals
                        .into_iter()
                        .map(|v| (v.id.to_vec(), v))
                        .collect::<BTreeMap<_, _>>();
                    signals = ids
                        .iter()
                        .map(|id| by_id.remove(id).ok_or(ArchiveStoreError::InvalidData))
                        .collect::<Result<_, _>>()?;
                }
            }
        }
        Ok(match kind {
            ArchiveKind::Event => ArchiveRecords::Events(events),
            ArchiveKind::Log => ArchiveRecords::Logs(signals),
            ArchiveKind::Span => ArchiveRecords::Spans(signals),
            ArchiveKind::Session => ArchiveRecords::Sessions(signals),
            ArchiveKind::Metric => ArchiveRecords::Metrics(signals),
        })
    }
    async fn complete_inner(
        &self,
        request: ArchiveCompleteRequest,
    ) -> Result<(), ArchiveStoreError> {
        let Some((project, kind)) = self.manifest_scope(request.segment_id).await? else {
            return Err(ArchiveStoreError::InvalidData);
        };
        if request.object.kind != BlobKind::archive(kind) {
            return Err(ArchiveStoreError::InvalidData);
        }
        let size = i64::try_from(request.object.size).map_err(invalid)?;
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        if !lock_project(&mut tx, project).await? {
            return Err(ArchiveStoreError::InvalidData);
        }
        lane(&mut tx, kind).await?;
        let row =
            sqlx::query("SELECT * FROM metric.archive_manifests WHERE segment_id=$1 FOR UPDATE")
                .bind(request.segment_id.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(unavailable)?
                .ok_or(ArchiveStoreError::InvalidData)?;
        let batch = decode_manifest(&row)?;
        if batch.project_id.get() != project
            || batch.kind != kind
            || batch.object_key != request.object.key
        {
            return Err(ArchiveStoreError::Conflict);
        }
        if batch.state == ArchiveBatchState::Complete {
            if row.try_get::<i64, _>("stored_bytes").map_err(invalid)? != size
                || row
                    .try_get::<Vec<u8>, _>("checksum")
                    .map_err(invalid)?
                    .as_slice()
                    != request.object.checksum.as_bytes()
            {
                return Err(ArchiveStoreError::Conflict);
            }
        } else {
            sqlx::query("UPDATE metric.archive_manifests SET state=1,stored_bytes=$2,checksum=$3,completed_at=$4 WHERE segment_id=$1 AND state=0 AND NOT source_committed")
                .bind(request.segment_id.as_bytes().as_slice()).bind(size).bind(request.object.checksum.as_bytes().as_slice()).bind(request.completed_at.unix_millis()).execute(&mut *tx).await.map_err(unavailable)?;
        }
        tx.commit().await.map_err(unavailable)
    }
    async fn commit_inner(
        &self,
        request: ArchiveSourceCommitRequest,
    ) -> Result<usize, ArchiveStoreError> {
        if request.source_ids.is_empty()
            || request.source_ids.len() > MAX_RECORDS
            || request
                .source_ids
                .iter()
                .any(|id| id.kind() != request.kind)
        {
            return Err(ArchiveStoreError::InvalidData);
        }
        let Some((project, kind)) = self.manifest_scope(request.segment_id).await? else {
            return Err(ArchiveStoreError::InvalidData);
        };
        if kind != request.kind {
            return Err(ArchiveStoreError::Conflict);
        }
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        if !lock_project(&mut tx, project).await? {
            return Err(ArchiveStoreError::InvalidData);
        }
        lane(&mut tx, kind).await?;
        let row =
            sqlx::query("SELECT * FROM metric.archive_manifests WHERE segment_id=$1 FOR UPDATE")
                .bind(request.segment_id.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(unavailable)?
                .ok_or(ArchiveStoreError::InvalidData)?;
        let batch = decode_manifest(&row)?;
        if batch.state != ArchiveBatchState::Complete {
            return Err(ArchiveStoreError::InvalidData);
        }
        if batch.project_id.get() != project
            || batch.kind != kind
            || batch.source_ids != request.source_ids
        {
            return Err(ArchiveStoreError::Conflict);
        }
        if row
            .try_get::<bool, _>("source_committed")
            .map_err(invalid)?
        {
            tx.commit().await.map_err(unavailable)?;
            return Ok(0);
        }
        let spec = source(kind);
        let ids = request
            .source_ids
            .iter()
            .map(|id| local_id(*id))
            .collect::<Vec<_>>();
        // Scoped locks make conflict detection and the expiry/manifest transition
        // one transaction. IDs from another project never enter this write.
        let mut q = QueryBuilder::<Postgres>::new("SELECT archive_segment FROM ");
        q.push(spec.table)
            .push(" WHERE project_id=")
            .push_bind(project)
            .push(" AND ")
            .push(spec.id)
            .push("=ANY(")
            .push_bind(&ids)
            .push(") ORDER BY ")
            .push(spec.id)
            .push(" FOR UPDATE");
        let locked = q.build().fetch_all(&mut *tx).await.map_err(unavailable)?;
        for row in locked {
            if row
                .try_get::<Option<Vec<u8>>, _>("archive_segment")
                .map_err(invalid)?
                .is_some_and(|v| v.as_slice() != request.segment_id.as_bytes())
            {
                return Err(ArchiveStoreError::Conflict);
            }
        }
        let mut q = QueryBuilder::<Postgres>::new("UPDATE ");
        q.push(spec.table)
            .push(" SET archive_segment=")
            .push_bind(request.segment_id.as_bytes().to_vec())
            .push(",expire_at=")
            .push_bind(request.expire_at.unix_millis())
            .push(",archive_due=NULL WHERE project_id=")
            .push_bind(project)
            .push(" AND ")
            .push(spec.id)
            .push("=ANY(")
            .push_bind(ids)
            .push(") AND archive_due IS NOT NULL AND archive_segment IS NULL");
        let changed = q
            .build()
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?
            .rows_affected() as usize;
        sqlx::query(
            "UPDATE metric.archive_manifests SET source_committed=true WHERE segment_id=$1",
        )
        .bind(request.segment_id.as_bytes().as_slice())
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(changed)
    }
    async fn manifest_scope(
        &self,
        id: ArchiveSegmentId,
    ) -> Result<Option<(i32, ArchiveKind)>, ArchiveStoreError> {
        sqlx::query("SELECT project_id,kind FROM metric.archive_manifests WHERE segment_id=$1")
            .bind(id.as_bytes().as_slice())
            .fetch_optional(&self.database.pool)
            .await
            .map_err(unavailable)?
            .map(|row| {
                Ok((
                    row.try_get("project_id").map_err(invalid)?,
                    kind(row.try_get("kind").map_err(invalid)?)?,
                ))
            })
            .transpose()
    }
}
impl ArchiveStore for PostgresArchiveStore {
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
        Box::pin(self.commit_inner(request))
    }
    fn object_referenced(&self, key: &BlobKey) -> PortFuture<'_, Result<bool, ArchiveStoreError>> {
        let key = key.as_str().to_owned();
        Box::pin(async move {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM metric.archive_manifests WHERE object_key=$1)",
            )
            .bind(key)
            .fetch_one(&self.database.pool)
            .await
            .map_err(unavailable)
        })
    }
}

// Every SQL identifier/projection is an application-owned literal selected by enum.
struct Source {
    table: &'static str,
    id: &'static str,
    time: &'static str,
    columns: &'static str,
    terminal: &'static str,
}
fn source(kind: ArchiveKind) -> Source {
    match kind {
        ArchiveKind::Event => Source {
            table: "metric.error_events",
            id: "event_id",
            time: "received_at",
            columns: "coalesce(occurred_at,received_at) AS occurred_at,issue_id",
            terminal: " AND processing_state IN (1,2)",
        },
        ArchiveKind::Log => Source {
            table: "metric.logs",
            id: "log_id",
            time: "received_at",
            columns: "occurred_at_ns,body_size",
            terminal: "",
        },
        ArchiveKind::Span => Source {
            table: "metric.spans",
            id: "record_id",
            time: "received_at",
            columns: "started_at_ns AS occurred_at_ns,body_size",
            terminal: "",
        },
        ArchiveKind::Session => Source {
            table: "metric.sessions",
            id: "session_id",
            time: "finished_at",
            columns: "session_id,release_id,environment_id,started_at,last_update,state,sequence,finished_at,duration_ms,user_digest",
            terminal: " AND state<>0",
        },
        ArchiveKind::Metric => Source {
            table: "metric.metric_buckets",
            id: "bucket_id",
            time: "received_at",
            columns: "series_id,kind,bucket_start,bucket_width_seconds,trace_id,aggregate_sum,measurement_count,last_value,minimum,maximum,bins,CASE WHEN octet_length(name)+octet_length(unit)+octet_length(tags)<=524288 THEN name END AS name,CASE WHEN octet_length(name)+octet_length(unit)+octet_length(tags)<=524288 THEN unit END AS unit,CASE WHEN octet_length(name)+octet_length(unit)+octet_length(tags)<=524288 THEN tags END AS tags",
            terminal: "",
        },
    }
}
async fn candidate(
    pool: &sqlx::PgPool,
    k: ArchiveKind,
    now: Timestamp,
) -> Result<Option<i32>, ArchiveStoreError> {
    let pending = sqlx::query_scalar("SELECT a.project_id FROM metric.archive_manifests a JOIN metric.projects p ON p.id=a.project_id WHERE a.kind=$1 AND NOT a.source_committed AND p.state IN (0,1) ORDER BY a.state,a.created_at,a.segment_id LIMIT 1")
        .bind(code(k)).fetch_optional(pool).await.map_err(unavailable)?;
    if let Some(project) = pending {
        return Ok(Some(project));
    }
    let mut connection = pool.acquire().await.map_err(unavailable)?;
    first_due(&mut connection, k, now)
        .await?
        .map(|row| row.try_get("project_id").map_err(invalid))
        .transpose()
}
async fn first_due(
    connection: &mut PgConnection,
    k: ArchiveKind,
    now: Timestamp,
) -> Result<Option<PgRow>, ArchiveStoreError> {
    let spec = source(k);
    let mut q = QueryBuilder::<Postgres>::new("SELECT project_id,");
    q.push(spec.time).push(" AS received_at FROM ").push(spec.table).push(" WHERE archive_due<=").push_bind(now.unix_millis()).push(" AND archive_segment IS NULL").push(spec.terminal).push(" AND project_id IN (SELECT id FROM metric.projects WHERE state IN (0,1)) ORDER BY archive_due,project_id,").push(spec.id).push(" LIMIT 1");
    q.build()
        .fetch_optional(connection)
        .await
        .map_err(unavailable)
}
async fn lock_project(
    connection: &mut PgConnection,
    project: i32,
) -> Result<bool, ArchiveStoreError> {
    let state: Option<i16> =
        sqlx::query_scalar("SELECT state FROM metric.projects WHERE id=$1 FOR SHARE")
            .bind(project)
            .fetch_optional(connection)
            .await
            .map_err(unavailable)?;
    Ok(matches!(state, Some(0 | 1)))
}
async fn lane(connection: &mut PgConnection, k: ArchiveKind) -> Result<(), ArchiveStoreError> {
    sqlx::query("SELECT kind FROM metric.archive_lanes WHERE kind=$1 FOR UPDATE")
        .bind(code(k))
        .fetch_one(connection)
        .await
        .map_err(unavailable)?;
    Ok(())
}
fn code(k: ArchiveKind) -> i16 {
    match k {
        ArchiveKind::Event => 0,
        ArchiveKind::Log => 1,
        ArchiveKind::Span => 2,
        ArchiveKind::Session => 3,
        ArchiveKind::Metric => 4,
    }
}
fn kind(v: i16) -> Result<ArchiveKind, ArchiveStoreError> {
    match v {
        0 => Ok(ArchiveKind::Event),
        1 => Ok(ArchiveKind::Log),
        2 => Ok(ArchiveKind::Span),
        3 => Ok(ArchiveKind::Session),
        4 => Ok(ArchiveKind::Metric),
        _ => Err(ArchiveStoreError::InvalidData),
    }
}
fn local_id(id: ArchiveSourceId) -> Vec<u8> {
    match id {
        ArchiveSourceId::Event(key) => key.event_id().as_bytes().to_vec(),
        _ => id.as_bytes(),
    }
}
fn source_id(
    k: ArchiveKind,
    p: ProjectId,
    bytes: &[u8],
) -> Result<ArchiveSourceId, ArchiveStoreError> {
    Ok(match k {
        ArchiveKind::Event => {
            let key = EventKey::from_bytes(bytes.try_into().map_err(invalid)?).map_err(invalid)?;
            if key.project_id() != p {
                return Err(ArchiveStoreError::InvalidData);
            }
            ArchiveSourceId::Event(key)
        }
        ArchiveKind::Log => {
            ArchiveSourceId::Log(LogId::from_bytes(bytes.try_into().map_err(invalid)?))
        }
        ArchiveKind::Span => {
            ArchiveSourceId::Span(SpanRecordId::from_bytes(bytes.try_into().map_err(invalid)?))
        }
        ArchiveKind::Session => {
            ArchiveSourceId::Session(SessionId::from_bytes(bytes.try_into().map_err(invalid)?))
        }
        ArchiveKind::Metric => ArchiveSourceId::Metric(bytes.try_into().map_err(invalid)?),
    })
}
fn decode_manifest(row: &PgRow) -> Result<ArchiveBatch, ArchiveStoreError> {
    let k = kind(row.try_get("kind").map_err(invalid)?)?;
    let p = ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?;
    let bytes: Vec<Vec<u8>> = row.try_get("source_ids").map_err(invalid)?;
    if bytes.is_empty()
        || bytes.len() > MAX_RECORDS
        || bytes.iter().collect::<BTreeSet<_>>().len() != bytes.len()
    {
        return Err(ArchiveStoreError::InvalidData);
    }
    let ids = bytes
        .iter()
        .map(|v| source_id(k, p, v))
        .collect::<Result<Vec<_>, _>>()?;
    let segment_id = ArchiveSegmentId::from_bytes(fixed(row, "segment_id")?);
    if segment_id != ArchiveSegmentId::derive_sources(k, p, &ids)
        || row.try_get::<i16, _>("schema_version").map_err(invalid)? != 1
    {
        return Err(ArchiveStoreError::InvalidData);
    }
    let from = at(row.try_get("received_from").map_err(invalid)?)?;
    let to = at(row.try_get("received_to").map_err(invalid)?)?;
    if from > to || row.try_get::<i32, _>("record_count").map_err(invalid)? as usize != ids.len() {
        return Err(ArchiveStoreError::InvalidData);
    }
    let key =
        BlobKey::new(row.try_get::<String, _>("object_key").map_err(invalid)?).map_err(invalid)?;
    if key != object_key(k, p, from, segment_id)? {
        return Err(ArchiveStoreError::InvalidData);
    }
    let state = match row.try_get::<i16, _>("state").map_err(invalid)? {
        0 => ArchiveBatchState::Writing,
        1 => {
            let size: i64 = row.try_get("stored_bytes").map_err(invalid)?;
            if size < 0 {
                return Err(ArchiveStoreError::InvalidData);
            }
            let _: [u8; 32] = fixed(row, "checksum")?;
            ArchiveBatchState::Complete
        }
        _ => return Err(ArchiveStoreError::InvalidData),
    };
    Ok(ArchiveBatch {
        kind: k,
        segment_id,
        project_id: p,
        received_from: from,
        received_to: to,
        object_key: key,
        source_ids: ids,
        records: empty(k),
        state,
    })
}
enum Record {
    Event(ArchiveEvent),
    Signal(ArchiveSignal),
}
impl Record {
    fn payload_len(&self) -> usize {
        match self {
            Self::Event(v) => v.canonical_payload.len(),
            Self::Signal(v) => v.canonical_payload.len(),
        }
    }
}
fn decode_source(
    row: &PgRow,
    k: ArchiveKind,
    codec: EventCodecConfig,
) -> Result<Record, ArchiveStoreError> {
    let p = ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?;
    let id: [u8; 16] = fixed(row, "source_id")?;
    let received = at(row.try_get("archive_received").map_err(invalid)?)?;
    if k == ArchiveKind::Event {
        let payload = events::decode_body(
            &row.try_get::<Vec<u8>, _>("body").map_err(invalid)?,
            codec.max_decoded_body_bytes.min(MAX_EVENT),
        )
        .map_err(invalid)?;
        let key = EventKey::new(p, metric_domain::EventId::from_bytes(id));
        let json: serde_json::Value = serde_json::from_slice(&payload).map_err(invalid)?;
        if json
            .get("event_id")
            .and_then(|v| v.as_str())
            .and_then(|s| metric_domain::EventId::parse(s).ok())
            != Some(key.event_id())
        {
            return Err(ArchiveStoreError::InvalidData);
        }
        return Ok(Record::Event(ArchiveEvent {
            key,
            project_id: p,
            received_at: received,
            occurred_at: at(row.try_get("occurred_at").map_err(invalid)?)?,
            issue_id: row
                .try_get::<Option<Vec<u8>>, _>("issue_id")
                .map_err(invalid)?
                .map(|v| v.try_into().map(IssueId::from_bytes).map_err(invalid))
                .transpose()?,
            canonical_payload: payload.into_boxed_slice(),
        }));
    }
    let (occurred, payload) = match k {
        ArchiveKind::Log | ArchiveKind::Span => (
            row.try_get::<i64, _>("occurred_at_ns").map_err(invalid)?,
            signals::decode_body(row)
                .map_err(invalid)?
                .as_bytes()
                .to_vec(),
        ),
        ArchiveKind::Session => {
            let v = sessions::decode(row).map_err(invalid)?;
            let occurred = v
                .started_at
                .unix_millis()
                .checked_mul(1_000_000)
                .ok_or(ArchiveStoreError::InvalidData)?;
            let payload = json!({
                "session_id": hex_bytes(&v.id.as_bytes()),
                "project_id": p.get(),
                "release_id": hex_bytes(&v.release_id.as_bytes()),
                "environment_id": hex_bytes(&v.environment_id.as_bytes()),
                "started_at_unix_ms": v.started_at.unix_millis(),
                "last_update_unix_ms": v.last_update.unix_millis(),
                "state": v.state.code(),
                "sequence": v.sequence,
                "finished_at_unix_ms": v.finished_at.map(|t| t.unix_millis()),
                "duration_ms": v.duration_ms,
                "user_digest": v.user_digest.map(|b| hex_bytes(&b)),
            });
            (occurred, serde_json::to_vec(&payload).map_err(invalid)?)
        }
        ArchiveKind::Metric => {
            let start: i64 = row.try_get("bucket_start").map_err(invalid)?;
            let name = crate::codec::decode_utf8(row.try_get("name").map_err(invalid)?)
                .map_err(invalid)?;
            let unit = crate::codec::decode_utf8(row.try_get("unit").map_err(invalid)?)
                .map_err(invalid)?;
            let tags: serde_json::Value =
                serde_json::from_slice(&row.try_get::<Vec<u8>, _>("tags").map_err(invalid)?)
                    .map_err(invalid)?;
            let occurred = start
                .checked_mul(1_000_000)
                .ok_or(ArchiveStoreError::InvalidData)?;
            let payload = json!({
                "bucket_id": hex_bytes(&id),
                "project_id": p.get(),
                "series_id": hex_bytes(&fixed::<16>(row, "series_id")?),
                "name": name,
                "kind": row.try_get::<i16, _>("kind").map_err(invalid)?,
                "unit": unit,
                "tags": tags,
                "bucket_start_unix_ms": start,
                "bucket_width_seconds": row.try_get::<i64, _>("bucket_width_seconds").map_err(invalid)?,
                "received_at_unix_ms": received.unix_millis(),
                "trace_id": row.try_get::<Option<Vec<u8>>, _>("trace_id").map_err(invalid)?.map(|v| hex_bytes(&v)),
                "sum": row.try_get::<f64, _>("aggregate_sum").map_err(invalid)?,
                "count": row.try_get::<i64, _>("measurement_count").map_err(invalid)?,
                "last": row.try_get::<Option<f64>, _>("last_value").map_err(invalid)?,
                "min": row.try_get::<Option<f64>, _>("minimum").map_err(invalid)?,
                "max": row.try_get::<Option<f64>, _>("maximum").map_err(invalid)?,
                "bins": row.try_get::<Vec<i64>, _>("bins").map_err(invalid)?,
            });
            (occurred, serde_json::to_vec(&payload).map_err(invalid)?)
        }
        ArchiveKind::Event => unreachable!(),
    };
    if payload.len() > MAX_EVENT {
        return Err(ArchiveStoreError::InvalidData);
    }
    Ok(Record::Signal(ArchiveSignal {
        id,
        project_id: p,
        received_at: received,
        occurred_at_ns: occurred,
        canonical_payload: payload.into_boxed_slice(),
    }))
}
fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn fixed<const N: usize>(row: &PgRow, column: &str) -> Result<[u8; N], ArchiveStoreError> {
    row.try_get::<Vec<u8>, _>(column)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
fn at(v: i64) -> Result<Timestamp, ArchiveStoreError> {
    Timestamp::from_unix_millis(v).map_err(invalid)
}
fn object_key(
    k: ArchiveKind,
    p: ProjectId,
    received: Timestamp,
    id: ArchiveSegmentId,
) -> Result<BlobKey, ArchiveStoreError> {
    let date =
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(received.unix_millis()) * 1_000_000)
            .map_err(invalid)?;
    Ok(BlobKey::archive(
        k,
        p,
        date.year(),
        u8::from(date.month()),
        date.day(),
        id,
    ))
}
fn record_ids(
    k: ArchiveKind,
    records: &ArchiveRecords,
) -> Result<Vec<ArchiveSourceId>, ArchiveStoreError> {
    Ok(match records {
        ArchiveRecords::Events(v) => v.iter().map(|v| ArchiveSourceId::Event(v.key)).collect(),
        ArchiveRecords::Logs(v)
        | ArchiveRecords::Spans(v)
        | ArchiveRecords::Sessions(v)
        | ArchiveRecords::Metrics(v) => v
            .iter()
            .map(|v| source_id(k, v.project_id, &v.id))
            .collect::<Result<_, _>>()?,
    })
}
fn record_times(records: &ArchiveRecords) -> Result<(Timestamp, Timestamp), ArchiveStoreError> {
    let times: Vec<_> = match records {
        ArchiveRecords::Events(v) => v.iter().map(|v| v.received_at).collect(),
        ArchiveRecords::Logs(v)
        | ArchiveRecords::Spans(v)
        | ArchiveRecords::Sessions(v)
        | ArchiveRecords::Metrics(v) => v.iter().map(|v| v.received_at).collect(),
    };
    Ok((
        *times.first().ok_or(ArchiveStoreError::InvalidData)?,
        *times.last().ok_or(ArchiveStoreError::InvalidData)?,
    ))
}
fn empty(k: ArchiveKind) -> ArchiveRecords {
    match k {
        ArchiveKind::Event => ArchiveRecords::Events(Vec::new()),
        ArchiveKind::Log => ArchiveRecords::Logs(Vec::new()),
        ArchiveKind::Span => ArchiveRecords::Spans(Vec::new()),
        ArchiveKind::Session => ArchiveRecords::Sessions(Vec::new()),
        ArchiveKind::Metric => ArchiveRecords::Metrics(Vec::new()),
    }
}
fn invalid<T>(_: T) -> ArchiveStoreError {
    ArchiveStoreError::InvalidData
}
fn unavailable<T>(_: T) -> ArchiveStoreError {
    ArchiveStoreError::Unavailable
}

#[cfg(test)]
pub(crate) mod tests;
