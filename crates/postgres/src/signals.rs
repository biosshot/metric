//! Native bounded Logs, Spans, trace correlation and rebuildable statistics.
use crate::{PostgresDatabase, PostgresError, codec, signal_storage};
use metric_domain::{
    EventId, ProjectId, Timestamp,
    signals::{
        LogId, LogRecord, LogSeverity, PerformanceBucket, SignalBody, SignalCursor, SignalPage,
        SpanId, SpanOperationClass, SpanRecord, SpanRecordId, TraceId, TraceView,
    },
};
use metric_ports::{
    DurableOutcome, LogQuery, PerformanceQuery, PortFuture, SegmentQuery, SignalStore,
    SignalStoreError,
};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};
use std::collections::{BTreeMap, BTreeSet};

const HOUR: i64 = 3_600_000;
const MAX_BODY: usize = 1024 * 1024;
const MAX_BATCH: usize = 10_000;
const MAX_BATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_READ_BYTES: i64 = 8 * 1024 * 1024;
const LOG_SELECT: &str = "SELECT * FROM metric.logs";
const SPAN_SELECT: &str = "SELECT * FROM metric.spans";

#[derive(Clone, Copy, Debug)]
pub struct SignalRetention {
    pub logs_days: u32,
    pub spans_days: u32,
    pub span_stats_hourly_days: u32,
    pub archive: bool,
}
impl Default for SignalRetention {
    fn default() -> Self {
        Self {
            logs_days: 30,
            spans_days: 30,
            span_stats_hourly_days: 90,
            archive: false,
        }
    }
}
#[derive(Clone)]
pub struct PostgresSignalStore {
    database: PostgresDatabase,
    retention: SignalRetention,
}
enum Record {
    Log(LogRecord),
    Span(SpanRecord),
}
struct Prepared {
    record: Record,
    body: Vec<u8>,
    body_size: i32,
    expires: i64,
    dimensions: Vec<u8>,
}
impl Prepared {
    fn key(&self) -> (i32, [u8; 16]) {
        match &self.record {
            Record::Log(r) => (r.project_id.get(), r.id.as_bytes()),
            Record::Span(r) => (r.project_id.get(), r.id.as_bytes()),
        }
    }
}
impl PostgresSignalStore {
    pub fn new(
        database: PostgresDatabase,
        retention: SignalRetention,
    ) -> Result<Self, PostgresError> {
        if [
            retention.logs_days,
            retention.spans_days,
            retention.span_stats_hourly_days,
        ]
        .into_iter()
        .any(|days| !(1..=3650).contains(&days))
        {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self {
            database,
            retention,
        })
    }
    async fn persist(&self, records: Vec<Record>) -> Result<Vec<DurableOutcome>, SignalStoreError> {
        if records.len() > MAX_BATCH {
            return Err(SignalStoreError::InvalidData);
        }
        if records.is_empty() {
            return Ok(vec![]);
        }
        let spans = matches!(records[0], Record::Span(_));
        let mut prepared = Vec::with_capacity(records.len());
        let mut bytes = 0_usize;
        for record in records {
            let (body, received, days, dimensions, size) = match &record {
                Record::Log(r) => {
                    validate_log(r)?;
                    (
                        r.body.as_bytes(),
                        r.received_at,
                        self.retention.logs_days,
                        vec![],
                        log_size(r),
                    )
                }
                Record::Span(r) => {
                    validate_span(r)?;
                    (
                        r.body.as_bytes(),
                        r.received_at,
                        self.retention.spans_days,
                        dimensions(r)?,
                        span_size(r),
                    )
                }
            };
            bytes = bytes
                .checked_add(size)
                .ok_or(SignalStoreError::InvalidData)?;
            if bytes > MAX_BATCH_BYTES {
                return Err(SignalStoreError::InvalidData);
            }
            let body_size = body.len() as i32;
            let body = encode_body(body)?;
            let expires = expiry(received.unix_millis(), days)?;
            prepared.push(Prepared {
                record,
                body,
                body_size,
                expires,
                dimensions,
            });
        }
        let projects: Vec<_> = prepared
            .iter()
            .map(|r| r.key().0)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        lock_projects(&mut transaction, &projects).await?;
        let mut inserted = BTreeSet::new();
        for chunk in prepared.chunks(1000) {
            let mut query = QueryBuilder::<Postgres>::new(if spans {
                "INSERT INTO metric.spans(project_id,record_id,received_at,started_at_ns,duration_ns,trace_id,span_id,parent_span_id,is_segment,operation_class,operation,status,name,environment,release,service,insight_flags,dimensions,body,body_size,expire_at,archive_due) "
            } else {
                "INSERT INTO metric.logs(project_id,log_id,received_at,occurred_at_ns,severity,message,message_folded,trace_id,span_id,environment,release,service,body,body_size,expire_at,archive_due) "
            });
            query.push_values(chunk, |mut row, value| {
                match &value.record {
                    Record::Log(r) => {
                        row.push_bind(r.project_id.get())
                            .push_bind(r.id.as_bytes().to_vec())
                            .push_bind(r.received_at.unix_millis())
                            .push_bind(r.occurred_at_ns)
                            .push_bind(r.severity.code())
                            .push_bind(r.message.as_bytes())
                            .push_bind(r.message.to_lowercase().into_bytes())
                            .push_bind(r.trace_id.map(|id| id.as_bytes().to_vec()))
                            .push_bind(r.span_id.map(|id| id.as_bytes().to_vec()))
                            .push_bind(r.environment.as_deref().map(str::as_bytes))
                            .push_bind(r.release.as_deref().map(str::as_bytes))
                            .push_bind(r.service.as_deref().map(str::as_bytes));
                    }
                    Record::Span(r) => {
                        row.push_bind(r.project_id.get())
                            .push_bind(r.id.as_bytes().to_vec())
                            .push_bind(r.received_at.unix_millis())
                            .push_bind(r.started_at_ns)
                            .push_bind(r.duration_ns)
                            .push_bind(r.trace_id.as_bytes().to_vec())
                            .push_bind(r.span_id.as_bytes().to_vec())
                            .push_bind(r.parent_span_id.map(|id| id.as_bytes().to_vec()))
                            .push_bind(r.is_segment)
                            .push_bind(r.operation_class.code())
                            .push_bind(r.operation.as_bytes())
                            .push_bind(r.status.as_bytes())
                            .push_bind(r.name.as_bytes())
                            .push_bind(r.environment.as_deref().map(str::as_bytes))
                            .push_bind(r.release.as_deref().map(str::as_bytes))
                            .push_bind(r.service.as_deref().map(str::as_bytes))
                            .push_bind(i64::from(r.insight_flags))
                            .push_bind(&value.dimensions);
                    }
                }
                row.push_bind(&value.body)
                    .push_bind(value.body_size)
                    .push_bind(if self.retention.archive {
                        None
                    } else {
                        Some(value.expires)
                    })
                    .push_bind(if self.retention.archive {
                        Some(value.expires)
                    } else {
                        None
                    });
            });
            query.push(if spans {
                " ON CONFLICT(project_id,record_id) DO NOTHING RETURNING project_id,record_id AS id"
            } else {
                " ON CONFLICT(project_id,log_id) DO NOTHING RETURNING project_id,log_id AS id"
            });
            for row in query
                .build()
                .fetch_all(&mut *transaction)
                .await
                .map_err(write_error)?
            {
                inserted.insert((
                    row.try_get::<i32, _>("project_id").map_err(invalid)?,
                    fixed::<16>(&row, "id")?,
                ));
            }
        }
        // Verify Mongo-compatible duplicate identities in one bounded PK join.
        let project_keys: Vec<_> = prepared.iter().map(|v| v.key().0).collect();
        let ids: Vec<_> = prepared.iter().map(|v| v.key().1.to_vec()).collect();
        let rows=sqlx::query(if spans {"SELECT DISTINCT s.project_id,s.record_id AS id,s.trace_id,s.span_id FROM unnest($1::integer[],$2::bytea[]) requested(p,id) JOIN metric.spans s ON s.project_id=requested.p AND s.record_id=requested.id"}else{"SELECT DISTINCT s.project_id,s.log_id AS id,s.occurred_at_ns,s.message FROM unnest($1::integer[],$2::bytea[]) requested(p,id) JOIN metric.logs s ON s.project_id=requested.p AND s.log_id=requested.id"}).bind(project_keys).bind(ids).fetch_all(&mut *transaction).await.map_err(unavailable)?;
        let mut current = BTreeMap::new();
        for row in rows {
            current.insert(
                (
                    row.try_get::<i32, _>("project_id").map_err(invalid)?,
                    fixed::<16>(&row, "id")?,
                ),
                row,
            );
        }
        let mut outcomes = Vec::with_capacity(prepared.len());
        for value in &prepared {
            let key = value.key();
            let row = current.get(&key).ok_or(SignalStoreError::Unavailable)?;
            let matches = match &value.record {
                Record::Log(r) => {
                    row.try_get::<i64, _>("occurred_at_ns").map_err(invalid)? == r.occurred_at_ns
                        && row.try_get::<Vec<u8>, _>("message").map_err(invalid)?
                            == r.message.as_bytes()
                }
                Record::Span(r) => {
                    fixed::<16>(row, "trace_id")? == r.trace_id.as_bytes()
                        && fixed::<8>(row, "span_id")? == r.span_id.as_bytes()
                }
            };
            if !matches {
                return Err(SignalStoreError::Conflict);
            }
            outcomes.push(if inserted.remove(&key) {
                DurableOutcome::Accepted
            } else {
                DurableOutcome::Duplicate
            });
        }
        if spans {
            for p in &projects {
                // Derived state is best effort: its failure cannot reject a durable
                // raw Span. A savepoint retains pending markers for a later retry.
                sqlx::query("SAVEPOINT signal_rollup")
                    .execute(&mut *transaction)
                    .await
                    .map_err(unavailable)?;
                if self.rollup(&mut transaction, *p, None, true).await.is_err() {
                    sqlx::query("ROLLBACK TO SAVEPOINT signal_rollup")
                        .execute(&mut *transaction)
                        .await
                        .map_err(unavailable)?;
                    tracing::warn!(project_id = *p, "PostgreSQL Span rollup deferred");
                }
                sqlx::query("RELEASE SAVEPOINT signal_rollup")
                    .execute(&mut *transaction)
                    .await
                    .map_err(unavailable)?;
            }
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(outcomes)
    }
    async fn rollup(
        &self,
        connection: &mut PgConnection,
        project: i32,
        bounds: Option<(i64, i64)>,
        pending: bool,
    ) -> Result<u64, SignalStoreError> {
        let count: i64 =
            sqlx::query_scalar("SELECT metric.refresh_span_stats($1,$2,$3,$4,10000,$5)")
                .bind(project)
                .bind(bounds.map(|b| b.0))
                .bind(bounds.map(|b| b.1))
                .bind(pending)
                .bind(self.retention.span_stats_hourly_days as i32)
                .fetch_one(connection)
                .await
                .map_err(write_error)?;
        u64::try_from(count).map_err(invalid)
    }
    async fn logs(
        &self,
        project: ProjectId,
        query: LogQuery,
    ) -> Result<SignalPage<LogRecord>, SignalStoreError> {
        validate_query(query.from_ns, query.until_ns, query.limit)?;
        let mut sql = begin(LOG_SELECT);
        sql.push(" WHERE project_id=")
            .push_bind(project.get())
            .push(" AND occurred_at_ns>=")
            .push_bind(query.from_ns)
            .push(" AND occurred_at_ns<")
            .push_bind(query.until_ns);
        if let Some(severity) = query.severity {
            sql.push(" AND severity=").push_bind(severity.code());
        }
        if let Some(message) = &query.message {
            bounded(message, 8192)?;
            let folded = message.to_lowercase().into_bytes();
            if folded.len() >= 3 {
                sql.push(" AND message_trigrams @> metric.signal_trigrams(")
                    .push_bind(folded.clone())
                    .push(")");
            }
            sql.push(" AND position(")
                .push_bind(folded)
                .push(" IN message_folded)>0");
        }
        filters(&mut sql, &query.environment, &query.release, &query.service)?;
        if let Some(trace) = query.trace_id {
            sql.push(" AND trace_id=")
                .push_bind(trace.as_bytes().to_vec());
        }
        anchor(&mut sql, query.before, false);
        let mut connection = self.database.pool.acquire().await.map_err(unavailable)?;
        let (rows, more) = finish_page(sql, query.limit + 1, false, false, &mut connection).await?;
        let mut items = rows.iter().map(decode_log).collect::<Result<Vec<_>, _>>()?;
        let more = more || items.len() > query.limit;
        items.truncate(query.limit);
        let next = if more {
            items.last().map(|r| SignalCursor {
                time_ns: r.occurred_at_ns,
                id: r.id.as_bytes(),
            })
        } else {
            None
        };
        Ok(SignalPage { items, next })
    }
    async fn segments(
        &self,
        project: ProjectId,
        query: SegmentQuery,
    ) -> Result<SignalPage<SpanRecord>, SignalStoreError> {
        validate_query(query.from_ns, query.until_ns, query.limit)?;
        let mut sql = begin(SPAN_SELECT);
        sql.push(" WHERE project_id=")
            .push_bind(project.get())
            .push(" AND is_segment AND started_at_ns>=")
            .push_bind(query.from_ns)
            .push(" AND started_at_ns<")
            .push_bind(query.until_ns);
        filters(&mut sql, &query.environment, &query.release, &query.service)?;
        anchor(&mut sql, query.before, true);
        let mut connection = self.database.pool.acquire().await.map_err(unavailable)?;
        let (rows, more) = finish_page(sql, query.limit + 1, true, false, &mut connection).await?;
        let mut items = rows
            .iter()
            .map(decode_span)
            .collect::<Result<Vec<_>, _>>()?;
        let more = more || items.len() > query.limit;
        items.truncate(query.limit);
        let next = if more {
            items.last().map(|r| SignalCursor {
                time_ns: r.started_at_ns,
                id: r.id.as_bytes(),
            })
        } else {
            None
        };
        Ok(SignalPage { items, next })
    }
    async fn trace_view(
        &self,
        projects: Vec<ProjectId>,
        trace: TraceId,
        maximum_spans: usize,
        maximum_logs: usize,
    ) -> Result<TraceView, SignalStoreError> {
        if projects.is_empty()
            || projects.len() > 32
            || maximum_spans == 0
            || maximum_spans > 2000
            || maximum_logs > 1000
        {
            return Err(SignalStoreError::InvalidData);
        }
        let projects: Vec<_> = projects
            .into_iter()
            .map(ProjectId::get)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
        let (rows, more) =
            trace_rows(&mut transaction, &projects, trace, maximum_spans + 1, true).await?;
        let mut spans = rows
            .iter()
            .map(decode_span)
            .collect::<Result<Vec<_>, _>>()?;
        let omitted = if more {
            1
        } else {
            spans.len().saturating_sub(maximum_spans) as u32
        };
        let mut partial = more || spans.len() > maximum_spans;
        spans.truncate(maximum_spans);
        let mut logs = vec![];
        if maximum_logs > 0 {
            let (rows, more) =
                trace_rows(&mut transaction, &projects, trace, maximum_logs + 1, false).await?;
            logs = rows.iter().map(decode_log).collect::<Result<Vec<_>, _>>()?;
            partial |= more || logs.len() > maximum_logs;
            logs.truncate(maximum_logs);
        }
        let errors=sqlx::query("SELECT event_id FROM unnest($1::integer[]) p JOIN LATERAL(SELECT event_id,occurred_at,project_id FROM metric.error_events WHERE project_id=p AND trace_id=$2 AND processing_state=2 ORDER BY occurred_at,event_id LIMIT 100) e ON true ORDER BY occurred_at,event_id,project_id LIMIT 100").bind(&projects).bind(trace.as_bytes().as_slice()).fetch_all(&mut *transaction).await.map_err(unavailable)?.iter().map(|r|fixed(r,"event_id").map(EventId::from_bytes)).collect::<Result<Vec<_>,_>>()?;
        transaction.commit().await.map_err(unavailable)?;
        if spans.is_empty() && logs.is_empty() && errors.is_empty() {
            return Err(SignalStoreError::NotFound);
        }
        Ok(TraceView {
            trace_id: trace,
            spans,
            logs,
            errors,
            partial,
            omitted_spans: omitted,
        })
    }
    async fn performance_rows(
        &self,
        project: ProjectId,
        query: PerformanceQuery,
    ) -> Result<Vec<PerformanceBucket>, SignalStoreError> {
        validate_query(
            query.from.unix_millis(),
            query.until.unix_millis(),
            query.limit,
        )?;
        let mut selection = QueryBuilder::<Postgres>::new(
            "SELECT hour,dimensions,environment,release,service,representative_trace_id,occurrence_count,failure_count,duration_sum,samples FROM metric.span_stats_hourly WHERE project_id=",
        );
        selection
            .push_bind(project.get())
            .push(" AND hour>=")
            .push_bind(query.from.unix_millis())
            .push(" AND hour<")
            .push_bind(query.until.unix_millis());
        // Optional dimensions use native binary projections and bound equality.
        if query.environment.is_some() || query.release.is_some() || query.service.is_some() {
            // Filter projections are stored separately so PostgreSQL never parses
            // JSON text containing accepted NUL metadata.
            selection.push(" AND ");
            stat_filters(&mut selection, &query)?;
        }
        selection
            .push(" ORDER BY hour DESC,occurrence_count DESC,dimension_key LIMIT ")
            .push_bind(query.limit as i64);
        let rows = selection
            .build()
            .fetch_all(&self.database.pool)
            .await
            .map_err(unavailable)?;
        rows.iter().map(decode_stat).collect()
    }
    async fn rebuild(
        &self,
        project: ProjectId,
        from: Timestamp,
        until: Timestamp,
    ) -> Result<u64, SignalStoreError> {
        let (lower, upper, lower_ns, upper_ns) = rebuild_bounds(from, until)?;
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        lock_projects(&mut transaction, &[project.get()]).await?;
        sqlx::query(
            "DELETE FROM metric.span_stats_hourly WHERE project_id=$1 AND hour>=$2 AND hour<$3",
        )
        .bind(project.get())
        .bind(lower)
        .bind(upper)
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;
        let count = self
            .rollup(
                &mut transaction,
                project.get(),
                Some((lower_ns, upper_ns)),
                false,
            )
            .await?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(count)
    }
}
impl SignalStore for PostgresSignalStore {
    fn persist_logs(
        &self,
        records: Vec<LogRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(self.persist(records.into_iter().map(Record::Log).collect()))
    }
    fn persist_spans(
        &self,
        records: Vec<SpanRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(self.persist(records.into_iter().map(Record::Span).collect()))
    }
    fn list_logs(
        &self,
        project: ProjectId,
        query: LogQuery,
    ) -> PortFuture<'_, Result<SignalPage<LogRecord>, SignalStoreError>> {
        Box::pin(self.logs(project, query))
    }
    fn load_log(
        &self,
        project: ProjectId,
        id: LogId,
    ) -> PortFuture<'_, Result<LogRecord, SignalStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM metric.logs WHERE project_id=$1 AND log_id=$2")
                .bind(project.get())
                .bind(id.as_bytes().as_slice())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(unavailable)?
                .ok_or(SignalStoreError::NotFound)?;
            decode_log(&row)
        })
    }
    fn list_segments(
        &self,
        project: ProjectId,
        query: SegmentQuery,
    ) -> PortFuture<'_, Result<SignalPage<SpanRecord>, SignalStoreError>> {
        Box::pin(self.segments(project, query))
    }
    fn trace(
        &self,
        projects: Vec<ProjectId>,
        trace: TraceId,
        maximum_spans: usize,
        maximum_logs: usize,
    ) -> PortFuture<'_, Result<TraceView, SignalStoreError>> {
        Box::pin(self.trace_view(projects, trace, maximum_spans, maximum_logs))
    }
    fn performance(
        &self,
        project: ProjectId,
        query: PerformanceQuery,
    ) -> PortFuture<'_, Result<Vec<PerformanceBucket>, SignalStoreError>> {
        Box::pin(self.performance_rows(project, query))
    }
    fn rebuild_span_stats(
        &self,
        project: ProjectId,
        from: Timestamp,
        until: Timestamp,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
        Box::pin(self.rebuild(project, from, until))
    }
}
async fn lock_projects(
    connection: &mut PgConnection,
    projects: &[i32],
) -> Result<(), SignalStoreError> {
    signal_storage::lock_projects(connection, projects).await?;
    sqlx::query("INSERT INTO metric.signal_project_locks(project_id) SELECT unnest($1::integer[]) ORDER BY 1 ON CONFLICT(project_id) DO NOTHING").bind(projects).execute(&mut *connection).await.map_err(unavailable)?;
    sqlx::query("SELECT project_id FROM metric.signal_project_locks WHERE project_id=ANY($1) ORDER BY project_id FOR UPDATE").bind(projects).fetch_all(connection).await.map_err(unavailable)?;
    Ok(())
}
fn begin(select: &'static str) -> QueryBuilder<Postgres> {
    let mut sql = QueryBuilder::new("WITH candidates AS (");
    sql.push(select);
    sql
}
fn anchor(sql: &mut QueryBuilder<Postgres>, before: Option<SignalCursor>, spans: bool) {
    if let Some(before) = before {
        sql.push(if spans {
            " AND (started_at_ns,record_id)<("
        } else {
            " AND (occurred_at_ns,log_id)<("
        })
        .push_bind(before.time_ns)
        .push(",")
        .push_bind(before.id.to_vec())
        .push(")");
    }
}
fn filters(
    sql: &mut QueryBuilder<Postgres>,
    environment: &Option<Box<str>>,
    release: &Option<Box<str>>,
    service: &Option<Box<str>>,
) -> Result<(), SignalStoreError> {
    for (value, column, maximum) in [
        (environment, " AND environment=", 128),
        (release, " AND release=", 256),
        (service, " AND service=", 256),
    ] {
        if let Some(value) = value {
            bounded(value, maximum)?;
            sql.push(column).push_bind(value.as_bytes().to_vec());
        }
    }
    Ok(())
}
fn stat_filters(
    sql: &mut QueryBuilder<Postgres>,
    query: &PerformanceQuery,
) -> Result<(), SignalStoreError> {
    let mut first = true;
    for (value, column, maximum) in [
        (&query.environment, "environment=", 128),
        (&query.release, "release=", 256),
        (&query.service, "service=", 256),
    ] {
        if let Some(value) = value {
            bounded(value, maximum)?;
            if !first {
                sql.push(" AND ");
            }
            first = false;
            sql.push(column).push_bind(value.as_bytes().to_vec());
        }
    }
    Ok(())
}
async fn finish_page(
    mut sql: QueryBuilder<Postgres>,
    maximum: usize,
    spans: bool,
    ascending: bool,
    connection: &mut PgConnection,
) -> Result<(Vec<PgRow>, bool), SignalStoreError> {
    let order = match (spans, ascending) {
        (true, true) => "started_at_ns,span_id,record_id,project_id",
        (true, false) => "started_at_ns DESC,record_id DESC",
        (false, true) => "occurred_at_ns,log_id,project_id",
        (false, false) => "occurred_at_ns DESC,log_id DESC",
    };
    sql.push(" ORDER BY ").push(order).push(" LIMIT ").push_bind(maximum as i64).push("), budget AS (SELECT *,sum(record_bytes) OVER(ORDER BY ").push(order).push(" ROWS UNBOUNDED PRECEDING) read_bytes,count(*) OVER() selected_count FROM candidates) SELECT * FROM budget WHERE read_bytes<=").push_bind(MAX_READ_BYTES).push(" ORDER BY ").push(order);
    let rows = sql
        .build()
        .fetch_all(connection)
        .await
        .map_err(unavailable)?;
    let more = rows
        .first()
        .map(|row| {
            row.try_get::<i64, _>("selected_count")
                .map(|count| count > rows.len() as i64)
                .map_err(invalid)
        })
        .transpose()?
        .unwrap_or(false);
    Ok((rows, more))
}
async fn trace_rows(
    connection: &mut PgConnection,
    projects: &[i32],
    trace: TraceId,
    maximum: usize,
    spans: bool,
) -> Result<(Vec<PgRow>, bool), SignalStoreError> {
    let mut sql = QueryBuilder::<Postgres>::new("WITH candidates AS (SELECT item.* FROM unnest(");
    sql.push_bind(projects.to_vec())
        .push("::integer[]) AS requested(p) JOIN LATERAL(")
        .push(if spans { SPAN_SELECT } else { LOG_SELECT })
        .push(" WHERE project_id=requested.p AND trace_id=")
        .push_bind(trace.as_bytes().to_vec())
        .push(if spans {
            " ORDER BY started_at_ns,span_id,record_id LIMIT "
        } else {
            " ORDER BY occurred_at_ns,log_id LIMIT "
        })
        .push_bind(maximum as i64)
        .push(") item ON true");
    finish_page(sql, maximum, spans, true, connection).await
}
fn validate_query(from: i64, until: i64, limit: usize) -> Result<(), SignalStoreError> {
    if from >= until || limit == 0 || limit > 200 {
        Err(SignalStoreError::InvalidData)
    } else {
        Ok(())
    }
}
fn bounded(value: &str, maximum: usize) -> Result<(), SignalStoreError> {
    if value.chars().count() > maximum {
        Err(SignalStoreError::InvalidData)
    } else {
        Ok(())
    }
}
fn validate_optional(value: &Option<Box<str>>, maximum: usize) -> Result<(), SignalStoreError> {
    if let Some(value) = value {
        bounded(value, maximum)?;
    }
    Ok(())
}
fn validate_log(r: &LogRecord) -> Result<(), SignalStoreError> {
    bounded(&r.message, 8192)?;
    validate_optional(&r.environment, 128)?;
    validate_optional(&r.release, 256)?;
    validate_optional(&r.service, 256)
}
fn validate_span(r: &SpanRecord) -> Result<(), SignalStoreError> {
    if r.duration_ns < 0 {
        return Err(SignalStoreError::InvalidData);
    }
    bounded(&r.operation, 128)?;
    bounded(&r.status, 64)?;
    bounded(&r.name, 1024)?;
    validate_optional(&r.environment, 128)?;
    validate_optional(&r.release, 256)?;
    validate_optional(&r.service, 256)
}
fn log_size(r: &LogRecord) -> usize {
    r.body.as_bytes().len()
        + r.message.len()
        + optional_size(&r.environment)
        + optional_size(&r.release)
        + optional_size(&r.service)
        + 256
}
fn span_size(r: &SpanRecord) -> usize {
    r.body.as_bytes().len()
        + r.operation.len()
        + r.status.len()
        + r.name.len()
        + optional_size(&r.environment)
        + optional_size(&r.release)
        + optional_size(&r.service)
        + 256
}
fn optional_size(value: &Option<Box<str>>) -> usize {
    value.as_deref().map_or(0, str::len)
}
fn dimensions(r: &SpanRecord) -> Result<Vec<u8>, SignalStoreError> {
    serde_json::to_vec(&(
        r.name.as_ref(),
        r.service.as_deref(),
        r.environment.as_deref(),
        r.release.as_deref(),
        r.operation_class.code(),
    ))
    .map_err(invalid)
}
fn encode_body(body: &[u8]) -> Result<Vec<u8>, SignalStoreError> {
    validate_body(body)?;
    let compressed = zstd::bulk::compress(body, 3).map_err(invalid)?;
    let use_compressed = compressed.len() + 64 <= body.len();
    let mut encoded = vec![1, u8::from(use_compressed)];
    encoded.extend_from_slice(if use_compressed { &compressed } else { body });
    Ok(encoded)
}
fn validate_body(body: &[u8]) -> Result<(), SignalStoreError> {
    if body.is_empty()
        || body.len() > MAX_BODY
        || serde_json::from_slice::<serde_json::Value>(body).is_err()
    {
        Err(SignalStoreError::InvalidData)
    } else {
        Ok(())
    }
}
pub(crate) fn decode_body(row: &PgRow) -> Result<SignalBody, SignalStoreError> {
    let encoded: Vec<u8> = row.try_get("body").map_err(invalid)?;
    if encoded.first() != Some(&1) || encoded.len() < 2 {
        return Err(SignalStoreError::InvalidData);
    }
    let body = match encoded[1] {
        0 if encoded.len() - 2 <= MAX_BODY => encoded[2..].to_vec(),
        1 => zstd::bulk::decompress(&encoded[2..], MAX_BODY).map_err(invalid)?,
        _ => return Err(SignalStoreError::InvalidData),
    };
    validate_body(&body)?;
    if row.try_get::<i32, _>("body_size").map_err(invalid)? != body.len() as i32 {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(SignalBody::new(body))
}
fn decode_log(row: &PgRow) -> Result<LogRecord, SignalStoreError> {
    let r = LogRecord {
        id: LogId::from_bytes(fixed(row, "log_id")?),
        project_id: ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?,
        received_at: timestamp(row, "received_at")?,
        occurred_at_ns: row.try_get("occurred_at_ns").map_err(invalid)?,
        severity: LogSeverity::from_code(row.try_get("severity").map_err(invalid)?)
            .map_err(invalid)?,
        message: text(row, "message")?,
        trace_id: optional_id(row, "trace_id")?.map(TraceId::from_bytes),
        span_id: optional_id(row, "span_id")?.map(SpanId::from_bytes),
        environment: optional_text(row, "environment")?,
        release: optional_text(row, "release")?,
        service: optional_text(row, "service")?,
        body: decode_body(row)?,
    };
    validate_log(&r)?;
    if row.try_get::<i32, _>("record_bytes").map_err(invalid)? != log_size(&r) as i32
        || row
            .try_get::<Vec<u8>, _>("message_folded")
            .map_err(invalid)?
            != r.message.to_lowercase().as_bytes()
    {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(r)
}
fn decode_span(row: &PgRow) -> Result<SpanRecord, SignalStoreError> {
    let r = SpanRecord {
        id: SpanRecordId::from_bytes(fixed(row, "record_id")?),
        project_id: ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?,
        received_at: timestamp(row, "received_at")?,
        started_at_ns: row.try_get("started_at_ns").map_err(invalid)?,
        duration_ns: row.try_get("duration_ns").map_err(invalid)?,
        trace_id: TraceId::from_bytes(fixed(row, "trace_id")?),
        span_id: SpanId::from_bytes(fixed(row, "span_id")?),
        parent_span_id: optional_id(row, "parent_span_id")?.map(SpanId::from_bytes),
        is_segment: row.try_get("is_segment").map_err(invalid)?,
        operation_class: SpanOperationClass::from_code(
            row.try_get("operation_class").map_err(invalid)?,
        )
        .map_err(invalid)?,
        operation: text(row, "operation")?,
        status: text(row, "status")?,
        name: text(row, "name")?,
        environment: optional_text(row, "environment")?,
        release: optional_text(row, "release")?,
        service: optional_text(row, "service")?,
        insight_flags: u32::try_from(row.try_get::<i64, _>("insight_flags").map_err(invalid)?)
            .map_err(invalid)?,
        body: decode_body(row)?,
    };
    validate_span(&r)?;
    if row.try_get::<i32, _>("record_bytes").map_err(invalid)? != span_size(&r) as i32
        || row.try_get::<Vec<u8>, _>("dimensions").map_err(invalid)? != dimensions(&r)?
    {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(r)
}
fn decode_stat(row: &PgRow) -> Result<PerformanceBucket, SignalStoreError> {
    let raw: Vec<u8> = row.try_get("dimensions").map_err(invalid)?;
    let (name, service, environment, release, operation): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i32,
    ) = serde_json::from_slice(&raw).map_err(invalid)?;
    let count = u64::try_from(row.try_get::<i64, _>("occurrence_count").map_err(invalid)?)
        .map_err(invalid)?;
    let failures =
        u64::try_from(row.try_get::<i64, _>("failure_count").map_err(invalid)?).map_err(invalid)?;
    let duration: i64 = row.try_get("duration_sum").map_err(invalid)?;
    let mut samples: Vec<i64> = row.try_get("samples").map_err(invalid)?;
    if count == 0
        || failures > count
        || duration < 0
        || samples.is_empty()
        || samples.len() > 2048
        || samples.iter().any(|d| *d < 0)
    {
        return Err(SignalStoreError::InvalidData);
    }
    samples.sort_unstable();
    bounded(&name, 1024)?;
    let service = service.map(String::into_boxed_str);
    let environment = environment.map(String::into_boxed_str);
    let release = release.map(String::into_boxed_str);
    validate_optional(&service, 256)?;
    validate_optional(&environment, 128)?;
    validate_optional(&release, 256)?;
    if optional_text(row, "environment")? != environment
        || optional_text(row, "release")? != release
        || optional_text(row, "service")? != service
    {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(PerformanceBucket {
        hour: timestamp(row, "hour")?,
        name: name.into(),
        service,
        environment,
        release,
        representative_trace_id: TraceId::from_bytes(fixed(row, "representative_trace_id")?),
        operation: SpanOperationClass::from_code(operation).map_err(invalid)?,
        count,
        failure_count: failures,
        average_duration_ms: duration as f64 / count as f64 / 1_000_000.0,
        p50_ms: percentile(&samples, 50),
        p75_ms: percentile(&samples, 75),
        p90_ms: percentile(&samples, 90),
        p95_ms: percentile(&samples, 95),
        p99_ms: percentile(&samples, 99),
    })
}
fn percentile(samples: &[i64], p: usize) -> f64 {
    samples[((samples.len() - 1) * p).div_ceil(100)] as f64 / 1_000_000.0
}
fn rebuild_bounds(
    from: Timestamp,
    until: Timestamp,
) -> Result<(i64, i64, i64, i64), SignalStoreError> {
    if from >= until {
        return Err(SignalStoreError::InvalidData);
    }
    let lower = from.unix_millis().div_euclid(HOUR) * HOUR;
    let upper = until.unix_millis();
    let upper = upper.div_euclid(HOUR) * HOUR + i64::from(upper.rem_euclid(HOUR) != 0) * HOUR;
    Ok((
        lower,
        upper,
        lower
            .checked_mul(1_000_000)
            .ok_or(SignalStoreError::InvalidData)?,
        upper
            .checked_mul(1_000_000)
            .ok_or(SignalStoreError::InvalidData)?,
    ))
}
fn expiry(at: i64, days: u32) -> Result<i64, SignalStoreError> {
    let expiry = at
        .checked_add(i64::from(days) * 86_400_000)
        .ok_or(SignalStoreError::InvalidData)?;
    Timestamp::from_unix_millis(expiry).map_err(invalid)?;
    Ok(expiry)
}
fn text(row: &PgRow, column: &str) -> Result<Box<str>, SignalStoreError> {
    codec::decode_utf8(row.try_get(column).map_err(invalid)?)
        .map(String::into_boxed_str)
        .map_err(invalid)
}
fn optional_text(row: &PgRow, column: &str) -> Result<Option<Box<str>>, SignalStoreError> {
    row.try_get::<Option<Vec<u8>>, _>(column)
        .map_err(invalid)?
        .map(|b| {
            codec::decode_utf8(b)
                .map(String::into_boxed_str)
                .map_err(invalid)
        })
        .transpose()
}
fn timestamp(row: &PgRow, column: &str) -> Result<Timestamp, SignalStoreError> {
    Timestamp::from_unix_millis(row.try_get(column).map_err(invalid)?).map_err(invalid)
}
fn fixed<const N: usize>(row: &PgRow, column: &str) -> Result<[u8; N], SignalStoreError> {
    row.try_get::<Vec<u8>, _>(column)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
fn optional_id<const N: usize>(
    row: &PgRow,
    column: &str,
) -> Result<Option<[u8; N]>, SignalStoreError> {
    row.try_get::<Option<Vec<u8>>, _>(column)
        .map_err(invalid)?
        .map(|b| b.try_into().map_err(invalid))
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
        sqlx::Error::Database(ref e)
            if e.code()
                .as_deref()
                .is_some_and(|c| c == "22003" || c.starts_with("23")) =>
        {
            SignalStoreError::InvalidData
        }
        _ => SignalStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
