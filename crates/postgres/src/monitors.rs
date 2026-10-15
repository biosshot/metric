//! Scoped transactional Cron/Uptime definitions, run merges, deadlines and claims.
use crate::{PostgresDatabase, PostgresError, signal_storage};
use metric_domain::{
    ProjectId, Timestamp,
    finalization::{EnvironmentId, ReleaseId},
    monitors::{
        MonitorAnchor, MonitorDefinition, MonitorId, MonitorPage, MonitorRun, MonitorRunAnchor,
        MonitorRunId, MonitorRunPage, MonitorRunSource, MonitorRunStatus, MonitorUpdate,
        UptimeFailure,
    },
};
use metric_ports::{DurableOutcome, MonitorStore, PortFuture, SignalStoreError};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};
use std::collections::{BTreeMap, BTreeSet};
mod codec;
use codec::{decode_monitor, decode_run, encode_config, failure, status};
type Result<T> = std::result::Result<T, SignalStoreError>;
const MAX_CONFIG: usize = 64 * 1024;
const MAX_BATCH: usize = 10_000;
const MAX_BATCH_BYTES: i64 = 16 * 1024 * 1024;
const READ_BYTES: i64 = 8 * 1024 * 1024;
const CHUNK: usize = 1000;
const RUN_SELECT: &str = "SELECT * FROM metric.monitor_runs";

