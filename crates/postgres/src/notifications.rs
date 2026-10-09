//! Native scoped notification control plane and transactional outbox/leases.
use crate::{PostgresDatabase, signal_storage};
use metric_domain::{
    EventId, ProjectId, Timestamp,
    grouping::IssueId,
    issue::{IssueNotificationKind, IssueTitle, IssueTransitionId},
    notifications::*,
};
use metric_ports::{NotificationStore, NotificationStoreError, PortFuture};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};
use std::collections::{BTreeMap, BTreeSet};
mod codec;
use codec::{
    decode_delivery, decode_destination, decode_rule, decode_transition, encode_destination,
    encode_rule, fixed, mask, optional_time, rule_kind, timestamp,
};
type Result<T> = std::result::Result<T, NotificationStoreError>;
const MAX_CONFIG: usize = 64 * 1024;
const READ_BYTES: i64 = 8 * 1024 * 1024;
const MAX_BATCH: usize = 10_000;
const BATCH_BYTES: usize = 16 * 1024 * 1024;
const CHUNK: usize = 1000;

#[derive(Clone)]
pub struct PostgresNotificationStore {
    database: PostgresDatabase,
}
#[derive(Clone, Copy)]
enum ConfigTable {
    Destination,
    Rule,
}
struct RuleCompletion {
    lease: Option<Timestamp>,
    next: Option<Timestamp>,
    last: Option<Timestamp>,
    window: Option<Timestamp>,
    count: u32,
    threshold: bool,
    updated: Option<Timestamp>,
}
impl PostgresNotificationStore {
    pub fn new(database: PostgresDatabase) -> Self {
        Self { database }
    }
    async fn enqueue(&self, deliveries: Vec<NotificationDelivery>) -> Result<()> {
        validate_batch(&deliveries)?;
        if deliveries.is_empty() {
            return Ok(());
        }
        let projects = projects(&deliveries);
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        guard(&mut tx, &projects).await?;
        write_deliveries(&mut tx, &deliveries).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }
    async fn expand(
        &self,
        transition: IssueNotificationTransition,
        deliveries: Vec<NotificationDelivery>,
    ) -> Result<()> {
        validate_batch(&deliveries)?;
        if deliveries.iter().any(|v| {
            v.project_id != transition.project_id
                || v.issue_id != transition.issue_id
                || v.transition_id != transition.transition_id
        }) {
            return Err(NotificationStoreError::InvalidData);
        }
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        guard(&mut tx, &[transition.project_id.get()]).await?;
        let stored=sqlx::query("SELECT issue_id,kind,event_id,created_at FROM metric.issue_transitions WHERE project_id=$1 AND transition_id=$2 FOR UPDATE")
            .bind(transition.project_id.get()).bind(transition.transition_id.as_bytes().as_slice()).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        if let Some(row) = stored
            && (fixed(&row, "issue_id")? != transition.issue_id.as_bytes()
                || row.try_get::<i16, _>("kind").map_err(invalid)? != transition.kind as i16
                || fixed(&row, "event_id")? != transition.event_id.as_bytes()
                || timestamp(&row, "created_at")? != transition.created_at)
        {
            return Err(NotificationStoreError::InvalidData);
        }
        // Stable delivery identities commit before/with transition consumption;
        // retry after an ambiguous commit remains safe even when it is gone.
        write_deliveries(&mut tx, &deliveries).await?;
        sqlx::query(
            "DELETE FROM metric.issue_transitions WHERE project_id=$1 AND transition_id=$2",
        )
        .bind(transition.project_id.get())
        .bind(transition.transition_id.as_bytes().as_slice())
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }
    async fn complete(
        &self,
        id: AlertRuleId,
        state: RuleCompletion,
        deliveries: Vec<NotificationDelivery>,
    ) -> Result<()> {
        validate_batch(&deliveries)?;
        let Some(project) = sqlx::query_scalar::<_, i32>(
            "SELECT project_id FROM metric.notification_rules WHERE rule_id=$1",
        )
        .bind(id.as_bytes().as_slice())
        .fetch_optional(&self.database.pool)
        .await
        .map_err(unavailable)?
        else {
            return Ok(());
        };
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        guard(&mut tx, &[project]).await?;
        let Some(row)=sqlx::query("SELECT project_id,kind,lease_until FROM metric.notification_rules WHERE rule_id=$1 FOR UPDATE").bind(id.as_bytes().as_slice()).fetch_optional(&mut *tx).await.map_err(unavailable)? else{return Ok(());};
        if row.get::<i32, _>("project_id") != project
            || row.get::<i16, _>("kind") != if state.lease.is_some() { 1 } else { 2 }
            || deliveries
                .iter()
                .any(|v| v.project_id.get() != project || v.rule_id != id)
        {
            return Err(NotificationStoreError::InvalidData);
        }
        // A stale evaluator cannot enqueue notifications or overwrite the newer
        // lease's state. The unchanged port carries the aggregate lease token.
        if state.lease.is_some() && optional_time(&row, "lease_until")? != state.lease {
            return Ok(());
        }
        write_deliveries(&mut tx, &deliveries).await?;
        sqlx::query("UPDATE metric.notification_rules SET next_evaluation_at=coalesce($2,next_evaluation_at),last_triggered_at=$3,storm_window_started_at=$4,storm_count=$5,threshold_met=$6,lease_until=NULL,updated_at=coalesce($7,updated_at) WHERE rule_id=$1")
            .bind(id.as_bytes().as_slice()).bind(state.next.map(|v|v.unix_millis())).bind(state.last.map(|v|v.unix_millis())).bind(state.window.map(|v|v.unix_millis())).bind(i64::from(state.count)).bind(state.threshold).bind(state.updated.map(|v|v.unix_millis()))
            .execute(&mut *tx).await.map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }
    async fn claim_delivery(
        &self,
        now: Timestamp,
        lease: Timestamp,
        limit: usize,
    ) -> Result<Option<ClaimedNotificationDelivery>> {
        bounded(limit, 1000)?;
        if lease <= now {
            return Err(NotificationStoreError::InvalidData);
        }
        let candidates=sqlx::query("SELECT d.delivery_id,d.project_id,d.destination_id,d.next_attempt_at FROM metric.notification_deliveries d JOIN metric.projects p ON p.id=d.project_id WHERE d.status=0 AND d.next_attempt_at<=$1 AND p.state IN(0,1) ORDER BY d.next_attempt_at,d.delivery_id LIMIT $2")
            .bind(now.unix_millis()).bind(limit as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?;
        if candidates.is_empty() {
            return Ok(None);
        }
        let requested = candidate_projects(&candidates);
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        let allowed = worker_guard(&mut tx, &requested).await?;
        let cursor=sqlx::query("SELECT last_project,last_destination FROM metric.notification_claim_cursor WHERE singleton FOR UPDATE").fetch_one(&mut *tx).await.map_err(unavailable)?;
        let last: Option<i32> = cursor.try_get("last_project").map_err(invalid)?;
        let destination: Option<Vec<u8>> = cursor.try_get("last_destination").map_err(invalid)?;
        // Recheck eligibility after the project/lane/cursor locks. Only native
        // bounded identity metadata is scanned to make the fairness decision.
        let ids = candidates
            .iter()
            .map(|r| r.get::<Vec<u8>, _>("delivery_id"))
            .collect::<Vec<_>>();
        let mut fresh=sqlx::query("SELECT delivery_id,project_id,destination_id,next_attempt_at FROM metric.notification_deliveries WHERE delivery_id=ANY($1) AND project_id=ANY($2) AND status=0 AND next_attempt_at<=$3 ORDER BY next_attempt_at,delivery_id")
            .bind(ids).bind(&allowed).bind(now.unix_millis()).fetch_all(&mut *tx).await.map_err(unavailable)?;
        fresh.sort_by_key(|r| {
            let p = r.get::<i32, _>("project_id");
            let d = r.get::<Vec<u8>, _>("destination_id");
            let rank =
                if last.is_none_or(|v| v != p) && destination.as_ref().is_none_or(|v| v != &d) {
                    0
                } else if last != Some(p) {
                    1
                } else if destination.as_ref() != Some(&d) {
                    2
                } else {
                    3
                };
            (
                rank,
                r.get::<i64, _>("next_attempt_at"),
                r.get::<Vec<u8>, _>("delivery_id"),
            )
        });
        let Some(candidate) = fresh.first() else {
            return Ok(None);
        };
        let p = candidate.get::<i32, _>("project_id");
        let d = candidate.get::<Vec<u8>, _>("destination_id");
        let row=sqlx::query("UPDATE metric.notification_deliveries SET attempts=attempts+1,next_attempt_at=$2,attempted_at=$3 WHERE delivery_id=$1 AND status=0 AND attempts<4294967295 RETURNING *")
            .bind(candidate.get::<Vec<u8>,_>("delivery_id")).bind(lease.unix_millis()).bind(now.unix_millis()).fetch_optional(&mut *tx).await.map_err(unavailable)?.ok_or(NotificationStoreError::InvalidData)?;
        sqlx::query("UPDATE metric.notification_claim_cursor SET last_project=$1,last_destination=$2 WHERE singleton").bind(p).bind(&d).execute(&mut *tx).await.map_err(unavailable)?;
        let destination=sqlx::query("SELECT * FROM metric.notification_destinations WHERE project_id=$1 AND destination_id=$2").bind(p).bind(d).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let result = decode_delivery(&row).and_then(|delivery| {
            let destination = destination
                .as_ref()
                .ok_or(NotificationStoreError::InvalidData)
                .and_then(decode_destination)?;
            Ok(ClaimedNotificationDelivery {
                attempt: delivery.attempts,
                delivery,
                destination,
                attempted_at: now,
            })
        });
        // As in the existing adapter, a missing/corrupt destination still leaves
        // an acknowledged attempt lease; other queued work remains claimable.
        tx.commit().await.map_err(unavailable)?;
        result.map(Some)
    }
    async fn claim_aggregate(&self, now: Timestamp, lease: Timestamp) -> Result<Option<AlertRule>> {
        if lease <= now {
            return Err(NotificationStoreError::InvalidData);
        }
        let candidates=sqlx::query("SELECT r.rule_id,r.project_id FROM metric.notification_rules r JOIN metric.projects p ON p.id=r.project_id WHERE r.kind=1 AND r.enabled AND r.next_evaluation_at<=$1 AND (r.lease_until IS NULL OR r.lease_until<=$1) AND p.state IN(0,1) ORDER BY r.next_evaluation_at,r.rule_id LIMIT 1000")
            .bind(now.unix_millis()).fetch_all(&self.database.pool).await.map_err(unavailable)?;
        if candidates.is_empty() {
            return Ok(None);
        }
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        let allowed = worker_guard(&mut tx, &candidate_projects(&candidates)).await?;
        let ids = candidates
            .iter()
            .map(|r| r.get::<Vec<u8>, _>("rule_id"))
            .collect::<Vec<_>>();
        let Some(row)=sqlx::query("SELECT * FROM metric.notification_rules WHERE rule_id=ANY($1) AND project_id=ANY($2) AND kind=1 AND enabled AND next_evaluation_at<=$3 AND (lease_until IS NULL OR lease_until<=$3) ORDER BY next_evaluation_at,rule_id LIMIT 1 FOR UPDATE")
            .bind(ids).bind(allowed).bind(now.unix_millis()).fetch_optional(&mut *tx).await.map_err(unavailable)? else{return Ok(None);};
        sqlx::query("UPDATE metric.notification_rules SET lease_until=$2 WHERE rule_id=$1")
            .bind(row.get::<Vec<u8>, _>("rule_id"))
            .bind(lease.unix_millis())
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let value = decode_rule(&row);
        tx.commit().await.map_err(unavailable)?;
        value.map(Some)
    }
    async fn mutate_delivery(
        &self,
        id: NotificationDeliveryId,
        next: Option<Timestamp>,
        terminal: Option<(i16, Timestamp, Timestamp)>,
        error: Option<&str>,
    ) -> Result<()> {
        if let Some(error) = error {
            validate_error(error)?;
        }
        let Some(p) = sqlx::query_scalar::<_, i32>(
            "SELECT project_id FROM metric.notification_deliveries WHERE delivery_id=$1",
        )
        .bind(id.as_bytes().as_slice())
        .fetch_optional(&self.database.pool)
        .await
        .map_err(unavailable)?
        else {
            return Ok(());
        };
        let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
        guard(&mut tx, &[p]).await?;
        if let Some((status, at, delete)) = terminal {
            sqlx::query("UPDATE metric.notification_deliveries SET status=$2,terminal_at=$3,delete_at=$4,next_attempt_at=NULL,attempted_at=NULL,last_error=coalesce($5,last_error) WHERE delivery_id=$1 AND status=0")
                .bind(id.as_bytes().as_slice()).bind(status).bind(at.unix_millis()).bind(delete.unix_millis()).bind(error).execute(&mut *tx).await.map_err(unavailable)?;
        } else {
            sqlx::query("UPDATE metric.notification_deliveries SET next_attempt_at=$2,attempted_at=NULL,last_error=$3 WHERE delivery_id=$1 AND status=0")
                .bind(id.as_bytes().as_slice()).bind(next.map(|v|v.unix_millis())).bind(error).execute(&mut *tx).await.map_err(unavailable)?;
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }
}
impl NotificationStore for PostgresNotificationStore {
    fn pending_transitions(
        &self,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<IssueNotificationTransition>>> {
        Box::pin(async move {
            bounded(limit, 10_000)?;
            let rows=sqlx::query("SELECT t.*,i.title FROM metric.issue_transitions t JOIN metric.issues i ON i.project_id=t.project_id AND i.issue_id=t.issue_id JOIN metric.projects p ON p.id=t.project_id WHERE p.state IN(0,1) ORDER BY t.created_at,t.project_id,t.transition_id LIMIT $1")
            .bind(limit as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?;
            rows.iter().map(decode_transition).collect()
        })
    }
    fn matching_rules(
        &self,
        project: ProjectId,
        kind: IssueNotificationKind,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<AlertRule>>> {
        Box::pin(async move {
            bounded(limit, 1000)?;
            let bit = 1_i16 << (kind as u8 - 1);
            let mut q = start(ConfigTable::Rule);
            q.push("project_id=")
                .push_bind(project.get())
                .push(" AND kind=0 AND enabled AND (trigger_mask & ")
                .push_bind(bit)
                .push(")<>0");
            let rows = finish(&mut q, ConfigTable::Rule, "rule_id", limit + 1)
                .build()
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
            if rows.len() > limit {
                return Err(NotificationStoreError::InvalidData);
            }
            rows.iter().map(decode_rule).collect()
        })
    }
    fn expand_transition(
        &self,
        t: IssueNotificationTransition,
        d: Vec<NotificationDelivery>,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(self.expand(t, d))
    }
    fn claim_due(
        &self,
        now: Timestamp,
        lease: Timestamp,
        limit: usize,
    ) -> PortFuture<'_, Result<Option<ClaimedNotificationDelivery>>> {
        Box::pin(self.claim_delivery(now, lease, limit))
    }
    fn mark_delivered(
        &self,
        id: NotificationDeliveryId,
        at: Timestamp,
        delete: Timestamp,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(async move {
            self.mutate_delivery(id, None, Some((1, at, delete)), None)
                .await
        })
    }
    fn schedule_retry(
        &self,
        id: NotificationDeliveryId,
        next: Timestamp,
        error: &'static str,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(self.mutate_delivery(id, Some(next), None, Some(error)))
    }
    fn mark_dead(
        &self,
        id: NotificationDeliveryId,
        at: Timestamp,
        delete: Timestamp,
        error: &'static str,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(self.mutate_delivery(id, None, Some((2, at, delete)), Some(error)))
    }
    fn upsert_destination(&self, v: NotificationDestination) -> PortFuture<'_, Result<()>> {
        Box::pin(async move {
            let (config, size) = encode_destination(&v)?;
            let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
            guard(&mut tx, &[v.project_id.get()]).await?;
            let kind = match v.kind {
                NotificationDestinationKind::Webhook => 0_i16,
                NotificationDestinationKind::Telegram => 1,
                NotificationDestinationKind::SmtpEmail => 2,
            };
            let changed=sqlx::query("INSERT INTO metric.notification_destinations(destination_id,project_id,kind,endpoint,sealed_secret,config,config_size,enabled,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(destination_id) DO UPDATE SET kind=EXCLUDED.kind,endpoint=EXCLUDED.endpoint,sealed_secret=EXCLUDED.sealed_secret,config=EXCLUDED.config,config_size=EXCLUDED.config_size,enabled=EXCLUDED.enabled,created_at=EXCLUDED.created_at,updated_at=EXCLUDED.updated_at WHERE metric.notification_destinations.project_id=EXCLUDED.project_id")
            .bind(v.id.as_bytes().as_slice()).bind(v.project_id.get()).bind(kind).bind(v.endpoint.as_str().as_bytes()).bind(v.sealed_secret.expose_ciphertext()).bind(config).bind(size as i32).bind(v.enabled).bind(v.created_at.unix_millis()).bind(v.updated_at.unix_millis())
            .execute(&mut *tx).await.map_err(unavailable)?.rows_affected();
            if changed != 1 {
                return Err(NotificationStoreError::InvalidData);
            }
            tx.commit().await.map_err(unavailable)?;
            Ok(())
        })
    }
    fn upsert_rule(&self, v: AlertRule) -> PortFuture<'_, Result<()>> {
        Box::pin(async move {
            let (config, size) = encode_rule(&v)?;
            let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
            guard(&mut tx, &[v.project_id.get()]).await?;
            let changed=sqlx::query("INSERT INTO metric.notification_rules(rule_id,project_id,kind,trigger_mask,name,enabled,config,config_size,cooldown_minutes,storm_limit,next_evaluation_at,last_triggered_at,storm_window_started_at,storm_count,threshold_met,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) ON CONFLICT(rule_id) DO UPDATE SET kind=EXCLUDED.kind,trigger_mask=EXCLUDED.trigger_mask,name=EXCLUDED.name,enabled=EXCLUDED.enabled,config=EXCLUDED.config,config_size=EXCLUDED.config_size,cooldown_minutes=EXCLUDED.cooldown_minutes,storm_limit=EXCLUDED.storm_limit,next_evaluation_at=EXCLUDED.next_evaluation_at,last_triggered_at=EXCLUDED.last_triggered_at,storm_window_started_at=EXCLUDED.storm_window_started_at,storm_count=EXCLUDED.storm_count,threshold_met=EXCLUDED.threshold_met,created_at=EXCLUDED.created_at,updated_at=EXCLUDED.updated_at,lease_until=NULL WHERE metric.notification_rules.project_id=EXCLUDED.project_id")
            .bind(v.id.as_bytes().as_slice()).bind(v.project_id.get()).bind(rule_kind(&v)).bind(mask(&v)).bind(v.name.as_str().as_bytes()).bind(v.enabled).bind(config).bind(size as i32).bind(v.cooldown_minutes as i32).bind(v.storm_limit_per_hour as i32)
            .bind(v.next_evaluation_at.map(|v|v.unix_millis())).bind(v.last_triggered_at.map(|v|v.unix_millis())).bind(v.storm_window_started_at.map(|v|v.unix_millis())).bind(i64::from(v.storm_count)).bind(v.threshold_met).bind(v.created_at.unix_millis()).bind(v.updated_at.unix_millis())
            .execute(&mut *tx).await.map_err(unavailable)?.rows_affected();
            if changed != 1 {
                return Err(NotificationStoreError::InvalidData);
            }
            tx.commit().await.map_err(unavailable)?;
            Ok(())
        })
    }
    fn delete_destination(
        &self,
        p: ProjectId,
        id: NotificationDestinationId,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(async move {
            let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
            guard(&mut tx, &[p.get()]).await?;
            sqlx::query("DELETE FROM metric.notification_destinations WHERE project_id=$1 AND destination_id=$2").bind(p.get()).bind(id.as_bytes().as_slice()).execute(&mut *tx).await.map_err(unavailable)?;
            tx.commit().await.map_err(unavailable)?;
            Ok(())
        })
    }
    fn delete_rule(&self, p: ProjectId, id: AlertRuleId) -> PortFuture<'_, Result<()>> {
        Box::pin(async move {
            let mut tx = self.database.pool.begin().await.map_err(unavailable)?;
            guard(&mut tx, &[p.get()]).await?;
            sqlx::query("DELETE FROM metric.notification_rules WHERE project_id=$1 AND rule_id=$2")
                .bind(p.get())
                .bind(id.as_bytes().as_slice())
                .execute(&mut *tx)
                .await
                .map_err(unavailable)?;
            tx.commit().await.map_err(unavailable)?;
            Ok(())
        })
    }
    fn list_destinations(
        &self,
        p: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<NotificationDestination>>> {
        Box::pin(async move {
            bounded(limit, 1000)?;
            let mut q = start(ConfigTable::Destination);
            q.push("project_id=").push_bind(p.get());
            let rows = finish(
                &mut q,
                ConfigTable::Destination,
                "created_at,destination_id",
                limit,
            )
            .build()
            .fetch_all(&self.database.pool)
            .await
            .map_err(unavailable)?;
            rows.iter().map(decode_destination).collect()
        })
    }
    fn list_rules(&self, p: ProjectId, limit: usize) -> PortFuture<'_, Result<Vec<AlertRule>>> {
        Box::pin(async move {
            bounded(limit, 1000)?;
            let mut q = start(ConfigTable::Rule);
            q.push("project_id=").push_bind(p.get());
            let rows = finish(&mut q, ConfigTable::Rule, "created_at,rule_id", limit)
                .build()
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
            rows.iter().map(decode_rule).collect()
        })
    }
    fn claim_due_aggregate_rule(
        &self,
        now: Timestamp,
        lease: Timestamp,
    ) -> PortFuture<'_, Result<Option<AlertRule>>> {
        Box::pin(self.claim_aggregate(now, lease))
    }
    fn complete_aggregate_rule(
        &self,
        id: AlertRuleId,
        lease: Timestamp,
        next: Timestamp,
        threshold: bool,
        last: Option<Timestamp>,
        window: Option<Timestamp>,
        count: u32,
        d: Vec<NotificationDelivery>,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(self.complete(
            id,
            RuleCompletion {
                lease: Some(lease),
                next: Some(next),
                last,
                window,
                count,
                threshold,
                updated: None,
            },
            d,
        ))
    }
    fn enqueue_delivery(&self, d: NotificationDelivery) -> PortFuture<'_, Result<()>> {
        Box::pin(self.enqueue(vec![d]))
    }
    fn complete_monitor_alert(
        &self,
        id: AlertRuleId,
        last: Timestamp,
        window: Timestamp,
        count: u32,
        threshold: bool,
        d: Vec<NotificationDelivery>,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(self.complete(
            id,
            RuleCompletion {
                lease: None,
                next: None,
                last: Some(last),
                window: Some(window),
                count,
                threshold,
                updated: Some(last),
            },
            d,
        ))
    }
    fn list_delivery_history(
        &self,
        p: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<NotificationDelivery>>> {
        Box::pin(async move {
            bounded(limit, 500)?;
            let rows=sqlx::query("WITH selected AS MATERIALIZED(SELECT * FROM metric.notification_deliveries WHERE project_id=$1 ORDER BY created_at DESC,delivery_id DESC LIMIT $2),bounded AS(SELECT *,sum(octet_length(payload)::bigint) OVER(ORDER BY created_at DESC,delivery_id DESC ROWS UNBOUNDED PRECEDING)::bigint+count(*) OVER()*512 AS response_bytes FROM selected) SELECT delivery_id,project_id,issue_id,transition_id,rule_id,action_id,destination_id,status,attempts,next_attempt_at,last_error,created_at,terminal_at,delete_at,CASE WHEN response_bytes<=$3 THEN payload END AS payload FROM bounded ORDER BY created_at DESC,delivery_id DESC")
            .bind(p.get()).bind(limit as i64).bind(READ_BYTES).fetch_all(&self.database.pool).await.map_err(unavailable)?;
            rows.iter().map(decode_delivery).collect()
        })
    }
}
fn projects(d: &[NotificationDelivery]) -> Vec<i32> {
    d.iter()
        .map(|v| v.project_id.get())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn candidate_projects(rows: &[PgRow]) -> Vec<i32> {
    rows.iter()
        .map(|r| r.get("project_id"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
async fn guard(c: &mut PgConnection, projects: &[i32]) -> Result<()> {
    signal_storage::lock_projects(c, projects)
        .await
        .map_err(|e| match e {
            metric_ports::SignalStoreError::Unavailable => NotificationStoreError::Unavailable,
            _ => NotificationStoreError::InvalidData,
        })?;
    lock_lanes(c, projects).await
}
async fn worker_guard(c: &mut PgConnection, projects: &[i32]) -> Result<Vec<i32>> {
    let allowed = sqlx::query_scalar(
        "SELECT id FROM metric.projects WHERE id=ANY($1) AND state IN(0,1) ORDER BY id FOR SHARE",
    )
    .bind(projects)
    .fetch_all(&mut *c)
    .await
    .map_err(unavailable)?;
    lock_lanes(c, &allowed).await?;
    Ok(allowed)
}
async fn lock_lanes(c: &mut PgConnection, p: &[i32]) -> Result<()> {
    if p.is_empty() {
        return Ok(());
    }
    sqlx::query("INSERT INTO metric.notification_project_locks(project_id) SELECT unnest($1::int[]) ORDER BY 1 ON CONFLICT DO NOTHING").bind(p).execute(&mut *c).await.map_err(unavailable)?;
    sqlx::query("SELECT project_id FROM metric.notification_project_locks WHERE project_id=ANY($1) ORDER BY project_id FOR UPDATE").bind(p).fetch_all(c).await.map_err(unavailable)?;
    Ok(())
}
fn validate_batch(d: &[NotificationDelivery]) -> Result<()> {
    if d.len() > MAX_BATCH {
        return Err(NotificationStoreError::InvalidData);
    }
    let mut total = 0usize;
    for v in d {
        total = total
            .checked_add(v.payload.as_bytes().len() + 512)
            .ok_or(NotificationStoreError::InvalidData)?;
    }
    if total > BATCH_BYTES {
        return Err(NotificationStoreError::InvalidData);
    }
    Ok(())
}
fn same_identity(a: &NotificationDelivery, b: &NotificationDelivery) -> bool {
    a.project_id == b.project_id
        && a.issue_id == b.issue_id
        && a.transition_id == b.transition_id
        && a.rule_id == b.rule_id
        && a.action_id == b.action_id
        && a.destination_id == b.destination_id
}
async fn write_deliveries(c: &mut PgConnection, values: &[NotificationDelivery]) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    let mut unique = BTreeMap::new();
    for v in values {
        if let Some(old) = unique.get(&v.id) {
            if !same_identity(old, v) {
                return Err(NotificationStoreError::InvalidData);
            }
        } else {
            unique.insert(v.id, v.clone());
        }
    }
    // A supplied foreign rule/destination never permits reading another project's
    // secret. Missing/deleted targets remain valid historical outbox identities.
    let destinations = unique
        .values()
        .map(|v| v.destination_id.as_bytes().to_vec())
        .collect::<Vec<_>>();
    let rules = unique
        .values()
        .map(|v| v.rule_id.as_bytes().to_vec())
        .collect::<Vec<_>>();
    let target_rows=sqlx::query("SELECT destination_id AS id,project_id,0 AS kind FROM metric.notification_destinations WHERE destination_id=ANY($1) UNION ALL SELECT rule_id AS id,project_id,1 AS kind FROM metric.notification_rules WHERE rule_id=ANY($2)")
        .bind(destinations).bind(rules).fetch_all(&mut *c).await.map_err(unavailable)?;
    let targets = target_rows
        .iter()
        .map(|r| {
            (
                (r.get::<i32, _>("kind"), r.get::<Vec<u8>, _>("id")),
                r.get::<i32, _>("project_id"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for v in unique.values() {
        for (kind, id) in [(0, v.destination_id.as_bytes()), (1, v.rule_id.as_bytes())] {
            if targets
                .get(&(kind, id.to_vec()))
                .is_some_and(|p| *p != v.project_id.get())
            {
                return Err(NotificationStoreError::InvalidData);
            }
        }
    }
    let values = unique.values().collect::<Vec<_>>();
    for batch in values.chunks(CHUNK) {
        let mut q = QueryBuilder::<Postgres>::new(
            "INSERT INTO metric.notification_deliveries(delivery_id,project_id,issue_id,transition_id,rule_id,action_id,destination_id,payload,status,attempts,next_attempt_at,created_at) ",
        );
        q.push_values(batch, |mut r, v| {
            r.push_bind(v.id.as_bytes().to_vec())
                .push_bind(v.project_id.get())
                .push_bind(v.issue_id.as_bytes().to_vec())
                .push_bind(v.transition_id.as_bytes().to_vec())
                .push_bind(v.rule_id.as_bytes().to_vec())
                .push_bind(v.action_id.as_bytes().to_vec())
                .push_bind(v.destination_id.as_bytes().to_vec())
                .push_bind(v.payload.as_bytes().to_vec())
                .push_bind(0_i16)
                .push_bind(i64::from(v.attempts))
                .push_bind(v.next_attempt_at.unix_millis())
                .push_bind(v.created_at.unix_millis());
        });
        q.push(" ON CONFLICT(delivery_id) DO UPDATE SET delivery_id=EXCLUDED.delivery_id WHERE metric.notification_deliveries.project_id=EXCLUDED.project_id AND metric.notification_deliveries.issue_id=EXCLUDED.issue_id AND metric.notification_deliveries.transition_id=EXCLUDED.transition_id AND metric.notification_deliveries.rule_id=EXCLUDED.rule_id AND metric.notification_deliveries.action_id=EXCLUDED.action_id AND metric.notification_deliveries.destination_id=EXCLUDED.destination_id");
        if q.build()
            .execute(&mut *c)
            .await
            .map_err(unavailable)?
            .rows_affected()
            != batch.len() as u64
        {
            return Err(NotificationStoreError::InvalidData);
        }
    }
    Ok(())
}
fn start(table: ConfigTable) -> QueryBuilder<Postgres> {
    QueryBuilder::new(match table {
        ConfigTable::Destination => {
            "WITH selected AS MATERIALIZED(SELECT * FROM metric.notification_destinations WHERE "
        }
        ConfigTable::Rule => {
            "WITH selected AS MATERIALIZED(SELECT * FROM metric.notification_rules WHERE "
        }
    })
}
fn finish<'a>(
    q: &'a mut QueryBuilder<Postgres>,
    table: ConfigTable,
    order: &'static str,
    limit: usize,
) -> &'a mut QueryBuilder<Postgres> {
    q.push(" ORDER BY ")
        .push(order)
        .push(" LIMIT ")
        .push_bind(limit as i64)
        .push(
            "),bounded AS(SELECT *,sum(GREATEST(config_size::bigint,octet_length(config)::bigint)",
        );
    if matches!(table, ConfigTable::Destination) {
        q.push("+octet_length(sealed_secret)+octet_length(endpoint)");
    }
    q.push(") OVER(ORDER BY ")
        .push(order)
        .push(" ROWS UNBOUNDED PRECEDING)::bigint+count(*) OVER()*512 AS response_bytes FROM selected) SELECT ");
    q.push(match table {ConfigTable::Destination=>"destination_id,project_id,kind,config_size,enabled,created_at,updated_at",ConfigTable::Rule=>"rule_id,project_id,kind,trigger_mask,name,enabled,config_size,cooldown_minutes,storm_limit,next_evaluation_at,lease_until,last_triggered_at,storm_window_started_at,storm_count,threshold_met,created_at,updated_at"});
    if matches!(table, ConfigTable::Destination) {
        q.push(",CASE WHEN response_bytes<=")
            .push_bind(READ_BYTES)
            .push(" THEN endpoint END AS endpoint,CASE WHEN response_bytes<=")
            .push_bind(READ_BYTES)
            .push(" THEN sealed_secret END AS sealed_secret");
    }
    q.push(",CASE WHEN response_bytes<=")
        .push_bind(READ_BYTES)
        .push(" THEN config END AS config FROM bounded ORDER BY ")
        .push(order)
}
fn bounded(v: usize, max: usize) -> Result<()> {
    if !(1..=max).contains(&v) {
        Err(NotificationStoreError::InvalidData)
    } else {
        Ok(())
    }
}
fn validate_error(v: &str) -> Result<()> {
    if v.is_empty() || v.len() > 64 || v.chars().any(char::is_control) {
        Err(NotificationStoreError::InvalidData)
    } else {
        Ok(())
    }
}
fn at(v: i64) -> Result<Timestamp> {
    Timestamp::from_unix_millis(v).map_err(invalid)
}
fn invalid(_: impl std::fmt::Debug) -> NotificationStoreError {
    NotificationStoreError::InvalidData
}
fn unavailable(_: sqlx::Error) -> NotificationStoreError {
    NotificationStoreError::Unavailable
}
#[cfg(test)]
mod tests;
