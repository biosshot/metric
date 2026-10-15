//! Bounded native retention, expiry and counter repairs behind the existing scheduler.
use crate::{MonitorRetention, PostgresDatabase, PostgresMonitorStore};
use metric_domain::{EventKey, Timestamp};
use metric_ports::{
    MaintenanceCursor, MaintenanceDisposition, MaintenanceRequest, MaintenanceResult,
    MaintenanceStore, MaintenanceStoreError, MaintenanceTask, MonitorStore, PortFuture,
};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, MaintenanceStoreError>;
const MAX_RETENTION: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);
const EVENTS: u8 = 16;
const EVENT_EXPIRY: u8 = 17;
const HOURLY: u8 = 48;
const HOURLY_EXPIRY: u8 = 49;
const ISSUES: u8 = 80;
const CATALOGS: u8 = 81;

#[derive(Clone)]
pub struct PostgresMaintenanceStore {
    database: PostgresDatabase,
    monitor_retention: MonitorRetention,
}
impl PostgresMaintenanceStore {
    #[must_use]
    pub fn new(database: PostgresDatabase) -> Self {
        Self {
            database,
            monitor_retention: MonitorRetention::default(),
        }
    }
    #[must_use]
    pub fn with_monitor_retention(mut self, retention: MonitorRetention) -> Self {
        self.monitor_retention = retention;
        self
    }
    async fn run_inner(&self, request: MaintenanceRequest) -> Result<MaintenanceResult> {
        validate(&request)?;
        match request.task {
            MaintenanceTask::RetryBacklog => {
                if request.cursor.is_some() {
                    return Err(MaintenanceStoreError::InvalidData);
                }
                let due: Option<i64> = sqlx::query_scalar("SELECT next_attempt_at FROM metric.error_events WHERE processing_state=0 AND next_attempt_at<=$1 ORDER BY next_attempt_at,received_at,event_key LIMIT 1")
                    .bind(request.now.unix_millis()).fetch_optional(&self.database.pool).await.map_err(unavailable)?;
                metrics::gauge!("metric_maintenance_due_retry_lag_seconds").set(
                    due.map_or(0.0, |value| {
                        request.now.unix_millis().saturating_sub(value).max(0) as f64 / 1000.0
                    }),
                );
                Ok(report(usize::from(due.is_some()), 0, None))
            }
            MaintenanceTask::EventRetention => {
                let (phase, after) = cursor(
                    &request,
                    EVENTS,
                    EVENT_EXPIRY + EVENT_TABLES.len() as u8 - 1,
                )?;
                if phase == EVENTS {
                    self.retain_events(&request, after).await
                } else {
                    self.expire(&request, phase, EVENT_EXPIRY, EVENT_TABLES)
                        .await
                }
            }
            MaintenanceTask::HourlyRetention => {
                let (phase, after) = cursor(
                    &request,
                    HOURLY,
                    HOURLY_EXPIRY + HOURLY_TABLES.len() as u8 - 1,
                )?;
                if phase == HOURLY {
                    self.retain_hourly(&request, after).await
                } else {
                    self.expire(&request, phase, HOURLY_EXPIRY, HOURLY_TABLES)
                        .await
                }
            }
            MaintenanceTask::CounterReconciliation => {
                let (phase, after) = cursor(&request, ISSUES, CATALOGS)?;
                if phase == ISSUES {
                    self.reconcile_issues(&request, after).await
                } else {
                    self.reconcile_catalogs(&request, after).await
                }
            }
            MaintenanceTask::MonitorRetention => self.repair_missed_retention(&request).await,
            MaintenanceTask::MonitorTimeouts | MaintenanceTask::MonitorMissed => {
                if request.cursor.is_some() {
                    return Err(MaintenanceStoreError::InvalidData);
                }
                let store =
                    PostgresMonitorStore::new(self.database.clone(), self.monitor_retention)
                        .map_err(invalid)?;
                let changed = if request.task == MaintenanceTask::MonitorTimeouts {
                    store
                        .terminalize_due_timeouts(request.now, request.batch_size)
                        .await
                } else {
                    store
                        .materialize_due_missed(request.now, request.batch_size)
                        .await
                }
                .map_err(|error| match error {
                    metric_ports::SignalStoreError::Unavailable
                    | metric_ports::SignalStoreError::Capacity => {
                        MaintenanceStoreError::Unavailable
                    }
                    _ => MaintenanceStoreError::InvalidData,
                })?;
                let changed = usize::try_from(changed).map_err(invalid)?;
                Ok(report(changed, changed, None))
            }
            // These two existing Scheduler probes are explicitly disabled in the
            // current adapter contract. Binary upload/GC stores own their work.
            MaintenanceTask::UploadExpiry | MaintenanceTask::BlobOrphanRegistration => {
                if request.cursor.is_some() {
                    return Err(MaintenanceStoreError::InvalidData);
                }
                Ok(MaintenanceResult {
                    disposition: MaintenanceDisposition::Disabled,
                    ..report(0, 0, None)
                })
            }
        }
    }

