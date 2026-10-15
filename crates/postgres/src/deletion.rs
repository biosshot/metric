//! Project fencing, cancellable grace and bounded durable dataset purge.
use crate::{PostgresDatabase, PostgresError};
use metric_domain::{
    DsnKey, OrganizationId, ProjectId, Timestamp,
    deletion::{
        ProjectDeletionChange, ProjectDeletionOperationId, ProjectDeletionPhase,
        ProjectDeletionRequest, ProjectDeletionStatus,
    },
};
use metric_ports::{
    PortFuture, ProjectDeletionStore, ProjectDeletionStoreError, ProjectPurgeRequest,
};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};
use std::time::Duration;

type Result<T> = std::result::Result<T, ProjectDeletionStoreError>;
const MAX_DURATION: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

// Codes and ordering are adapter-local and append-only. Delete dependent rows first.
// All identifiers here are owned by the adapter, never derived from a request.
const OWNED: &[(i32, &str)] = &[
    (10, "metric.blob_references"),
    (11, "metric.error_events"),
    (12, "metric.logs"),
    (13, "metric.spans"),
    (14, "metric.span_stats_hourly"),
    (15, "metric.sessions"),
    (16, "metric.session_stats_hourly"),
    (17, "metric.feedback_attachments"),
    (18, "metric.feedback"),
    (19, "metric.dashboard_configurations"),
    (20, "metric.monitor_runs"),
    (21, "metric.monitors"),
    (22, "metric.metric_buckets"),
    (23, "metric.replay_segments"),
    (24, "metric.replays"),
    (25, "metric.issue_activities"),
    (26, "metric.issue_transitions"),
    (27, "metric.issue_stats_hourly"),
    (28, "metric.issues"),
    (29, "metric.environments"),
    (30, "metric.debug_files"),
    (31, "metric.debug_uploads"),
    (32, "metric.notification_deliveries"),
    (33, "metric.notification_rules"),
    (34, "metric.notification_destinations"),
    (35, "metric.archive_manifests"),
    (36, "metric.dsn_mappings"),
    (37, "metric.project_keys"),
    (38, "metric.reserved_keys"),
    (39, "metric.project_catalog_usage"),
    (40, "metric.session_project_locks"),
    (41, "metric.metric_project_locks"),
    (42, "metric.signal_project_locks"),
    (43, "metric.monitor_project_locks"),
    (44, "metric.notification_project_locks"),
];
const SHARED: &[(i32, &str, &str, &str)] = &[
    (
        45,
        "metric.artifact_uploads",
        "metric.artifact_upload_bindings",
        "upload_id",
    ),
    (
        46,
        "metric.artifact_bundles",
        "metric.artifact_bundle_bindings",
        "bundle_id",
    ),
    (47, "metric.deploys", "metric.deploy_projects", "deploy_id"),
    (
        48,
        "metric.releases",
        "metric.release_projects",
        "release_id",
    ),
];
const FIRST: i32 = 10;
const LAST: i32 = 48;