#[derive(Debug, Clone, Copy)]
pub struct MonitorRetention {
    pub runs_days: u32,
}
impl Default for MonitorRetention {
    fn default() -> Self {
        Self { runs_days: 90 }
    }
}
#[derive(Clone)]
pub struct PostgresMonitorStore {
    database: PostgresDatabase,
    retention: MonitorRetention,
}
#[derive(Clone)]
struct StoredMonitor {
    definition: MonitorDefinition,
    lease: Option<Timestamp>,
}
#[derive(Clone, Copy)]
enum Work {
    Timeout,
    Missed,
    Uptime(Timestamp),
}
impl PostgresMonitorStore {
    pub fn new(
        database: PostgresDatabase,
        retention: MonitorRetention,
    ) -> std::result::Result<Self, PostgresError> {
        if !(1..=3650).contains(&retention.runs_days) {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self {
            database,
            retention,
        })
    }
    async fn persist(&self, updates: Vec<MonitorUpdate>) -> Result<Vec<DurableOutcome>> {
        if updates.is_empty() {
            return Ok(vec![]);
        }
        if updates.len() > MAX_BATCH {
            return Err(SignalStoreError::InvalidData);
        }
        let mut bytes = updates.len() * 512;
        for update in &updates {
            update.validate().map_err(invalid)?;
            if let Some(definition) = &update.definition {
                bytes = bytes
                    .checked_add(encode_config(definition)?.1)
                    .ok_or(SignalStoreError::Capacity)?;
            }
        }
        if bytes > MAX_BATCH_BYTES as usize {
            return Err(SignalStoreError::Capacity);
        }
        let projects: Vec<_> = updates
            .iter()
            .map(|v| v.run.project_id.get())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let ids: Vec<_> = updates
            .iter()
            .map(|v| v.run.monitor_id.as_bytes().to_vec())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let run_ids: Vec<_> = updates
            .iter()
            .map(|v| v.run.id.as_bytes().to_vec())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        signal_storage::lock_projects(&mut tx, &projects).await?;
        lock_lane(&mut tx, &projects).await?;
        let mut monitors = load_monitors(&mut tx, &ids, false).await?;
        let mut changed_monitors = BTreeSet::new();
        // Resolve all definitions before runs, retaining web-owned configuration.
        for update in &updates {
            if let Some(incoming) = &update.definition {
                let existing = monitors.get(&incoming.id);
                if existing.is_some_and(|v| v.definition.project_id != incoming.project_id) {
                    return Err(SignalStoreError::Conflict);
                }
                if existing.is_some_and(|v| v.definition.managed_by_web) {
                    continue;
                }
                let mut definition = incoming.clone();
                let lease = existing.and_then(|v| v.lease);
                if let Some(existing) = existing {
                    definition.created_at = existing.definition.created_at;
                    definition.revision = existing
                        .definition
                        .revision
                        .checked_add(1)
                        .ok_or(SignalStoreError::InvalidData)?;
                    definition.last_run_id = existing.definition.last_run_id;
                    definition.last_status = existing.definition.last_status;
                    definition.last_check_in_at = existing.definition.last_check_in_at;
                }
                encode_config(&definition)?;
                monitors.insert(definition.id, StoredMonitor { definition, lease });
                changed_monitors.insert(incoming.id);
            }
        }
        let mut runs = load_runs(&mut tx, &run_ids).await?;
        let mut changed_runs = BTreeMap::new();
        let mut projections = BTreeMap::<MonitorId, MonitorRun>::new();
        let mut outcomes = Vec::with_capacity(updates.len());
        for update in updates {
            let mut incoming = update.run;
            let monitor = monitors
                .get(&incoming.monitor_id)
                .ok_or(SignalStoreError::NotFound)?;
            if monitor.definition.project_id != incoming.project_id {
                return Err(SignalStoreError::Conflict);
            }
            if incoming.status == MonitorRunStatus::InProgress && incoming.timeout_at.is_none() {
                incoming.timeout_at = Some(
                    monitor
                        .definition
                        .config
                        .timeout_at(incoming.started_at)
                        .map_err(invalid)?,
                );
            }
            let merged = merge(runs.get(&incoming.id), &incoming)?;
            if let Some(run) = merged {
                if projections
                    .get(&run.monitor_id)
                    .is_none_or(|v| v.received_at <= run.received_at)
                {
                    projections.insert(run.monitor_id, run.clone());
                }
                runs.insert(run.id, run.clone());
                changed_runs.insert(run.id, run);
                outcomes.push(DurableOutcome::Accepted);
            } else {
                outcomes.push(DurableOutcome::Duplicate);
            }
        }
        for (id, run) in projections {
            let monitor = monitors.get_mut(&id).ok_or(SignalStoreError::NotFound)?;
            if project_run(monitor, &run)? {
                changed_monitors.insert(id);
            }
        }
        let changed: Vec<_> = changed_monitors
            .iter()
            .map(|id| monitors.get(id).unwrap())
            .collect();
        write_monitors(&mut tx, &changed).await?;
        write_runs(&mut tx, &changed_runs.values().collect::<Vec<_>>()).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(outcomes)
    }
    async fn upsert(&self, definition: MonitorDefinition) -> Result<MonitorDefinition> {
        encode_config(&definition)?;
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        let p = [definition.project_id.get()];
        signal_storage::lock_projects(&mut tx, &p).await?;
        lock_lane(&mut tx, &p).await?;
        let previous = load_monitors(&mut tx, &[definition.id.as_bytes().to_vec()], false).await?;
        let previous = previous.get(&definition.id);
        if previous.is_some_and(|v| v.definition.project_id != definition.project_id) {
            return Err(SignalStoreError::Conflict);
        }
        let value = StoredMonitor {
            definition: definition.clone(),
            lease: previous.and_then(|v| v.lease),
        };
        write_monitors(&mut tx, &[&value]).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(definition)
    }
    async fn work(
        &self,
        now: Timestamp,
        limit: usize,
        work: Work,
    ) -> Result<(u64, Vec<MonitorDefinition>)> {
        let candidates=match work {
            Work::Timeout=>sqlx::query("SELECT r.run_id AS id,r.project_id FROM metric.monitor_runs r JOIN metric.projects p ON p.id=r.project_id WHERE r.status=0 AND r.timeout_at<=$1 AND p.state IN(0,1) ORDER BY r.timeout_at,r.run_id LIMIT $2").bind(now.unix_millis()).bind(limit as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?,
            Work::Missed=>sqlx::query("SELECT m.monitor_id AS id,m.project_id FROM metric.monitors m JOIN metric.projects p ON p.id=m.project_id WHERE m.kind=0 AND m.enabled AND m.due_at<=$1 AND p.state IN(0,1) ORDER BY m.due_at,m.monitor_id LIMIT $2").bind(now.unix_millis()).bind(limit as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?,
            Work::Uptime(_)=>sqlx::query("SELECT m.monitor_id AS id,m.project_id FROM metric.monitors m JOIN metric.projects p ON p.id=m.project_id WHERE m.kind=1 AND m.enabled AND m.due_at<=$1 AND (m.lease_until IS NULL OR m.lease_until<=$1) AND p.state IN(0,1) ORDER BY m.due_at,m.monitor_id LIMIT $2").bind(now.unix_millis()).bind(limit as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?,
        };
        if candidates.is_empty() {
            return Ok((0, vec![]));
        }
        let requested: Vec<_> = candidates
            .iter()
            .map(|r| r.get::<i32, _>("project_id"))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        // Workers recheck project fences and deadlines after acquiring the same
        // project-before-lane lock order as foreground admission and deletion.
        let allowed:Vec<i32>=sqlx::query_scalar("SELECT id FROM metric.projects WHERE id=ANY($1) AND state IN(0,1) ORDER BY id FOR SHARE")
            .bind(&requested).fetch_all(&mut *tx).await.map_err(unavailable)?;
        lock_lane(&mut tx, &allowed).await?;
        let ids: Vec<Vec<u8>> = candidates
            .iter()
            .filter(|r| allowed.binary_search(&r.get("project_id")).is_ok())
            .map(|r| r.get("id"))
            .collect();
        let mut selected_runs = if matches!(work, Work::Timeout) {
            load_runs(&mut tx, &ids).await?
        } else {
            BTreeMap::new()
        };
        let monitor_ids = if matches!(work, Work::Timeout) {
            selected_runs
                .values()
                .map(|r| r.monitor_id.as_bytes().to_vec())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            ids
        };
        let monitors = load_monitors(&mut tx, &monitor_ids, true).await?;
        // Load existing deterministic missed slots once for the whole batch.
        let missed_runs = if matches!(work, Work::Missed) {
            let ids = monitors
                .values()
                .filter(|m| !m.definition.is_uptime())
                .map(|m| {
                    MonitorRunId::missed(m.definition.id, m.definition.next_expected_at)
                        .as_bytes()
                        .to_vec()
                })
                .collect::<Vec<_>>();
            load_runs(&mut tx, &ids).await?
        } else {
            BTreeMap::new()
        };
        let mut changed = BTreeMap::new();
        let mut changed_runs = BTreeMap::new();
        let mut claimed = vec![];
        let mut count = 0;
        match work {
            Work::Timeout => {
                for run in selected_runs.values_mut() {
                    if !allowed.contains(&run.project_id.get())
                        || run.status != MonitorRunStatus::InProgress
                        || run.timeout_at.is_none_or(|v| v > now)
                    {
                        continue;
                    }
                    let Some(monitor) = monitors.get(&run.monitor_id) else {
                        continue;
                    };
                    run.status = MonitorRunStatus::Timeout;
                    run.finished_at = Some(now);
                    run.received_at = now;
                    run.duration_ms = Some(
                        u64::try_from(
                            now.unix_millis()
                                .checked_sub(run.started_at.unix_millis())
                                .ok_or(SignalStoreError::InvalidData)?,
                        )
                        .map_err(invalid)?,
                    );
                    run.validate().map_err(invalid)?;
                    let current = changed
                        .entry(run.monitor_id)
                        .or_insert_with(|| monitor.clone());
                    project_run(current, run)?;
                    changed_runs.insert(run.id, run.clone());
                    count += 1;
                }
            }
            Work::Missed | Work::Uptime(_) => {
                // Preserve native due/id order after bounded decoding.
                let mut ordered: Vec<_> = monitors.values().collect();
                ordered.sort_by_key(|m| (due(&m.definition).ok(), m.definition.id));
                for old in ordered {
                    let definition = &old.definition;
                    if !allowed.contains(&definition.project_id.get())
                        || !definition.enabled
                        || due(definition)? > now
                    {
                        continue;
                    }
                    let mut current = old.clone();
                    match work {
                        Work::Uptime(lease) => {
                            if !definition.is_uptime() || old.lease.is_some_and(|v| v > now) {
                                continue;
                            }
                            current.lease = Some(lease);
                            claimed.push(definition.clone());
                        }
                        Work::Missed => {
                            if definition.is_uptime() {
                                continue;
                            }
                            let run = MonitorRun {
                                id: MonitorRunId::missed(
                                    definition.id,
                                    definition.next_expected_at,
                                ),
                                project_id: definition.project_id,
                                monitor_id: definition.id,
                                check_in_id: None,
                                status: MonitorRunStatus::Missed,
                                source: MonitorRunSource::Scheduler,
                                scheduled_for: Some(definition.next_expected_at),
                                started_at: definition.next_expected_at,
                                finished_at: Some(now),
                                duration_ms: None,
                                received_at: now,
                                release_id: None,
                                timeout_at: None,
                                delete_at: Some(add_days(now, self.retention.runs_days)?),
                                http_status: None,
                                uptime_failure: None,
                            };
                            if let Some(merged) = merge(missed_runs.get(&run.id), &run)? {
                                changed_runs.insert(run.id, merged);
                            }
                            project_run(&mut current, &run)?;
                        }
                        Work::Timeout => unreachable!(),
                    }
                    changed.insert(definition.id, current);
                    count += 1;
                }
            }
        }
        write_monitors(&mut tx, &changed.values().collect::<Vec<_>>()).await?;
        write_runs(&mut tx, &changed_runs.values().collect::<Vec<_>>()).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok((count, claimed))
    }
}
impl MonitorStore for PostgresMonitorStore {
    fn persist_monitors(
        &self,
        updates: Vec<MonitorUpdate>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>>> {
        Box::pin(self.persist(updates))
    }
    fn upsert_monitor(
        &self,
        monitor: MonitorDefinition,
    ) -> PortFuture<'_, Result<MonitorDefinition>> {
        Box::pin(self.upsert(monitor))
    }
    fn load_monitor(
        &self,
        project: ProjectId,
        id: MonitorId,
    ) -> PortFuture<'_, Result<MonitorDefinition>> {
        Box::pin(async move {
            let mut q = monitor_select();
            q.push("project_id=")
                .push_bind(project.get())
                .push(" AND monitor_id=")
                .push_bind(id.as_bytes().to_vec());
            let rows = finish_select(&mut q, "monitor_id", 1, READ_BYTES)
                .build()
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
            rows.first()
                .ok_or(SignalStoreError::NotFound)
                .and_then(decode_monitor)
                .map(|v| v.definition)
        })
    }
    fn list_monitors(
        &self,
        project: ProjectId,
        before: Option<MonitorAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<MonitorPage>> {
        Box::pin(async move {
            page_limit(limit)?;
            let maximum = limit.min(MAX_BATCH);
            let mut q = monitor_select();
            q.push("project_id=").push_bind(project.get());
            if let Some(before) = before {
                q.push(" AND (updated_at,monitor_id)<(")
                    .push_bind(before.updated_at.unix_millis())
                    .push(",")
                    .push_bind(before.monitor_id.as_bytes().to_vec())
                    .push(")");
            }
            let rows = finish_select(
                &mut q,
                "updated_at DESC,monitor_id DESC",
                maximum + 1,
                READ_BYTES,
            )
            .build()
            .fetch_all(&self.database.pool)
            .await
            .map_err(unavailable)?;
            let mut items: Vec<MonitorDefinition> = vec![];
            let mut next = None;
            for row in rows {
                if items.len() == maximum
                    || row.try_get::<i64, _>("response_bytes").map_err(invalid)? > READ_BYTES
                {
                    next = items.last().map(|v| MonitorAnchor {
                        updated_at: v.updated_at,
                        monitor_id: v.id,
                    });
                    break;
                }
                items.push(decode_monitor(&row)?.definition);
            }
            Ok(MonitorPage { items, next })
        })
    }
    fn list_monitor_runs(
        &self,
        project: ProjectId,
        monitor: MonitorId,
        from: Option<Timestamp>,
        until: Option<Timestamp>,
        before: Option<MonitorRunAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<MonitorRunPage>> {
        Box::pin(async move {
            page_limit(limit)?;
            let maximum = limit.min(READ_BYTES as usize / 512);
            let mut q = QueryBuilder::<Postgres>::new(RUN_SELECT);
            q.push(" WHERE project_id=")
                .push_bind(project.get())
                .push(" AND monitor_id=")
                .push_bind(monitor.as_bytes().to_vec());
            if let Some(from) = from {
                q.push(" AND started_at>=").push_bind(from.unix_millis());
            }
            if let Some(until) = until {
                q.push(" AND started_at<=").push_bind(until.unix_millis());
            }
            if let Some(before) = before {
                q.push(" AND (started_at,run_id)<(")
                    .push_bind(before.started_at.unix_millis())
                    .push(",")
                    .push_bind(before.run_id.as_bytes().to_vec())
                    .push(")");
            }
            q.push(" ORDER BY started_at DESC,run_id DESC LIMIT ")
                .push_bind((maximum + 1) as i64);
            let rows = q
                .build()
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
            let mut items = rows.iter().map(decode_run).collect::<Result<Vec<_>>>()?;
            let next = if items.len() > maximum {
                items.pop();
                items.last().map(|r| MonitorRunAnchor {
                    started_at: r.started_at,
                    run_id: r.id,
                })
            } else {
                None
            };
            Ok(MonitorRunPage { items, next })
        })
    }
    fn delete_monitor(&self, project: ProjectId, id: MonitorId) -> PortFuture<'_, Result<()>> {
        Box::pin(async move {
            let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
            let p = [project.get()];
            signal_storage::lock_projects(&mut tx, &p).await?;
            lock_lane(&mut tx, &p).await?;
            let deleted =
                sqlx::query("DELETE FROM metric.monitors WHERE project_id=$1 AND monitor_id=$2")
                    .bind(project.get())
                    .bind(id.as_bytes().as_slice())
                    .execute(&mut *tx)
                    .await
                    .map_err(write_error)?
                    .rows_affected();
            if deleted != 1 {
                return Err(SignalStoreError::NotFound);
            }
            tx.commit().await.map_err(unavailable)?;
            Ok(())
        })
    }
    fn terminalize_due_timeouts(
        &self,
        now: Timestamp,
        batch_size: usize,
    ) -> PortFuture<'_, Result<u64>> {
        Box::pin(async move {
            worker_limit(batch_size, MAX_BATCH)?;
            Ok(self.work(now, batch_size, Work::Timeout).await?.0)
        })
    }
    fn materialize_due_missed(
        &self,
        now: Timestamp,
        batch_size: usize,
    ) -> PortFuture<'_, Result<u64>> {
        Box::pin(async move {
            worker_limit(batch_size, MAX_BATCH)?;
            Ok(self.work(now, batch_size, Work::Missed).await?.0)
        })
    }
    fn claim_due_uptime(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<MonitorDefinition>>> {
        Box::pin(async move {
            worker_limit(limit, 1000)?;
            if lease_until <= now {
                return Err(SignalStoreError::InvalidData);
            }
            Ok(self.work(now, limit, Work::Uptime(lease_until)).await?.1)
        })
    }
    fn pending_monitor_alerts(&self, limit: usize) -> PortFuture<'_, Result<Vec<MonitorRun>>> {
        Box::pin(async move {
            worker_limit(limit, 1000)?;
            let rows=sqlx::query("SELECT r.* FROM metric.monitor_runs r WHERE r.evaluated_at IS NULL AND (r.status IN(2,3,4) OR (r.status=1 AND r.http_status IS NOT NULL)) AND EXISTS(SELECT 1 FROM metric.projects p WHERE p.id=r.project_id AND p.state IN(0,1)) ORDER BY r.received_at,r.run_id LIMIT $1")
            .bind(limit as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?;
            rows.iter().map(decode_run).collect()
        })
    }
    fn mark_monitor_alert_evaluated(
        &self,
        run: MonitorRunId,
        evaluated_at: Timestamp,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(async move {
            sqlx::query("UPDATE metric.monitor_runs r SET evaluated_at=$2 WHERE r.run_id=$1 AND EXISTS(SELECT 1 FROM metric.projects p WHERE p.id=r.project_id AND p.state IN(0,1))")
            .bind(run.as_bytes().as_slice()).bind(evaluated_at.unix_millis()).execute(&self.database.pool).await.map_err(write_error)?;
            Ok(())
        })
    }
}
fn merge(existing: Option<&MonitorRun>, incoming: &MonitorRun) -> Result<Option<MonitorRun>> {
    incoming.validate().map_err(invalid)?;
    let Some(existing) = existing else {
        return Ok(Some(incoming.clone()));
    };
    if existing.project_id != incoming.project_id
        || existing.monitor_id != incoming.monitor_id
        || existing.check_in_id != incoming.check_in_id
        || existing.source != incoming.source
    {
        return Err(SignalStoreError::Conflict);
    }
    if existing.status != MonitorRunStatus::InProgress || !incoming.status.is_terminal() {
        return Ok(None);
    }
    let mut merged = existing.clone();
    merged.status = incoming.status;
    merged.finished_at = incoming.finished_at;
    merged.received_at = incoming.received_at;
    merged.duration_ms = incoming.duration_ms.or(existing.duration_ms);
    merged.delete_at = incoming.delete_at.or(existing.delete_at);
    // Original identity/start/source metadata remain immutable. Validate this
    // actual merged record, not only the individually valid incoming message.
    merged.validate().map_err(invalid)?;
    Ok(Some(merged))
}
fn project_run(monitor: &mut StoredMonitor, run: &MonitorRun) -> Result<bool> {
    let def = &mut monitor.definition;
    if def.last_check_in_at.is_some_and(|v| v > run.received_at) {
        return Ok(false);
    }
    def.last_run_id = Some(run.id);
    def.last_status = Some(run.status);
    def.last_check_in_at = Some(run.received_at);
    def.updated_at = run.received_at;
    if run.status.is_terminal() {
        def.next_expected_at = def
            .config
            .schedule
            .next_after(run.received_at)
            .map_err(invalid)?;
        if def.is_uptime() {
            monitor.lease = None;
        }
    }
    def.validate().map_err(invalid)?;
    Ok(true)
}
fn monitor_select() -> QueryBuilder<Postgres> {
    QueryBuilder::new("WITH selected AS MATERIALIZED (SELECT * FROM metric.monitors WHERE ")
}
fn finish_select<'a>(
    q: &'a mut QueryBuilder<Postgres>,
    order: &'static str,
    limit: usize,
    budget: i64,
) -> &'a mut QueryBuilder<Postgres> {
    q.push(" ORDER BY ").push(order).push(" LIMIT ").push_bind(limit as i64)
        .push("),bounded AS(SELECT *,sum(config_size::bigint+512) OVER(ORDER BY ").push(order).push(" ROWS UNBOUNDED PRECEDING)::bigint AS response_bytes FROM selected) SELECT monitor_id,project_id,kind,slug,name,environment_id,environment,enabled,managed_by_web,revision,config_size,next_expected_at,due_at,lease_until,last_run_id,last_status,last_check_in_at,created_at,updated_at,response_bytes,CASE WHEN response_bytes<=")
        .push_bind(budget).push(" THEN config END AS config FROM bounded ORDER BY ").push(order)
}
async fn load_monitors(
    connection: &mut PgConnection,
    ids: &[Vec<u8>],
    partial: bool,
) -> Result<BTreeMap<MonitorId, StoredMonitor>> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut q = monitor_select();
    q.push("monitor_id=ANY(").push_bind(ids.to_vec()).push(")");
    let rows = finish_select(&mut q, "monitor_id", MAX_BATCH, MAX_BATCH_BYTES)
        .build()
        .fetch_all(connection)
        .await
        .map_err(unavailable)?;
    let mut values = BTreeMap::new();
    for row in rows {
        if row.try_get::<i64, _>("response_bytes").map_err(invalid)? > MAX_BATCH_BYTES {
            if partial {
                break;
            }
            return Err(SignalStoreError::Capacity);
        }
        let value = decode_monitor(&row)?;
        values.insert(value.definition.id, value);
    }
    Ok(values)
}
async fn load_runs(
    connection: &mut PgConnection,
    ids: &[Vec<u8>],
) -> Result<BTreeMap<MonitorRunId, MonitorRun>> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut q = QueryBuilder::<Postgres>::new(RUN_SELECT);
    q.push(" WHERE run_id=ANY(")
        .push_bind(ids.to_vec())
        .push(") ORDER BY run_id LIMIT 10000");
    let rows = q.build().fetch_all(connection).await.map_err(unavailable)?;
    rows.iter()
        .map(|row| {
            let run = decode_run(row)?;
            Ok((run.id, run))
        })
        .collect()
}
async fn lock_lane(connection: &mut PgConnection, projects: &[i32]) -> Result<()> {
    if projects.is_empty() {
        return Ok(());
    }
    sqlx::query("INSERT INTO metric.monitor_project_locks(project_id) SELECT unnest($1::int[]) ORDER BY 1 ON CONFLICT DO NOTHING")
        .bind(projects).execute(&mut *connection).await.map_err(write_error)?;
    sqlx::query("SELECT project_id FROM metric.monitor_project_locks WHERE project_id=ANY($1) ORDER BY project_id FOR UPDATE")
        .bind(projects).fetch_all(connection).await.map_err(unavailable)?;
    Ok(())
}
async fn write_monitors(connection: &mut PgConnection, values: &[&StoredMonitor]) -> Result<()> {
    for batch in values.chunks(CHUNK) {
        let prepared = batch
            .iter()
            .map(|v| {
                Ok((
                    *v,
                    encode_config(&v.definition)?,
                    due(&v.definition)?,
                    positive(v.definition.revision)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut q = QueryBuilder::<Postgres>::new(
            "INSERT INTO metric.monitors(monitor_id,project_id,kind,slug,name,environment_id,environment,enabled,managed_by_web,revision,config,config_size,next_expected_at,due_at,lease_until,last_run_id,last_status,last_check_in_at,created_at,updated_at) ",
        );
        q.push_values(
            &prepared,
            |mut row, (stored, (body, size), due, revision)| {
                let v = &stored.definition;
                row.push_bind(v.id.as_bytes().to_vec())
                    .push_bind(v.project_id.get())
                    .push_bind(i16::from(v.is_uptime()))
                    .push_bind(v.slug.to_string())
                    .push_bind(v.name.to_string())
                    .push_bind(v.environment_id.as_bytes().to_vec())
                    .push_bind(v.environment.to_string())
                    .push_bind(v.enabled)
                    .push_bind(v.managed_by_web)
                    .push_bind(*revision)
                    .push_bind(body.clone())
                    .push_bind(*size as i32)
                    .push_bind(v.next_expected_at.unix_millis())
                    .push_bind(due.unix_millis())
                    .push_bind(stored.lease.map(|v| v.unix_millis()))
                    .push_bind(v.last_run_id.map(|v| v.as_bytes().to_vec()))
                    .push_bind(v.last_status.map(status))
                    .push_bind(v.last_check_in_at.map(|v| v.unix_millis()))
                    .push_bind(v.created_at.unix_millis())
                    .push_bind(v.updated_at.unix_millis());
            },
        );
        q.push(" ON CONFLICT(monitor_id) DO UPDATE SET kind=EXCLUDED.kind,slug=EXCLUDED.slug,name=EXCLUDED.name,environment_id=EXCLUDED.environment_id,environment=EXCLUDED.environment,enabled=EXCLUDED.enabled,managed_by_web=EXCLUDED.managed_by_web,revision=EXCLUDED.revision,config=EXCLUDED.config,config_size=EXCLUDED.config_size,next_expected_at=EXCLUDED.next_expected_at,due_at=EXCLUDED.due_at,lease_until=EXCLUDED.lease_until,last_run_id=EXCLUDED.last_run_id,last_status=EXCLUDED.last_status,last_check_in_at=EXCLUDED.last_check_in_at,created_at=EXCLUDED.created_at,updated_at=EXCLUDED.updated_at WHERE metric.monitors.project_id=EXCLUDED.project_id");
        if q.build()
            .execute(&mut *connection)
            .await
            .map_err(write_error)?
            .rows_affected()
            != batch.len() as u64
        {
            return Err(SignalStoreError::Conflict);
        }
    }
    Ok(())
}
async fn write_runs(connection: &mut PgConnection, values: &[&MonitorRun]) -> Result<()> {
    for batch in values.chunks(CHUNK) {
        for value in batch {
            value.validate().map_err(invalid)?;
        }
        let mut q = QueryBuilder::<Postgres>::new(
            "INSERT INTO metric.monitor_runs(run_id,project_id,monitor_id,check_in_id,status,source,scheduled_for,started_at,finished_at,duration_ms,received_at,release_id,timeout_at,delete_at,http_status,uptime_failure) ",
        );
        q.push_values(batch, |mut row, v| {
            row.push_bind(v.id.as_bytes().to_vec())
                .push_bind(v.project_id.get())
                .push_bind(v.monitor_id.as_bytes().to_vec())
                .push_bind(v.check_in_id.map(|v| v.to_vec()))
                .push_bind(status(v.status))
                .push_bind(if v.source == MonitorRunSource::Sdk {
                    0_i16
                } else {
                    1_i16
                })
                .push_bind(v.scheduled_for.map(|v| v.unix_millis()))
                .push_bind(v.started_at.unix_millis())
                .push_bind(v.finished_at.map(|v| v.unix_millis()))
                .push_bind(v.duration_ms.map(|v| v as i64))
                .push_bind(v.received_at.unix_millis())
                .push_bind(v.release_id.map(|v| v.as_bytes().to_vec()))
                .push_bind(v.timeout_at.map(|v| v.unix_millis()))
                .push_bind(v.delete_at.map(|v| v.unix_millis()))
                .push_bind(v.http_status.map(i32::from))
                .push_bind(v.uptime_failure.map(failure));
        });
        q.push(" ON CONFLICT(run_id) DO UPDATE SET status=EXCLUDED.status,finished_at=EXCLUDED.finished_at,duration_ms=EXCLUDED.duration_ms,received_at=EXCLUDED.received_at,delete_at=EXCLUDED.delete_at WHERE metric.monitor_runs.project_id=EXCLUDED.project_id AND metric.monitor_runs.monitor_id=EXCLUDED.monitor_id AND metric.monitor_runs.source=EXCLUDED.source AND metric.monitor_runs.check_in_id IS NOT DISTINCT FROM EXCLUDED.check_in_id");
        if q.build()
            .execute(&mut *connection)
            .await
            .map_err(write_error)?
            .rows_affected()
            != batch.len() as u64
        {
            return Err(SignalStoreError::Conflict);
        }
    }
    Ok(())
}
fn due(def: &MonitorDefinition) -> Result<Timestamp> {
    if def.is_uptime() {
        Ok(def.next_expected_at)
    } else {
        def.config.missed_at(def.next_expected_at).map_err(invalid)
    }
}
fn positive(v: u64) -> Result<i64> {
    let v = i64::try_from(v).map_err(invalid)?;
    if v <= 0 {
        Err(SignalStoreError::InvalidData)
    } else {
        Ok(v)
    }
}
fn at(v: i64) -> Result<Timestamp> {
    Timestamp::from_unix_millis(v).map_err(invalid)
}
fn add_days(value: Timestamp, days: u32) -> Result<Timestamp> {
    at(value
        .unix_millis()
        .checked_add(i64::from(days) * 86_400_000)
        .ok_or(SignalStoreError::InvalidData)?)
}
fn worker_limit(limit: usize, max: usize) -> Result<()> {
    if !(1..=max).contains(&limit) {
        Err(SignalStoreError::InvalidData)
    } else {
        Ok(())
    }
}
fn page_limit(limit: usize) -> Result<()> {
    worker_limit(limit, 100_000)
}
fn invalid(_: impl std::fmt::Debug) -> SignalStoreError {
    SignalStoreError::InvalidData
}
fn unavailable(_: sqlx::Error) -> SignalStoreError {
    SignalStoreError::Unavailable
}
fn write_error(error: sqlx::Error) -> SignalStoreError {
    match error.as_database_error().and_then(|e| e.code()).as_deref() {
        Some("23505") => SignalStoreError::Conflict,
        Some("23503") => SignalStoreError::NotFound,
        Some("23514" | "22003" | "22021") => SignalStoreError::InvalidData,
        _ => SignalStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