    // Keyset traversal bounds each repair and is safe to repeat after restart.
    // Only scheduler-created Missed runs used the incorrect default retention.
    async fn repair_missed_retention(
        &self,
        request: &MaintenanceRequest,
    ) -> Result<MaintenanceResult> {
        let after = request
            .cursor
            .as_ref()
            .map_or(&[][..], MaintenanceCursor::as_bytes);
        if !(1..=3650).contains(&self.monitor_retention.runs_days) {
            return Err(MaintenanceStoreError::InvalidData);
        }
        if !after.is_empty() && after.len() != 16 {
            return Err(MaintenanceStoreError::InvalidData);
        }
        // Traverse the primary key without a status filter: a sparse set of
        // missed runs must not require an unbounded scan to fill one batch.
        let keys: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT run_id FROM metric.monitor_runs WHERE run_id>$1 ORDER BY run_id LIMIT $2",
        )
        .bind(after)
        .bind(request.batch_size as i64)
        .fetch_all(&self.database.pool)
        .await
        .map_err(unavailable)?;
        let days_ms = i64::from(self.monitor_retention.runs_days) * 86_400_000;
        let changed = sqlx::query("UPDATE metric.monitor_runs SET delete_at=received_at+$2 WHERE run_id=ANY($1) AND status=4 AND source=1 AND delete_at IS DISTINCT FROM received_at+$2")
            .bind(&keys).bind(days_ms).execute(&self.database.pool).await.map_err(unavailable)?.rows_affected();
        let next = if keys.len() == request.batch_size {
            keys.last()
                .and_then(|key| MaintenanceCursor::new(key.clone()))
        } else {
            None
        };
        Ok(report(
            keys.len(),
            usize::try_from(changed).map_err(invalid)?,
            next,
        ))
    }

    async fn retain_events(
        &self,
        request: &MaintenanceRequest,
        after: &[u8],
    ) -> Result<MaintenanceResult> {
        if !after.is_empty() {
            let _: EventKey =
                EventKey::from_bytes(after.try_into().map_err(invalid)?).map_err(invalid)?;
        }
        let keys: Vec<Vec<u8>> = sqlx::query_scalar("SELECT event_key FROM metric.error_events WHERE event_key>$1 ORDER BY event_key LIMIT $2")
            .bind(after).bind(request.batch_size as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?;
        let mut projects = keys
            .iter()
            .map(|key| {
                EventKey::from_bytes(key.as_slice().try_into().map_err(invalid)?)
                    .map(|key| key.project_id().get())
                    .map_err(invalid)
            })
            .collect::<Result<Vec<_>>>()?;
        projects.sort_unstable();
        projects.dedup();
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        let active = lock_projects(&mut tx, &projects).await?;
        let rows = sqlx::query("SELECT event_key,received_at,processing_state,expire_at,archive_due,archive_segment FROM metric.error_events WHERE event_key=ANY($1) AND project_id=ANY($2) ORDER BY event_key FOR UPDATE")
            .bind(&keys).bind(active).fetch_all(&mut *tx).await.map_err(unavailable)?;
        let mut updates = Vec::new();
        let mut desired = Vec::new();
        let mut held = Vec::new();
        for row in rows {
            let state: i16 = row.try_get("processing_state").map_err(invalid)?;
            let expiry: Option<i64> = row.try_get("expire_at").map_err(invalid)?;
            let hold: Option<i64> = row.try_get("archive_due").map_err(invalid)?;
            let segment: Option<Vec<u8>> = row.try_get("archive_segment").map_err(invalid)?;
            if state == 0 {
                if expiry.is_some() || hold.is_some() || segment.is_some() {
                    return Err(MaintenanceStoreError::InvalidData);
                }
                continue;
            }
            if !matches!(state, 1 | 2) {
                return Err(MaintenanceStoreError::InvalidData);
            }
            if let Some(segment) = segment {
                // Keep the verified publication's hot-copy delay, independently
                // of the event's old received_at or the current retention policy.
                if segment.len() != 16 || expiry.is_none() || hold.is_some() {
                    return Err(MaintenanceStoreError::InvalidData);
                }
                continue;
            }
            let deadline = add(
                row.try_get("received_at").map_err(invalid)?,
                request.event_retention,
            )?;
            let archive = request.archive_events;
            if (archive && (hold != Some(deadline) || expiry.is_some()))
                || (!archive && (expiry != Some(deadline) || hold.is_some()))
            {
                updates.push(row.try_get::<Vec<u8>, _>("event_key").map_err(invalid)?);
                desired.push(deadline);
                held.push(archive);
            }
        }
        let changed = sqlx::query("UPDATE metric.error_events e SET expire_at=CASE WHEN u.held THEN NULL ELSE u.deadline END,archive_due=CASE WHEN u.held THEN u.deadline ELSE NULL END FROM unnest($1::bytea[],$2::bigint[],$3::boolean[]) u(key,deadline,held) WHERE e.event_key=u.key AND e.processing_state IN(1,2) AND e.archive_segment IS NULL")
            .bind(updates).bind(desired).bind(held).execute(&mut *tx).await.map_err(unavailable)?.rows_affected() as usize;
        tx.commit().await.map_err(unavailable)?;
        page(
            keys.len(),
            changed,
            request.batch_size,
            EVENTS,
            keys.last().map(Vec::as_slice),
            Some(EVENT_EXPIRY),
        )
    }

    async fn retain_hourly(
        &self,
        request: &MaintenanceRequest,
        after: &[u8],
    ) -> Result<MaintenanceResult> {
        let (project, issue, hour) = if after.is_empty() {
            (0, vec![], i64::MIN)
        } else {
            if after.len() != 28 {
                return Err(MaintenanceStoreError::InvalidData);
            }
            (
                project_id(&after[..4])?,
                after[4..20].to_vec(),
                i64::from_be_bytes(after[20..].try_into().map_err(invalid)?),
            )
        };
        let rows = sqlx::query("SELECT project_id,issue_id,bucket_start FROM metric.issue_stats_hourly WHERE (project_id,issue_id,bucket_start)>($1,$2,$3) ORDER BY project_id,issue_id,bucket_start LIMIT $4")
            .bind(project).bind(issue).bind(hour).bind(request.batch_size as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?;
        let mut projects = Vec::new();
        let mut issues = Vec::new();
        let mut hours = Vec::new();
        let mut expiry = Vec::new();
        let mut last = None;
        for row in &rows {
            let project: i32 = row.try_get("project_id").map_err(invalid)?;
            let issue: Vec<u8> = row.try_get("issue_id").map_err(invalid)?;
            if issue.len() != 16 {
                return Err(MaintenanceStoreError::InvalidData);
            }
            let hour: i64 = row.try_get("bucket_start").map_err(invalid)?;
            let mut key = project.to_be_bytes().to_vec();
            key.extend_from_slice(&issue);
            key.extend_from_slice(&hour.to_be_bytes());
            last = Some(key);
            projects.push(project);
            issues.push(issue);
            hours.push(hour);
            expiry.push(add(hour, request.hourly_retention)?);
        }
        let mut guarded = projects.clone();
        guarded.sort_unstable();
        guarded.dedup();
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        let active = lock_projects(&mut tx, &guarded).await?;
        let changed = sqlx::query("UPDATE metric.issue_stats_hourly h SET expire_at=u.expiry FROM unnest($1::integer[],$2::bytea[],$3::bigint[],$4::bigint[]) u(project,issue,hour,expiry) WHERE h.project_id=u.project AND h.issue_id=u.issue AND h.bucket_start=u.hour AND h.project_id=ANY($5) AND h.expire_at IS DISTINCT FROM u.expiry")
            .bind(projects).bind(issues).bind(hours).bind(expiry).bind(active).execute(&mut *tx).await.map_err(unavailable)?.rows_affected() as usize;
        tx.commit().await.map_err(unavailable)?;
        page(
            rows.len(),
            changed,
            request.batch_size,
            HOURLY,
            last.as_deref(),
            Some(HOURLY_EXPIRY),
        )
    }

    async fn expire(
        &self,
        request: &MaintenanceRequest,
        phase: u8,
        first: u8,
        tables: &[ExpiryTable],
    ) -> Result<MaintenanceResult> {
        if request
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.as_bytes().len() != 1)
        {
            return Err(MaintenanceStoreError::InvalidData);
        }
        let index = usize::from(phase - first);
        let spec = tables
            .get(index)
            .ok_or(MaintenanceStoreError::InvalidData)?;
        // ctid exists only inside this single statement, never in a saved cursor.
        // Row locks recheck changed deadlines and protect the selected source set.
        let mut q = QueryBuilder::<Postgres>::new("WITH picked AS MATERIALIZED (SELECT ctid FROM ");
        q.push(spec.table)
            .push(" WHERE ")
            .push(spec.expiry)
            .push("<=")
            .push_bind(request.now.unix_millis())
            .push(spec.guard)
            .push(" ORDER BY ")
            .push(spec.order)
            .push(" LIMIT ")
            .push_bind(request.batch_size as i64)
            .push(" FOR UPDATE SKIP LOCKED),removed AS (DELETE FROM ")
            .push(spec.table)
            .push(" t USING picked s WHERE t.ctid=s.ctid AND ")
            .push(spec.expiry)
            .push("<=")
            .push_bind(request.now.unix_millis())
            .push(spec.guard);
        if let Some((field, kind)) = spec.references {
            q.push(" RETURNING project_id,").push(field).push(" AS owner_id),refs AS (DELETE FROM metric.blob_references r USING removed d WHERE r.project_id=d.project_id AND r.event_id=d.owner_id AND r.source_kind=").push_bind(kind).push(")");
        } else {
            q.push(" RETURNING 1)");
        }
        q.push(" SELECT count(*) FROM removed");
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        let changed: i64 = q
            .build_query_scalar()
            .fetch_one(&mut *tx)
            .await
            .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        let changed = usize::try_from(changed).map_err(invalid)?;
        let next = if changed == request.batch_size {
            Some(phase)
        } else if index + 1 < tables.len() {
            Some(phase + 1)
        } else {
            None
        };
        Ok(report(
            changed,
            changed,
            next.map(|phase| make_cursor(phase, &[])).transpose()?,
        ))
    }

    async fn reconcile_issues(
        &self,
        request: &MaintenanceRequest,
        after: &[u8],
    ) -> Result<MaintenanceResult> {
        let (project, id) = scoped(after)?;
        let maximum = request.batch_size.min(32);
        let rows = sqlx::query("SELECT project_id,issue_id FROM metric.issues WHERE (project_id,issue_id)>($1,$2) ORDER BY project_id,issue_id LIMIT $3")
            .bind(project).bind(id).bind(maximum as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?;
        let mut projects = rows
            .iter()
            .map(|row| row.try_get::<i32, _>("project_id").map_err(invalid))
            .collect::<Result<Vec<_>>>()?;
        projects.sort_unstable();
        projects.dedup();
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        let active = lock_projects(&mut tx, &projects).await?;
        let mut changed = 0;
        let mut last = None;
        for row in &rows {
            let project: i32 = row.try_get("project_id").map_err(invalid)?;
            let issue: Vec<u8> = row.try_get("issue_id").map_err(invalid)?;
            if issue.len() != 16 {
                return Err(MaintenanceStoreError::InvalidData);
            }
            let mut key = project.to_be_bytes().to_vec();
            key.extend_from_slice(&issue);
            last = Some(key);
            if !active.contains(&project) {
                continue;
            }
            // A bounded retained-event count is a lower bound on lifetime count.
            // Never lower an Issue count after retention removes its hot events.
            changed += sqlx::query("UPDATE metric.issues SET occurrence_count=greatest(occurrence_count,$3) WHERE project_id=$1 AND issue_id=$2 AND occurrence_count<$3")
                .bind(project).bind(&issue).bind(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM (SELECT 1 FROM metric.error_events WHERE project_id=$1 AND issue_id=$2 AND processing_state=2 LIMIT $3) retained")
                    .bind(project).bind(&issue).bind(request.batch_size as i64).fetch_one(&mut *tx).await.map_err(unavailable)?)
                .execute(&mut *tx).await.map_err(unavailable)?.rows_affected() as usize;
        }
        tx.commit().await.map_err(unavailable)?;
        page(
            rows.len(),
            changed,
            maximum,
            ISSUES,
            last.as_deref(),
            Some(CATALOGS),
        )
    }

    async fn reconcile_catalogs(
        &self,
        request: &MaintenanceRequest,
        after: &[u8],
    ) -> Result<MaintenanceResult> {
        let after = if after.is_empty() {
            0
        } else {
            project_id(after)?
        };
        let maximum = request.batch_size.min(32);
        let projects: Vec<i32> =
            sqlx::query_scalar("SELECT id FROM metric.projects WHERE id>$1 ORDER BY id LIMIT $2")
                .bind(after)
                .bind(maximum as i64)
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        // Exclusive project guards prevent foreground catalog additions during
        // exact, scoped native counts. Query timeout bounds unusually large catalogs.
        let active: Vec<i32> = sqlx::query_scalar("SELECT id FROM metric.projects WHERE id=ANY($1) AND state IN(0,1) ORDER BY id FOR UPDATE")
            .bind(&projects).fetch_all(&mut *tx).await.map_err(unavailable)?;
        let mut changed = 0;
        for project in active {
            changed += sqlx::query("INSERT INTO metric.project_catalog_usage AS u(project_id,environment_count) VALUES($1,(SELECT count(*) FROM metric.environments WHERE project_id=$1)) ON CONFLICT(project_id) DO UPDATE SET environment_count=EXCLUDED.environment_count WHERE u.environment_count IS DISTINCT FROM EXCLUDED.environment_count")
                .bind(project).execute(&mut *tx).await.map_err(unavailable)?.rows_affected() as usize;
        }
        tx.commit().await.map_err(unavailable)?;
        let key = projects.last().map(|project| project.to_be_bytes());
        page(
            projects.len(),
            changed,
            maximum,
            CATALOGS,
            key.as_ref().map(|value| value.as_slice()),
            None,
        )
    }
}

impl MaintenanceStore for PostgresMaintenanceStore {
    fn run(&self, request: MaintenanceRequest) -> PortFuture<'_, Result<MaintenanceResult>> {
        Box::pin(async move {
            let task = request.task.name();
            let started = Instant::now();
            let result = self.run_inner(request).await;
            let outcome = match &result {
                Ok(value) if value.disposition == MaintenanceDisposition::Disabled => "disabled",
                Ok(_) => "ok",
                Err(MaintenanceStoreError::InvalidData) => "invalid_data",
                Err(MaintenanceStoreError::Unavailable) => "unavailable",
            };
            metrics::histogram!("metric_maintenance_store_duration_seconds", "task" => task, "outcome" => outcome).record(started.elapsed().as_secs_f64());
            result
        })
    }
}