#[derive(Clone)]
pub struct PostgresProjectDeletionStore {
    database: PostgresDatabase,
    max_keys: usize,
}
impl PostgresProjectDeletionStore {
    pub fn new(
        database: PostgresDatabase,
        max_keys: usize,
    ) -> std::result::Result<Self, PostgresError> {
        if !(1..=1024).contains(&max_keys) {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self { database, max_keys })
    }
    async fn keys(&self, conn: &mut PgConnection, project: ProjectId) -> Result<Vec<DsnKey>> {
        let bytes: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT key FROM metric.project_keys WHERE project_id=$1 ORDER BY key LIMIT $2",
        )
        .bind(project.get())
        .bind((self.max_keys + 1) as i64)
        .fetch_all(conn)
        .await
        .map_err(error)?;
        if bytes.len() > self.max_keys {
            return Err(ProjectDeletionStoreError::InvalidData);
        }
        bytes
            .into_iter()
            .map(|key| {
                Ok(DsnKey::from_bytes(
                    key.try_into()
                        .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
                ))
            })
            .collect()
    }
    async fn request(&self, r: ProjectDeletionRequest) -> Result<ProjectDeletionChange> {
        if r.purge_after < r.requested_at {
            return Err(ProjectDeletionStoreError::InvalidData);
        }
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        let project=sqlx::query("SELECT organization_id,state FROM metric.projects WHERE id=$1 AND organization_id=$2 FOR UPDATE")
            .bind(r.project_id.get()).bind(r.organization_id.get() as i64).fetch_optional(&mut *tx).await.map_err(error)?.ok_or(ProjectDeletionStoreError::NotFound)?;
        // Exact operation retry can observe a committed result, even after a worker progressed.
        if let Some(job) =
            sqlx::query("SELECT * FROM metric.project_deletions WHERE operation_id=$1 FOR UPDATE")
                .bind(r.operation_id.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(error)?
        {
            let status = decode(&job)?;
            if status.project_id != r.project_id || status.organization_id != r.organization_id {
                return Err(ProjectDeletionStoreError::Conflict);
            }
            let affected_keys = self.keys(&mut tx, r.project_id).await?;
            tx.commit().await.map_err(error)?;
            return Ok(ProjectDeletionChange {
                status,
                affected_keys,
            });
        }
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metric.project_deletions WHERE project_id=$1 AND phase IN(0,1))")
            .bind(r.project_id.get()).fetch_one(&mut *tx).await.map_err(error)?;
        if active {
            return Err(ProjectDeletionStoreError::Conflict);
        }
        let previous: i16 = get(&project, "state")?;
        if !matches!(previous, 0 | 1) {
            return Err(ProjectDeletionStoreError::NotFound);
        }
        let affected_keys = self.keys(&mut tx, r.project_id).await?;
        let row=sqlx::query("INSERT INTO metric.project_deletions(operation_id,project_id,organization_id,requested_by,previous_state,requested_at,purge_after,next_attempt_at) VALUES($1,$2,$3,$4,$5,$6,$7,$7) RETURNING *")
            .bind(r.operation_id.as_bytes().as_slice()).bind(r.project_id.get()).bind(r.organization_id.get() as i64).bind(r.requested_by.get() as i64).bind(previous).bind(r.requested_at.unix_millis()).bind(r.purge_after.unix_millis()).fetch_one(&mut *tx).await.map_err(error)?;
        sqlx::query("UPDATE metric.projects SET state=2 WHERE id=$1")
            .bind(r.project_id.get())
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        sqlx::query("UPDATE metric.project_keys SET state=2 WHERE project_id=$1 AND state=0")
            .bind(r.project_id.get())
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        let status = decode(&row)?;
        tx.commit().await.map_err(error)?;
        Ok(ProjectDeletionChange {
            status,
            affected_keys,
        })
    }
    async fn cancel(
        &self,
        project: ProjectId,
        operation: ProjectDeletionOperationId,
        now: Timestamp,
        retention: Duration,
    ) -> Result<ProjectDeletionChange> {
        let expires = after(now, retention)?;
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        let exists: Option<i32> =
            sqlx::query_scalar("SELECT id FROM metric.projects WHERE id=$1 FOR UPDATE")
                .bind(project.get())
                .fetch_optional(&mut *tx)
                .await
                .map_err(error)?;
        if exists.is_none() {
            return Err(ProjectDeletionStoreError::NotFound);
        }
        let row=sqlx::query("SELECT * FROM metric.project_deletions WHERE operation_id=$1 AND project_id=$2 FOR UPDATE")
            .bind(operation.as_bytes().as_slice()).bind(project.get()).fetch_optional(&mut *tx).await.map_err(error)?.ok_or(ProjectDeletionStoreError::NotFound)?;
        if get::<i16>(&row, "phase")? != 0 {
            return Err(ProjectDeletionStoreError::NotCancellable);
        }
        let affected_keys = self.keys(&mut tx, project).await?;
        sqlx::query("UPDATE metric.projects SET state=$2 WHERE id=$1 AND state=2")
            .bind(project.get())
            .bind(get::<i16>(&row, "previous_state")?)
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        sqlx::query("UPDATE metric.project_keys SET state=0 WHERE project_id=$1 AND state=2")
            .bind(project.get())
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        let row=sqlx::query("UPDATE metric.project_deletions SET phase=3,completed_at=$2,expire_at=$3,next_attempt_at=$3 WHERE operation_id=$1 RETURNING *")
            .bind(operation.as_bytes().as_slice()).bind(now.unix_millis()).bind(expires).fetch_one(&mut *tx).await.map_err(error)?;
        let status = decode(&row)?;
        tx.commit().await.map_err(error)?;
        Ok(ProjectDeletionChange {
            status,
            affected_keys,
        })
    }
    async fn status(&self, project: ProjectId) -> Result<ProjectDeletionStatus> {
        let row=sqlx::query("SELECT * FROM metric.project_deletions WHERE project_id=$1 ORDER BY requested_at DESC,operation_id DESC LIMIT 1")
            .bind(project.get()).fetch_optional(&self.database.pool).await.map_err(error)?.ok_or(ProjectDeletionStoreError::NotFound)?;
        decode(&row)
    }
    async fn release_slug(&self, now: Timestamp) -> Result<()> {
        // Reservation release survives expiry of the job: the tombstone owns its deadline.
        sqlx::query("WITH due AS (SELECT id FROM metric.projects WHERE state=4 AND slug IS NOT NULL AND slug_reserved_until<=$1 ORDER BY slug_reserved_until,id LIMIT 1 FOR UPDATE SKIP LOCKED) UPDATE metric.projects p SET slug=NULL FROM due WHERE p.id=due.id")
            .bind(now.unix_millis()).execute(&self.database.pool).await.map_err(error)?;
        Ok(())
    }
    async fn purge(&self, r: ProjectPurgeRequest) -> Result<Option<ProjectDeletionStatus>> {
        validate(r)?;
        self.release_slug(r.now).await?;
        // Read candidates without a job lock: every mutation locks project before job.
        let due=sqlx::query("SELECT operation_id,project_id FROM metric.project_deletions WHERE phase IN(0,1) AND next_attempt_at<=$1 ORDER BY next_attempt_at,operation_id LIMIT 1")
            .bind(r.now.unix_millis()).fetch_optional(&self.database.pool).await.map_err(error)?;
        let Some(due) = due else { return Ok(None) };
        let project = ProjectId::new(get(&due, "project_id")?)
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?;
        let operation: Vec<u8> = get(&due, "operation_id")?;
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        let p=sqlx::query("SELECT id,state,organization_id FROM metric.projects WHERE id=$1 FOR UPDATE SKIP LOCKED")
            .bind(project.get()).fetch_optional(&mut *tx).await.map_err(error)?;
        let Some(p) = p else { return Ok(None) };
        let job=sqlx::query("SELECT * FROM metric.project_deletions WHERE operation_id=$1 AND phase IN(0,1) AND next_attempt_at<=$2 FOR UPDATE")
            .bind(&operation).bind(r.now.unix_millis()).fetch_optional(&mut *tx).await.map_err(error)?;
        let Some(job) = job else { return Ok(None) };
        if !matches!(get::<i16>(&p, "state")?, 2 | 3) {
            return Err(ProjectDeletionStoreError::InvalidData);
        }
        if get::<i16>(&job, "phase")? == 0 {
            sqlx::query("UPDATE metric.projects SET state=3 WHERE id=$1")
                .bind(project.get())
                .execute(&mut *tx)
                .await
                .map_err(error)?;
            sqlx::query("UPDATE metric.project_deletions SET phase=1 WHERE operation_id=$1")
                .bind(&operation)
                .execute(&mut *tx)
                .await
                .map_err(error)?;
        }
        let result = self.step(&mut tx, &job, project, r).await;
        if let Err(failure) = result {
            let _ = tx.rollback().await;
            self.record_failure(project, &operation, r).await?;
            return Err(failure);
        }
        let row = sqlx::query("SELECT * FROM metric.project_deletions WHERE operation_id=$1")
            .bind(&operation)
            .fetch_one(&mut *tx)
            .await
            .map_err(error)?;
        let status = decode(&row)?;
        tx.commit().await.map_err(error)?;
        Ok(Some(status))
    }
    async fn step(
        &self,
        conn: &mut PgConnection,
        job: &PgRow,
        project: ProjectId,
        r: ProjectPurgeRequest,
    ) -> Result<()> {
        let code: i32 = get(job, "dataset_code")?;
        let operation: Vec<u8> = get(job, "operation_id")?;
        let org: i64 = get(job, "organization_id")?;
        let count = if let Some((_, table)) = OWNED.iter().find(|(c, _)| *c == code) {
            let mut q = QueryBuilder::<Postgres>::new("WITH batch AS (SELECT ctid FROM ");
            q.push(*table)
                .push(" WHERE project_id=")
                .push_bind(project.get())
                .push(" LIMIT ")
                .push_bind(r.batch_size as i64)
                .push(" FOR UPDATE) DELETE FROM ")
                .push(*table)
                .push(" t USING batch WHERE t.ctid=batch.ctid");
            q.build()
                .execute(&mut *conn)
                .await
                .map_err(error)?
                .rows_affected()
        } else if let Some((_, parent, bindings, id)) =
            SHARED.iter().find(|(c, _, _, _)| *c == code)
        {
            // Serialize organization-shared associations with publication and GC.
            sqlx::query("SELECT id FROM metric.organizations WHERE id=$1 FOR UPDATE")
                .bind(org)
                .fetch_one(&mut *conn)
                .await
                .map_err(error)?;
            let mut q = QueryBuilder::<Postgres>::new("SELECT DISTINCT ");
            q.push(*id)
                .push(" FROM ")
                .push(*bindings)
                .push(" WHERE organization_id=")
                .push_bind(org)
                .push(" AND project_id=")
                .push_bind(project.get())
                .push(" ORDER BY ")
                .push(*id)
                .push(" LIMIT ")
                .push_bind(r.batch_size as i64);
            let rows = q.build().fetch_all(&mut *conn).await.map_err(error)?;
            for row in &rows {
                let key: Vec<u8> = get(row, id)?;
                let mut remove = QueryBuilder::<Postgres>::new("DELETE FROM ");
                remove
                    .push(*bindings)
                    .push(" WHERE organization_id=")
                    .push_bind(org)
                    .push(" AND project_id=")
                    .push_bind(project.get())
                    .push(" AND ")
                    .push(*id)
                    .push("=")
                    .push_bind(key.clone());
                remove.build().execute(&mut *conn).await.map_err(error)?;
                if code == 46 {
                    sqlx::query("UPDATE metric.artifact_bundles b SET due_at=$3,deletion_operation=$4 WHERE organization_id=$1 AND bundle_id=$2 AND state=0 AND NOT EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.organization_id=b.organization_id AND r.bundle_id=b.bundle_id)")
                        .bind(org).bind(&key).bind(r.now.unix_millis()).bind(&operation).execute(&mut *conn).await.map_err(error)?;
                } else {
                    let mut remove = QueryBuilder::<Postgres>::new("DELETE FROM ");
                    remove
                        .push(*parent)
                        .push(" p WHERE organization_id=")
                        .push_bind(org)
                        .push(" AND ")
                        .push(*id)
                        .push("=")
                        .push_bind(key)
                        .push(" AND NOT EXISTS(SELECT 1 FROM ")
                        .push(*bindings)
                        .push(" r WHERE r.organization_id=p.organization_id AND r.")
                        .push(*id)
                        .push("=p.")
                        .push(*id)
                        .push(")");
                    remove.build().execute(&mut *conn).await.map_err(error)?;
                }
            }
            rows.len() as u64
        } else {
            return Err(ProjectDeletionStoreError::InvalidData);
        };
        if code == LAST && count < (r.batch_size as u64) {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM metric.artifact_bundles WHERE deletion_operation=$1)",
            )
            .bind(&operation)
            .fetch_one(&mut *conn)
            .await
            .map_err(error)?;
            if !waiting {
                if !get::<bool>(job, "reconciliation_pass")? {
                    sqlx::query("UPDATE metric.project_deletions SET dataset_code=$2,reconciliation_pass=true,next_attempt_at=$3,attempts=0,last_error=NULL WHERE operation_id=$1")
                        .bind(&operation).bind(FIRST).bind(r.now.unix_millis()).execute(&mut *conn).await.map_err(error)?;
                    return Ok(());
                }
                sqlx::query("UPDATE metric.projects SET state=4,display_name='Deleted',items=0,inbound_filters='[]',max_events_per_second=NULL,burst=NULL,debug_file_revision=0,artifact_revision=0,debug_file_bytes=0,debug_file_count=0,deleted_at=$2,deletion_operation=$3,slug_reserved_until=$4 WHERE id=$1")
                    .bind(project.get()).bind(r.now.unix_millis()).bind(&operation).bind(after(r.now,r.slug_reservation)?).execute(&mut *conn).await.map_err(error)?;
                sqlx::query("UPDATE metric.project_deletions SET phase=2,completed_at=$2,expire_at=$3,next_attempt_at=$4,attempts=0,last_error=NULL WHERE operation_id=$1")
                    .bind(&operation).bind(r.now.unix_millis()).bind(after(r.now,r.completed_retention)?).bind(after(r.now,r.slug_reservation)?).execute(&mut *conn).await.map_err(error)?;
                return Ok(());
            }
        }
        let next = if count < (r.batch_size as u64) && code < LAST {
            code + 1
        } else {
            code
        };
        sqlx::query("UPDATE metric.project_deletions SET dataset_code=$2,next_attempt_at=$3,attempts=0,last_error=NULL WHERE operation_id=$1")
            .bind(operation).bind(next).bind(r.now.unix_millis()).execute(conn).await.map_err(error)?;
        Ok(())
    }
    async fn record_failure(
        &self,
        project: ProjectId,
        operation: &[u8],
        r: ProjectPurgeRequest,
    ) -> Result<()> {
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        sqlx::query("SELECT id FROM metric.projects WHERE id=$1 FOR UPDATE")
            .bind(project.get())
            .fetch_one(&mut *tx)
            .await
            .map_err(error)?;
        let row=sqlx::query("SELECT attempts FROM metric.project_deletions WHERE operation_id=$1 AND phase IN(0,1) FOR UPDATE")
            .bind(operation).fetch_optional(&mut *tx).await.map_err(error)?;
        if let Some(row) = row {
            let attempts = get::<i64>(&row, "attempts")?
                .saturating_add(1)
                .min(i64::from(u32::MAX));
            let delay = r
                .retry_base
                .saturating_mul(
                    1_u32
                        .checked_shl((attempts - 1).min(31) as u32)
                        .unwrap_or(u32::MAX),
                )
                .min(r.retry_max);
            sqlx::query("UPDATE metric.project_deletions SET attempts=$2,last_error='dataset_batch_failed',next_attempt_at=$3 WHERE operation_id=$1")
                .bind(operation).bind(attempts).bind(after(r.now,delay)?).execute(&mut *tx).await.map_err(error)?;
        }
        tx.commit().await.map_err(error)
    }
}
impl ProjectDeletionStore for PostgresProjectDeletionStore {
    fn request_deletion(
        &self,
        r: ProjectDeletionRequest,
    ) -> PortFuture<'_, Result<ProjectDeletionChange>> {
        Box::pin(self.request(r))
    }
    fn cancel_deletion(
        &self,
        p: ProjectId,
        o: ProjectDeletionOperationId,
        n: Timestamp,
        d: Duration,
    ) -> PortFuture<'_, Result<ProjectDeletionChange>> {
        Box::pin(self.cancel(p, o, n, d))
    }
    fn deletion_status(&self, p: ProjectId) -> PortFuture<'_, Result<ProjectDeletionStatus>> {
        Box::pin(self.status(p))
    }
    fn purge_next(
        &self,
        r: ProjectPurgeRequest,
    ) -> PortFuture<'_, Result<Option<ProjectDeletionStatus>>> {
        Box::pin(self.purge(r))
    }
}
fn decode(row: &PgRow) -> Result<ProjectDeletionStatus> {
    if get::<i16>(row, "plan_version")? != 1 {
        return Err(ProjectDeletionStoreError::InvalidData);
    }
    let code = get::<i32>(row, "dataset_code")?;
    if !(FIRST..=LAST).contains(&code) {
        return Err(ProjectDeletionStoreError::InvalidData);
    }
    Ok(ProjectDeletionStatus {
        operation_id: ProjectDeletionOperationId::from_bytes(
            get::<Vec<u8>>(row, "operation_id")?
                .try_into()
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        ),
        project_id: ProjectId::new(get(row, "project_id")?)
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        organization_id: OrganizationId::new(
            u64::try_from(get::<i64>(row, "organization_id")?)
                .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        phase: match get::<i16>(row, "phase")? {
            0 => ProjectDeletionPhase::PendingGrace,
            1 => ProjectDeletionPhase::Purging,
            2 => ProjectDeletionPhase::Deleted,
            3 => ProjectDeletionPhase::Cancelled,
            _ => return Err(ProjectDeletionStoreError::InvalidData),
        },
        dataset_code: code as u16,
        reconciliation_pass: get(row, "reconciliation_pass")?,
        requested_at: stamp(get(row, "requested_at")?)?,
        purge_after: stamp(get(row, "purge_after")?)?,
        completed_at: get::<Option<i64>>(row, "completed_at")?
            .map(stamp)
            .transpose()?,
        next_attempt_at: stamp(get(row, "next_attempt_at")?)?,
        attempts: u32::try_from(get::<i64>(row, "attempts")?)
            .map_err(|_| ProjectDeletionStoreError::InvalidData)?,
        last_error: get(row, "last_error")?,
    })
}
fn get<'r, T: sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres>>(
    row: &'r PgRow,
    key: &str,
) -> Result<T> {
    row.try_get(key)
        .map_err(|_| ProjectDeletionStoreError::InvalidData)
}
fn stamp(v: i64) -> Result<Timestamp> {
    Timestamp::from_unix_millis(v).map_err(|_| ProjectDeletionStoreError::InvalidData)
}
fn after(t: Timestamp, d: Duration) -> Result<i64> {
    if d.is_zero() || d > MAX_DURATION {
        return Err(ProjectDeletionStoreError::InvalidData);
    }
    let millis =
        i64::try_from(d.as_millis()).map_err(|_| ProjectDeletionStoreError::InvalidData)?;
    let value = t
        .unix_millis()
        .checked_add(millis)
        .ok_or(ProjectDeletionStoreError::InvalidData)?;
    stamp(value).map(|t| t.unix_millis())
}
fn validate(r: ProjectPurgeRequest) -> Result<()> {
    if !(1..=10_000).contains(&r.batch_size) || r.retry_base > r.retry_max {
        return Err(ProjectDeletionStoreError::InvalidData);
    }
    for d in [
        r.retry_base,
        r.retry_max,
        r.completed_retention,
        r.slug_reservation,
    ] {
        after(r.now, d)?;
    }
    Ok(())
}
fn error(e: sqlx::Error) -> ProjectDeletionStoreError {
    match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            ProjectDeletionStoreError::Conflict
        }
        sqlx::Error::Database(db)
            if matches!(db.code().as_deref(), Some("23514" | "23502" | "22003")) =>
        {
            ProjectDeletionStoreError::InvalidData
        }
        _ => ProjectDeletionStoreError::Unavailable,
    }
}
#[cfg(test)]
mod tests;