// All identifiers are adapter-owned literals; requests contain only typed values.
struct ExpiryTable {
    table: &'static str,
    expiry: &'static str,
    order: &'static str,
    guard: &'static str,
    references: Option<(&'static str, i16)>,
}
const EVENT_TABLES: &[ExpiryTable] = &[
    ExpiryTable {
        table: "metric.error_events",
        expiry: "expire_at",
        order: "expire_at,project_id,event_id",
        guard: " AND archive_due IS NULL AND processing_state IN(1,2)",
        references: Some(("event_id", 0)),
    },
    ExpiryTable {
        table: "metric.logs",
        expiry: "expire_at",
        order: "expire_at,project_id,log_id",
        guard: " AND archive_due IS NULL",
        references: None,
    },
    ExpiryTable {
        table: "metric.spans",
        expiry: "expire_at",
        order: "expire_at,project_id,record_id",
        guard: " AND archive_due IS NULL",
        references: None,
    },
    ExpiryTable {
        table: "metric.sessions",
        expiry: "expire_at",
        order: "expire_at,project_id,session_id",
        guard: " AND archive_due IS NULL AND state<>0",
        references: None,
    },
    ExpiryTable {
        table: "metric.metric_buckets",
        expiry: "expire_at",
        order: "expire_at,project_id,bucket_id",
        guard: " AND archive_due IS NULL",
        references: None,
    },
    ExpiryTable {
        table: "metric.feedback",
        expiry: "expire_at",
        order: "expire_at,project_id,feedback_id",
        guard: "",
        references: Some(("feedback_id", 1)),
    },
    ExpiryTable {
        table: "metric.replays",
        expiry: "expire_at",
        order: "expire_at,project_id,replay_id",
        guard: " AND archive_due IS NULL",
        references: None,
    },
    ExpiryTable {
        table: "metric.monitor_runs",
        expiry: "delete_at",
        order: "delete_at,project_id,run_id",
        guard: " AND status<>0",
        references: None,
    },
    ExpiryTable {
        table: "metric.notification_deliveries",
        expiry: "delete_at",
        order: "delete_at,project_id,delivery_id",
        guard: " AND status<>0",
        references: None,
    },
    ExpiryTable {
        table: "metric.setup_tokens",
        expiry: "expires_at",
        order: "expires_at,id",
        guard: "",
        references: None,
    },
    ExpiryTable {
        table: "metric.web_sessions",
        expiry: "absolute_expires_at",
        order: "absolute_expires_at,id",
        guard: "",
        references: None,
    },
    ExpiryTable {
        table: "metric.api_tokens",
        expiry: "expires_at",
        order: "expires_at,id",
        guard: "",
        references: None,
    },
    ExpiryTable {
        table: "metric.artifact_uploads",
        expiry: "expire_at",
        order: "expire_at,upload_id",
        guard: " AND state IN(2,3)",
        references: None,
    },
    ExpiryTable {
        table: "metric.artifact_bundles",
        expiry: "due_at",
        order: "due_at,bundle_id",
        guard: " AND state=2",
        references: None,
    },
    ExpiryTable {
        table: "metric.project_deletions",
        expiry: "expire_at",
        order: "expire_at,operation_id",
        guard: " AND phase IN(2,3)",
        references: None,
    },
];
const HOURLY_TABLES: &[ExpiryTable] = &[
    ExpiryTable {
        table: "metric.issue_stats_hourly",
        expiry: "expire_at",
        order: "expire_at,project_id,issue_id,bucket_start",
        guard: "",
        references: None,
    },
    ExpiryTable {
        table: "metric.session_stats_hourly",
        expiry: "expire_at",
        order: "expire_at,project_id,release_id,environment_id,hour",
        guard: "",
        references: None,
    },
    ExpiryTable {
        table: "metric.span_stats_hourly",
        expiry: "expire_at",
        order: "expire_at,project_id,hour,dimension_key",
        guard: "",
        references: None,
    },
];
fn validate(request: &MaintenanceRequest) -> Result<()> {
    if !(1..=10_000).contains(&request.batch_size)
        || request.event_retention.is_zero()
        || request.event_retention > MAX_RETENTION
        || request.hourly_retention.is_zero()
        || request.hourly_retention > MAX_RETENTION
    {
        return Err(MaintenanceStoreError::InvalidData);
    }
    Ok(())
}
fn cursor(request: &MaintenanceRequest, first: u8, last: u8) -> Result<(u8, &[u8])> {
    let Some(value) = &request.cursor else {
        return Ok((first, &[]));
    };
    let bytes = value.as_bytes();
    if !(first..=last).contains(&bytes[0]) {
        return Err(MaintenanceStoreError::InvalidData);
    }
    Ok((bytes[0], &bytes[1..]))
}
fn make_cursor(phase: u8, key: &[u8]) -> Result<MaintenanceCursor> {
    let mut bytes = vec![phase];
    bytes.extend_from_slice(key);
    MaintenanceCursor::new(bytes).ok_or(MaintenanceStoreError::InvalidData)
}
fn scoped(after: &[u8]) -> Result<(i32, Vec<u8>)> {
    if after.is_empty() {
        return Ok((0, vec![]));
    }
    if after.len() != 20 {
        return Err(MaintenanceStoreError::InvalidData);
    }
    Ok((project_id(&after[..4])?, after[4..].to_vec()))
}
fn project_id(bytes: &[u8]) -> Result<i32> {
    let value = i32::from_be_bytes(bytes.try_into().map_err(invalid)?);
    metric_domain::ProjectId::new(value).map_err(invalid)?;
    Ok(value)
}
async fn lock_projects(connection: &mut PgConnection, projects: &[i32]) -> Result<Vec<i32>> {
    sqlx::query_scalar(
        "SELECT id FROM metric.projects WHERE id=ANY($1) AND state IN(0,1) ORDER BY id FOR SHARE",
    )
    .bind(projects)
    .fetch_all(connection)
    .await
    .map_err(unavailable)
}
fn page(
    scanned: usize,
    changed: usize,
    maximum: usize,
    phase: u8,
    last: Option<&[u8]>,
    next: Option<u8>,
) -> Result<MaintenanceResult> {
    let cursor = if scanned == maximum {
        Some(make_cursor(
            phase,
            last.ok_or(MaintenanceStoreError::InvalidData)?,
        )?)
    } else {
        next.map(|phase| make_cursor(phase, &[])).transpose()?
    };
    Ok(report(scanned, changed, cursor))
}
fn report(
    scanned: usize,
    changed: usize,
    next_cursor: Option<MaintenanceCursor>,
) -> MaintenanceResult {
    MaintenanceResult {
        scanned,
        changed,
        next_cursor,
        disposition: MaintenanceDisposition::Completed,
    }
}
fn add(value: i64, duration: Duration) -> Result<i64> {
    let duration = i64::try_from(duration.as_millis()).map_err(invalid)?;
    let value = value
        .checked_add(duration)
        .ok_or(MaintenanceStoreError::InvalidData)?;
    Timestamp::from_unix_millis(value).map_err(invalid)?;
    Ok(value)
}
fn unavailable<T>(_: T) -> MaintenanceStoreError {
    MaintenanceStoreError::Unavailable
}
fn invalid<T>(_: T) -> MaintenanceStoreError {
    MaintenanceStoreError::InvalidData
}

#[cfg(test)]
mod tests;
