//! Durable notification transition expansion and bounded webhook delivery.

use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc, time::Duration};

use crate::{
    auth::{AuthError, IdentityService},
    explore::ExploreService,
    shutdown::ShutdownSignal,
};
use metric_domain::{
    Timestamp,
    auth::{AuditAction, AuthContext, Permission, RequestCorrelationId},
    explore::{
        ExploreAggregate, ExploreAggregateKind, ExploreField, ExplorePredicate, ExplorePredicateOp,
        ExploreQuery, ExploreValue,
    },
    grouping::IssueId,
    issue::IssueTransitionId,
    monitors::MonitorRun,
    notifications::{
        AlertRule, ClaimedNotificationDelivery, IssueNotificationTransition, NotificationDelivery,
        NotificationDeliveryStatus, NotificationDestination, NotificationPayload,
        notification_delivery_id,
    },
};
use metric_ports::{
    Clock, MonitorStore, NotificationDeliveryAdapter, NotificationDeliveryError,
    NotificationDeliveryReceipt, NotificationStore, NotificationStoreError, SignalStoreError,
};
use serde_json::json;
use thiserror::Error;
use tokio::{
    sync::{Mutex, Notify, mpsc},
    task::JoinHandle,
    time::{MissedTickBehavior, interval, timeout},
};

const EXPANSION_RULE_LIMIT: usize = 256;
const AGGREGATE_ALIAS: &str = "count";

#[derive(Debug, Clone, Copy)]
pub struct NotificationConfig {
    pub queue_capacity: usize,
    pub worker_concurrency: usize,
    pub transition_batch_size: usize,
    pub due_scan_limit: usize,
    pub poll_interval: Duration,
    pub attempt_timeout: Duration,
    pub attempt_lease: Duration,
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub delivered_retention: Duration,
    pub dead_retention: Duration,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 1_000,
            worker_concurrency: 8,
            transition_batch_size: 100,
            due_scan_limit: 100,
            poll_interval: Duration::from_millis(250),
            attempt_timeout: Duration::from_secs(10),
            attempt_lease: Duration::from_secs(30),
            max_attempts: 8,
            initial_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(60 * 60),
            delivered_retention: Duration::from_secs(30 * 24 * 60 * 60),
            dead_retention: Duration::from_secs(90 * 24 * 60 * 60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NotificationError {
    #[error("notification configuration is invalid")]
    InvalidConfiguration,
    #[error("notification storage is temporarily unavailable")]
    StorageUnavailable,
    #[error("notification storage contains invalid data")]
    InvalidData,
    #[error("notification administration is forbidden")]
    Forbidden,
    #[error("project is disabled")]
    ProjectDisabled,
    #[error("project deletion is pending")]
    ProjectDeletionPending,
    #[error("project purge is in progress")]
    ProjectPurging,
    #[error("project is deleted")]
    ProjectDeleted,
}

impl NotificationError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "notification_invalid_configuration",
            Self::StorageUnavailable => "notification_temporarily_unavailable",
            Self::InvalidData => "notification_invalid_data",
            Self::Forbidden => "notification_forbidden",
            Self::ProjectDisabled => "project_disabled",
            Self::ProjectDeletionPending => "project_deletion_pending",
            Self::ProjectPurging => "project_purging",
            Self::ProjectDeleted => "project_deleted",
        }
    }
}

pub type NotificationAccessFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), NotificationError>> + Send + 'a>>;

pub trait NotificationAdminAccess: Send + Sync + 'static {
    fn authorize<'a>(
        &'a self,
        context: &'a AuthContext,
        project_id: metric_domain::ProjectId,
    ) -> NotificationAccessFuture<'a>;

    fn audit<'a>(
        &'a self,
        context: &'a AuthContext,
        request_id: RequestCorrelationId,
        project_id: metric_domain::ProjectId,
        action: AuditAction,
        target_id: String,
    ) -> NotificationAccessFuture<'a>;
}

impl NotificationAdminAccess for IdentityService {
    fn authorize<'a>(
        &'a self,
        context: &'a AuthContext,
        project_id: metric_domain::ProjectId,
    ) -> NotificationAccessFuture<'a> {
        Box::pin(async move {
            self.authorize_project_mutation(context, project_id, Permission::ProjectAdmin)
                .await
                .map_err(map_auth_error)
        })
    }

    fn audit<'a>(
        &'a self,
        context: &'a AuthContext,
        request_id: RequestCorrelationId,
        project_id: metric_domain::ProjectId,
        action: AuditAction,
        target_id: String,
    ) -> NotificationAccessFuture<'a> {
        Box::pin(async move {
            self.record_notification_audit(context, request_id, project_id, action, target_id)
                .await
                .map_err(map_auth_error)
        })
    }
}

pub struct NotificationAdminService {
    access: Arc<dyn NotificationAdminAccess>,
    store: Arc<dyn NotificationStore>,
}

impl NotificationAdminService {
    #[must_use]
    pub fn new(
        access: Arc<dyn NotificationAdminAccess>,
        store: Arc<dyn NotificationStore>,
    ) -> Self {
        Self { access, store }
    }

    pub async fn put_destination(
        &self,
        context: &AuthContext,
        request_id: RequestCorrelationId,
        destination: NotificationDestination,
    ) -> Result<(), NotificationError> {
        self.access
            .authorize(context, destination.project_id)
            .await?;
        let project_id = destination.project_id;
        let target_id = hex::encode(destination.id.as_bytes());
        self.store.upsert_destination(destination).await?;
        self.access
            .audit(
                context,
                request_id,
                project_id,
                AuditAction::NotificationDestinationUpserted,
                target_id,
            )
            .await
    }

    pub async fn put_rule(
        &self,
        context: &AuthContext,
        request_id: RequestCorrelationId,
        rule: AlertRule,
    ) -> Result<(), NotificationError> {
        rule.validate()
            .map_err(|_| NotificationError::InvalidData)?;
        self.access.authorize(context, rule.project_id).await?;
        let project_id = rule.project_id;
        let target_id = hex::encode(rule.id.as_bytes());
        self.store.upsert_rule(rule).await?;
        self.access
            .audit(
                context,
                request_id,
                project_id,
                AuditAction::AlertRuleUpserted,
                target_id,
            )
            .await
    }

    pub async fn destinations(
        &self,
        context: &AuthContext,
        project_id: metric_domain::ProjectId,
    ) -> Result<Vec<NotificationDestination>, NotificationError> {
        self.access.authorize(context, project_id).await?;
        self.store
            .list_destinations(project_id, EXPANSION_RULE_LIMIT)
            .await
            .map_err(Into::into)
    }

    pub async fn rules(
        &self,
        context: &AuthContext,
        project_id: metric_domain::ProjectId,
    ) -> Result<Vec<AlertRule>, NotificationError> {
        self.access.authorize(context, project_id).await?;
        self.store
            .list_rules(project_id, EXPANSION_RULE_LIMIT)
            .await
            .map_err(Into::into)
    }

    pub async fn delivery_history(
        &self,
        context: &AuthContext,
        project_id: metric_domain::ProjectId,
    ) -> Result<Vec<NotificationDelivery>, NotificationError> {
        self.access.authorize(context, project_id).await?;
        self.store
            .list_delivery_history(project_id, 100)
            .await
            .map_err(Into::into)
    }

    pub async fn enqueue_test(
        &self,
        context: &AuthContext,
        project_id: metric_domain::ProjectId,
        destination_id: metric_domain::notifications::NotificationDestinationId,
        request_id: &RequestCorrelationId,
        now: Timestamp,
    ) -> Result<NotificationDelivery, NotificationError> {
        self.access.authorize(context, project_id).await?;
        let destination_exists = self
            .store
            .list_destinations(project_id, EXPANSION_RULE_LIMIT)
            .await?
            .iter()
            .any(|destination| destination.id == destination_id && destination.enabled);
        if !destination_exists {
            return Err(NotificationError::InvalidData);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"metric/notification-test/v1");
        hasher.update(&project_id.get().to_be_bytes());
        hasher.update(&destination_id.as_bytes());
        hasher.update(request_id.as_str().as_bytes());
        let digest = hasher.finalize();
        let mut transition_bytes = [0; 16];
        transition_bytes.copy_from_slice(&digest.as_bytes()[..16]);
        let transition_id = IssueTransitionId::from_bytes(transition_bytes);
        let rule_id = metric_domain::notifications::AlertRuleId::from_bytes(transition_bytes);
        let id = notification_delivery_id(transition_id, rule_id, destination_id);
        let payload = NotificationPayload::new(
            serde_json::to_vec(&json!({
                "version": 1,
                "type": "test",
                "project_id": project_id.get(),
                "title": "Metric test notification",
                "occurred_at_ms": now.unix_millis(),
                "rule_id": hex::encode(rule_id.as_bytes()),
                "destination_id": hex::encode(destination_id.as_bytes()),
            }))
            .map_err(|_| NotificationError::InvalidData)?,
        )
        .map_err(|_| NotificationError::InvalidData)?;
        let delivery = NotificationDelivery {
            id,
            project_id,
            issue_id: IssueId::from_bytes(transition_bytes),
            transition_id,
            rule_id,
            action_id: destination_id,
            destination_id,
            payload,
            status: NotificationDeliveryStatus::Pending,
            attempts: 0,
            next_attempt_at: now,
            last_error: None,
            created_at: now,
            delivered_at: None,
            delete_at: None,
        };
        self.store.enqueue_delivery(delivery.clone()).await?;
        Ok(delivery)
    }
}

fn map_auth_error(error: AuthError) -> NotificationError {
    match error {
        AuthError::Forbidden | AuthError::InvalidCredentials | AuthError::InvalidCredential => {
            NotificationError::Forbidden
        }
        AuthError::ProjectDisabled => NotificationError::ProjectDisabled,
        AuthError::ProjectDeletionPending => NotificationError::ProjectDeletionPending,
        AuthError::ProjectPurging => NotificationError::ProjectPurging,
        AuthError::ProjectDeleted => NotificationError::ProjectDeleted,
        _ => NotificationError::StorageUnavailable,
    }
}

impl From<NotificationStoreError> for NotificationError {
    fn from(error: NotificationStoreError) -> Self {
        match error {
            NotificationStoreError::InvalidData => Self::InvalidData,
            NotificationStoreError::Unavailable => Self::StorageUnavailable,
        }
    }
}

pub struct NotificationDispatcher {
    store: Arc<dyn NotificationStore>,
    adapter: Arc<dyn NotificationDeliveryAdapter>,
    clock: Arc<dyn Clock>,
    config: NotificationConfig,
    wake: Arc<Notify>,
}

pub trait NotificationSignal: Send + Sync + 'static {
    fn notify_transition(&self);
}

impl NotificationDispatcher {
    pub fn new(
        store: Arc<dyn NotificationStore>,
        adapter: Arc<dyn NotificationDeliveryAdapter>,
        clock: Arc<dyn Clock>,
        config: NotificationConfig,
    ) -> Result<Self, NotificationError> {
        if config.queue_capacity == 0
            || config.worker_concurrency == 0
            || config.worker_concurrency > config.queue_capacity
            || config.transition_batch_size == 0
            || config.due_scan_limit == 0
            || config.poll_interval.is_zero()
            || config.attempt_timeout.is_zero()
            || config.attempt_lease <= config.attempt_timeout
            || config.max_attempts == 0
            || config.initial_delay.is_zero()
            || config.initial_delay > config.max_delay
            || config.delivered_retention.is_zero()
            || config.dead_retention < config.delivered_retention
        {
            return Err(NotificationError::InvalidConfiguration);
        }
        Ok(Self {
            store,
            adapter,
            clock,
            config,
            wake: Arc::new(Notify::new()),
        })
    }

    pub async fn expand_once(&self) -> Result<usize, NotificationError> {
        let transitions = self
            .store
            .pending_transitions(self.config.transition_batch_size)
            .await?;
        let count = transitions.len();
        for transition in transitions {
            self.expand_transition(transition).await?;
        }
        metrics::gauge!("metric_notification_transition_backlog_seen").set(count as f64);
        Ok(count)
    }

    async fn expand_transition(
        &self,
        transition: IssueNotificationTransition,
    ) -> Result<(), NotificationError> {
        let rules = self
            .store
            .matching_rules(transition.project_id, transition.kind, EXPANSION_RULE_LIMIT)
            .await?;
        let mut deliveries = Vec::new();
        let mut identities = BTreeSet::new();
        for rule in rules {
            rule.validate()
                .map_err(|_| NotificationError::InvalidData)?;
            for destination_id in rule.destination_ids.iter().copied() {
                let id =
                    notification_delivery_id(transition.transition_id, rule.id, destination_id);
                if !identities.insert(id) {
                    continue;
                }
                let payload = notification_payload(&transition, rule.id, destination_id)?;
                deliveries.push(NotificationDelivery {
                    id,
                    project_id: transition.project_id,
                    issue_id: transition.issue_id,
                    transition_id: transition.transition_id,
                    rule_id: rule.id,
                    action_id: destination_id,
                    destination_id,
                    payload,
                    status: NotificationDeliveryStatus::Pending,
                    attempts: 0,
                    next_attempt_at: transition.created_at,
                    last_error: None,
                    created_at: transition.created_at,
                    delivered_at: None,
                    delete_at: None,
                });
            }
        }
        self.store.expand_transition(transition, deliveries).await?;
        metrics::counter!("metric_notification_transitions_expanded_total").increment(1);
        Ok(())
    }

    pub async fn claim_once(
        &self,
    ) -> Result<Option<ClaimedNotificationDelivery>, NotificationError> {
        let now = self.clock.now();
        let lease_until = add_duration(now, self.config.attempt_lease)?;
        self.store
            .claim_due(now, lease_until, self.config.due_scan_limit)
            .await
            .map_err(Into::into)
    }

    pub async fn deliver_claim(
        &self,
        claim: ClaimedNotificationDelivery,
    ) -> Result<(), NotificationError> {
        let delivery_id = claim.delivery.id;
        let attempt = claim.attempt;
        if attempt > self.config.max_attempts {
            let now = self.clock.now();
            self.store
                .mark_dead(
                    delivery_id,
                    now,
                    add_duration(now, self.config.dead_retention)?,
                    "attempts_exhausted",
                )
                .await?;
            metrics::counter!("metric_notification_delivery_attempts_total", "outcome" => "exhausted")
                .increment(1);
            return Ok(());
        }
        let result = timeout(
            self.config.attempt_timeout,
            self.adapter.deliver(claim.clone()),
        )
        .await
        .unwrap_or(Err(NotificationDeliveryError::Timeout));
        let now = self.clock.now();
        match classify_result(result) {
            DeliveryDisposition::Delivered => {
                self.store
                    .mark_delivered(
                        delivery_id,
                        now,
                        add_duration(now, self.config.delivered_retention)?,
                    )
                    .await?;
                metrics::counter!("metric_notification_delivery_attempts_total", "outcome" => "delivered")
                    .increment(1);
            }
            DeliveryDisposition::Permanent(code) => {
                self.store
                    .mark_dead(
                        delivery_id,
                        now,
                        add_duration(now, self.config.dead_retention)?,
                        code,
                    )
                    .await?;
                metrics::counter!("metric_notification_delivery_attempts_total", "outcome" => "dead")
                    .increment(1);
            }
            DeliveryDisposition::Retryable(code, retry_after) => {
                if attempt >= self.config.max_attempts {
                    self.store
                        .mark_dead(
                            delivery_id,
                            now,
                            add_duration(now, self.config.dead_retention)?,
                            "attempts_exhausted",
                        )
                        .await?;
                    metrics::counter!("metric_notification_delivery_attempts_total", "outcome" => "exhausted")
                        .increment(1);
                } else {
                    let default_backoff = retry_delay(&claim, self.config);
                    let backoff = retry_after
                        .map(|value| value.min(self.config.max_delay).max(default_backoff))
                        .unwrap_or(default_backoff);
                    self.store
                        .schedule_retry(delivery_id, add_duration(now, backoff)?, code)
                        .await?;
                    metrics::counter!("metric_notification_delivery_attempts_total", "outcome" => "retry")
                        .increment(1);
                }
            }
        }
        Ok(())
    }

    pub fn start(
        self: Arc<Self>,
        shutdown: ShutdownSignal,
    ) -> Result<NotificationTask, NotificationError> {
        let (sender, receiver) = mpsc::channel(self.config.queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut joins = Vec::with_capacity(self.config.worker_concurrency + 1);
        for _ in 0..self.config.worker_concurrency {
            let service = Arc::clone(&self);
            let receiver = Arc::clone(&receiver);
            let shutdown = shutdown.clone();
            joins.push(tokio::spawn(async move {
                loop {
                    let claim = tokio::select! {
                        () = shutdown.cancelled() => break,
                        claim = async { receiver.lock().await.recv().await } => claim,
                    };
                    let Some(claim) = claim else {
                        break;
                    };
                    let _ = service.deliver_claim(claim).await;
                }
            }));
        }
        let service = Arc::clone(&self);
        joins.push(tokio::spawn(async move {
            let mut ticker = interval(service.config.poll_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    _ = ticker.tick() => {}
                    () = service.wake.notified() => {}
                }
                {
                    let _ = service.expand_once().await;
                    while sender.capacity() > 0 {
                        match service.claim_once().await {
                            Ok(Some(claim)) => {
                                if sender.send(claim).await.is_err() {
                                    break;
                                }
                            }
                            _ => break,
                        }
                    }
                }
            }
        }));
        Ok(NotificationTask { joins })
    }
}

impl NotificationSignal for NotificationDispatcher {
    fn notify_transition(&self) {
        self.wake.notify_one();
    }
}

pub struct NotificationTask {
    joins: Vec<JoinHandle<()>>,
}

pub struct AggregateAlertEvaluator {
    store: Arc<dyn NotificationStore>,
    explore: Arc<ExploreService>,
    clock: Arc<dyn Clock>,
    poll_interval: Duration,
    lease: Duration,
    limit: usize,
}

pub struct MonitorAlertEvaluator {
    store: Arc<dyn NotificationStore>,
    monitors: Arc<dyn MonitorStore>,
    clock: Arc<dyn Clock>,
    poll_interval: Duration,
    batch_size: usize,
}

impl MonitorAlertEvaluator {
    pub fn new(
        store: Arc<dyn NotificationStore>,
        monitors: Arc<dyn MonitorStore>,
        clock: Arc<dyn Clock>,
        poll_interval: Duration,
        batch_size: usize,
    ) -> Result<Self, NotificationError> {
        if poll_interval.is_zero() || batch_size == 0 || batch_size > 1_000 {
            return Err(NotificationError::InvalidConfiguration);
        }
        Ok(Self {
            store,
            monitors,
            clock,
            poll_interval,
            batch_size,
        })
    }

    pub async fn evaluate_once(&self) -> Result<usize, NotificationError> {
        let runs = self
            .monitors
            .pending_monitor_alerts(self.batch_size)
            .await
            .map_err(map_monitor_store_error)?;
        let now = self.clock.now();
        for run in &runs {
            let rules = self
                .store
                .list_rules(run.project_id, EXPANSION_RULE_LIMIT)
                .await?;
            for rule in rules.into_iter().filter(|rule| {
                rule.enabled
                    && rule.monitor.as_ref().is_some_and(|monitor| {
                        monitor.monitor_id == run.monitor_id
                            && (monitor.outcomes.contains(&run.status)
                                || (run.status
                                    == metric_domain::monitors::MonitorRunStatus::Success
                                    && run.http_status.is_some()
                                    && rule.threshold_met
                                    && monitor.notify_resolved))
                    })
            }) {
                let resolving = run.status == metric_domain::monitors::MonitorRunStatus::Success;
                self.expand_monitor_rule(&rule, run, now, resolving).await?;
            }
            self.monitors
                .mark_monitor_alert_evaluated(run.id, now)
                .await
                .map_err(map_monitor_store_error)?;
        }
        Ok(runs.len())
    }

    async fn expand_monitor_rule(
        &self,
        rule: &AlertRule,
        run: &MonitorRun,
        now: Timestamp,
        resolving: bool,
    ) -> Result<(), NotificationError> {
        let window_expired = rule.storm_window_started_at.is_none_or(|started| {
            now.unix_millis().saturating_sub(started.unix_millis()) >= 3_600_000
        });
        let window_started = if window_expired {
            now
        } else {
            rule.storm_window_started_at.unwrap_or(now)
        };
        let mut storm_count = if window_expired { 0 } else { rule.storm_count };
        let cooldown_elapsed = rule.last_triggered_at.is_none_or(|last| {
            now.unix_millis().saturating_sub(last.unix_millis())
                >= i64::from(rule.cooldown_minutes) * 60_000
        });
        if !resolving && (!cooldown_elapsed || storm_count >= rule.storm_limit_per_hour) {
            return Ok(());
        }
        if !resolving {
            storm_count = storm_count.saturating_add(1);
        }
        let transition_id = IssueTransitionId::from_bytes(run.id.as_bytes());
        let mut deliveries = Vec::with_capacity(rule.destination_ids.len());
        for destination_id in rule.destination_ids.iter().copied() {
            deliveries.push(NotificationDelivery {
                id: notification_delivery_id(transition_id, rule.id, destination_id),
                project_id: run.project_id,
                issue_id: IssueId::from_bytes(run.monitor_id.as_bytes()),
                transition_id,
                rule_id: rule.id,
                action_id: destination_id,
                destination_id,
                payload: monitor_payload(rule, run, now, resolving)?,
                status: NotificationDeliveryStatus::Pending,
                attempts: 0,
                next_attempt_at: now,
                last_error: None,
                created_at: now,
                delivered_at: None,
                delete_at: None,
            });
        }
        self.store
            .complete_monitor_alert(
                rule.id,
                now,
                window_started,
                storm_count,
                !resolving,
                deliveries,
            )
            .await?;
        Ok(())
    }

    pub fn start(self: Arc<Self>, shutdown: ShutdownSignal) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(self.poll_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return,
                    _ = ticker.tick() => {
                        if self.evaluate_once().await.is_err() {
                            metrics::counter!(
                                "metric_monitor_alert_evaluations_total",
                                "outcome" => "error"
                            ).increment(1);
                        }
                    }
                }
            }
        })
    }
}

fn monitor_payload(
    rule: &AlertRule,
    run: &MonitorRun,
    now: Timestamp,
    resolving: bool,
) -> Result<NotificationPayload, NotificationError> {
    NotificationPayload::new(
        serde_json::to_vec(&json!({
            "schema": 1,
            "kind": if run.http_status.is_some() || run.uptime_failure.is_some() { "uptime_monitor" } else { "cron_monitor" },
            "state": if resolving { "resolved" } else { "firing" },
            "rule": rule.name.as_str(),
            "monitor_id": run.monitor_id.to_string(),
            "run_id": run.id.to_string(),
            "status": run.status.as_str(),
            "http_status": run.http_status,
            "failure": run.uptime_failure.map(|value| value.as_str()),
            "started_at": run.started_at.unix_millis(),
            "detected_at": now.unix_millis(),
        }))
        .map_err(|_| NotificationError::InvalidData)?,
    )
    .map_err(|_| NotificationError::InvalidData)
}

fn map_monitor_store_error(error: SignalStoreError) -> NotificationError {
    match error {
        SignalStoreError::Unavailable | SignalStoreError::Capacity => {
            NotificationError::StorageUnavailable
        }
        _ => NotificationError::InvalidData,
    }
}

impl AggregateAlertEvaluator {
    pub fn new(
        store: Arc<dyn NotificationStore>,
        explore: Arc<ExploreService>,
        clock: Arc<dyn Clock>,
        poll_interval: Duration,
        lease: Duration,
        limit: usize,
    ) -> Result<Self, NotificationError> {
        if poll_interval.is_zero() || lease <= poll_interval || limit == 0 || limit > 1_000 {
            return Err(NotificationError::InvalidConfiguration);
        }
        Ok(Self {
            store,
            explore,
            clock,
            poll_interval,
            lease,
            limit,
        })
    }

    pub async fn evaluate_once(&self) -> Result<usize, NotificationError> {
        let mut completed = 0;
        for _ in 0..self.limit {
            let now = self.clock.now();
            let claimed_until = add_duration(now, self.lease)?;
            let Some(rule) = self
                .store
                .claim_due_aggregate_rule(now, claimed_until)
                .await?
            else {
                break;
            };
            self.evaluate_rule(rule, now, claimed_until).await?;
            completed += 1;
        }
        Ok(completed)
    }

    async fn evaluate_rule(
        &self,
        rule: AlertRule,
        now: Timestamp,
        claimed_until: Timestamp,
    ) -> Result<(), NotificationError> {
        let aggregate = rule
            .aggregate
            .as_ref()
            .ok_or(NotificationError::InvalidData)?;
        let from = add_duration_signed(now, -i64::from(aggregate.lookback_minutes) * 60_000)?;
        let mut predicates = Vec::new();
        for (field, value) in [
            (ExploreField::Environment, aggregate.environment.as_ref()),
            (ExploreField::Release, aggregate.release.as_ref()),
        ] {
            if let Some(value) = value {
                predicates.push(ExplorePredicate {
                    field,
                    op: ExplorePredicateOp::Exact,
                    value: Some(ExploreValue::String(value.as_str().into())),
                    upper: None,
                });
            }
        }
        let plan = self
            .explore
            .plan(
                rule.project_id,
                ExploreQuery {
                    dataset: aggregate.dataset,
                    from,
                    until: now,
                    predicates,
                    aggregates: vec![ExploreAggregate {
                        kind: ExploreAggregateKind::Count,
                        field: None,
                        alias: AGGREGATE_ALIAS.into(),
                    }],
                    group_by: Vec::new(),
                    interval: None,
                    cursor: None,
                    limit: 1,
                },
            )
            .map_err(|_| NotificationError::InvalidData)?;
        let result = self
            .explore
            .execute(plan)
            .await
            .map_err(|_| NotificationError::StorageUnavailable)?;
        let count = result
            .rows
            .first()
            .and_then(|row| row.values.get(AGGREGATE_ALIAS))
            .and_then(|value| match value {
                ExploreValue::Integer(value) => u64::try_from(*value).ok(),
                ExploreValue::Number(value) if value.is_finite() && *value >= 0.0 => {
                    Some(*value as u64)
                }
                _ => None,
            })
            .unwrap_or(0);
        let threshold_met = count >= aggregate.threshold;
        let window_expired = rule.storm_window_started_at.is_none_or(|started| {
            now.unix_millis().saturating_sub(started.unix_millis()) >= 3_600_000
        });
        let storm_window_started_at = if window_expired {
            Some(now)
        } else {
            rule.storm_window_started_at
        };
        let mut storm_count = if window_expired { 0 } else { rule.storm_count };
        let cooldown_elapsed = rule.last_triggered_at.is_none_or(|last| {
            now.unix_millis().saturating_sub(last.unix_millis())
                >= i64::from(rule.cooldown_minutes) * 60_000
        });
        let firing = threshold_met && cooldown_elapsed && storm_count < rule.storm_limit_per_hour;
        let resolving = !threshold_met && rule.threshold_met && aggregate.notify_resolved;
        let action = firing.then_some("aggregate_threshold").or_else(|| {
            resolving
                .then_some("aggregate_resolved")
                .filter(|_| storm_count < rule.storm_limit_per_hour)
        });
        let mut deliveries = Vec::new();
        let mut last_triggered_at = rule.last_triggered_at;
        if let Some(action) = action {
            storm_count = storm_count.saturating_add(1);
            last_triggered_at = Some(now);
            let transition_id =
                aggregate_transition_id(rule.id, rule.next_evaluation_at.unwrap_or(now), action);
            for destination_id in rule.destination_ids.iter().copied() {
                let id = notification_delivery_id(transition_id, rule.id, destination_id);
                deliveries.push(NotificationDelivery {
                    id,
                    project_id: rule.project_id,
                    issue_id: IssueId::from_bytes(transition_id.as_bytes()),
                    transition_id,
                    rule_id: rule.id,
                    action_id: destination_id,
                    destination_id,
                    payload: aggregate_payload(&rule, destination_id, action, count, now)?,
                    status: NotificationDeliveryStatus::Pending,
                    attempts: 0,
                    next_attempt_at: now,
                    last_error: None,
                    created_at: now,
                    delivered_at: None,
                    delete_at: None,
                });
            }
        }
        let next = add_duration(
            now,
            Duration::from_secs(u64::from(aggregate.evaluation_interval_minutes) * 60),
        )?;
        self.store
            .complete_aggregate_rule(
                rule.id,
                claimed_until,
                next,
                threshold_met,
                last_triggered_at,
                storm_window_started_at,
                storm_count,
                deliveries,
            )
            .await?;
        Ok(())
    }

    pub fn start(self: Arc<Self>, shutdown: ShutdownSignal) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(self.poll_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        let _ = self.evaluate_once().await;
                    }
                }
            }
        })
    }
}

fn aggregate_transition_id(
    rule_id: metric_domain::notifications::AlertRuleId,
    now: Timestamp,
    action: &str,
) -> IssueTransitionId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric/aggregate-alert/v1");
    hasher.update(&rule_id.as_bytes());
    hasher.update(&now.unix_millis().to_be_bytes());
    hasher.update(action.as_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    IssueTransitionId::from_bytes(bytes)
}

fn aggregate_payload(
    rule: &AlertRule,
    destination_id: metric_domain::notifications::NotificationDestinationId,
    action: &str,
    count: u64,
    now: Timestamp,
) -> Result<NotificationPayload, NotificationError> {
    let aggregate = rule
        .aggregate
        .as_ref()
        .ok_or(NotificationError::InvalidData)?;
    NotificationPayload::new(
        serde_json::to_vec(&json!({
            "version": 1,
            "type": action,
            "project_id": rule.project_id.get(),
            "title": rule.name.as_str(),
            "dataset": aggregate.dataset.as_str(),
            "count": count,
            "threshold": aggregate.threshold,
            "occurred_at_ms": now.unix_millis(),
            "rule_id": hex::encode(rule.id.as_bytes()),
            "destination_id": hex::encode(destination_id.as_bytes()),
        }))
        .map_err(|_| NotificationError::InvalidData)?,
    )
    .map_err(|_| NotificationError::InvalidData)
}

fn add_duration_signed(timestamp: Timestamp, millis: i64) -> Result<Timestamp, NotificationError> {
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_add(millis))
        .map_err(|_| NotificationError::InvalidData)
}

impl NotificationTask {
    pub fn abort_handles(&self) -> Vec<tokio::task::AbortHandle> {
        self.joins.iter().map(JoinHandle::abort_handle).collect()
    }

    pub async fn wait(self) {
        for join in self.joins {
            let _ = join.await;
        }
    }
}

enum DeliveryDisposition {
    Delivered,
    Retryable(&'static str, Option<Duration>),
    Permanent(&'static str),
}

fn classify_result(
    result: Result<NotificationDeliveryReceipt, NotificationDeliveryError>,
) -> DeliveryDisposition {
    match result {
        Ok(receipt) if (200..300).contains(&receipt.status) => DeliveryDisposition::Delivered,
        Ok(receipt) if matches!(receipt.status, 408 | 429) || receipt.status >= 500 => {
            DeliveryDisposition::Retryable("http_retryable", receipt.retry_after)
        }
        Ok(_) => DeliveryDisposition::Permanent("http_rejected"),
        Err(NotificationDeliveryError::Retryable) => {
            DeliveryDisposition::Retryable("network_error", None)
        }
        Err(NotificationDeliveryError::Timeout) => DeliveryDisposition::Retryable("timeout", None),
        Err(NotificationDeliveryError::ResponseTooLarge) => {
            DeliveryDisposition::Retryable("response_too_large", None)
        }
        Err(NotificationDeliveryError::Rejected) => {
            DeliveryDisposition::Permanent("provider_rejected")
        }
        Err(NotificationDeliveryError::InvalidSecret) => {
            DeliveryDisposition::Permanent("invalid_secret")
        }
    }
}

fn notification_payload(
    transition: &IssueNotificationTransition,
    rule_id: metric_domain::notifications::AlertRuleId,
    destination_id: metric_domain::notifications::NotificationDestinationId,
) -> Result<NotificationPayload, NotificationError> {
    let kind = match transition.kind {
        metric_domain::issue::IssueNotificationKind::NewIssue => "new_issue",
        metric_domain::issue::IssueNotificationKind::Regression => "regression",
        metric_domain::issue::IssueNotificationKind::Resolved => "resolved",
    };
    let bytes = serde_json::to_vec(&json!({
        "version": 1,
        "type": kind,
        "transition_id": hex::encode(transition.transition_id.as_bytes()),
        "project_id": transition.project_id.get(),
        "issue_id": hex::encode(transition.issue_id.as_bytes()),
        "event_id": transition.event_id.to_string(),
        "title": transition.title.as_str(),
        "occurred_at_ms": transition.created_at.unix_millis(),
        "rule_id": hex::encode(rule_id.as_bytes()),
        "destination_id": hex::encode(destination_id.as_bytes()),
    }))
    .map_err(|_| NotificationError::InvalidData)?;
    NotificationPayload::new(bytes).map_err(|_| NotificationError::InvalidData)
}

fn retry_delay(claim: &ClaimedNotificationDelivery, config: NotificationConfig) -> Duration {
    let multiplier = match claim.attempt {
        0 | 1 => 1,
        2 => 6,
        3 => 24,
        4 => 120,
        5 => 360,
        _ => 720,
    };
    let base = config
        .initial_delay
        .checked_mul(multiplier)
        .unwrap_or(config.max_delay)
        .min(config.max_delay);
    let digest = blake3::hash(
        &[
            claim.delivery.id.as_bytes().as_slice(),
            &claim.attempt.to_be_bytes(),
        ]
        .concat(),
    );
    let jitter = u16::from_be_bytes([digest.as_bytes()[0], digest.as_bytes()[1]]) as u128;
    let factor_milli = 800_u128 + jitter * 400 / u16::MAX as u128;
    Duration::from_millis(
        u64::try_from(base.as_millis().saturating_mul(factor_milli) / 1_000).unwrap_or(u64::MAX),
    )
    .min(config.max_delay)
}

fn add_duration(timestamp: Timestamp, duration: Duration) -> Result<Timestamp, NotificationError> {
    let millis = i64::try_from(duration.as_millis()).map_err(|_| NotificationError::InvalidData)?;
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_add(millis))
        .map_err(|_| NotificationError::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;
    use metric_domain::{
        EventId, ProjectId,
        grouping::IssueId,
        issue::IssueTransitionId,
        notifications::{
            AlertRuleId, NotificationDeliveryId, NotificationDestination,
            NotificationDestinationId, SealedWebhookSecret, WebhookEndpoint,
        },
    };

    #[test]
    fn retryable_and_terminal_statuses_are_closed() {
        for status in [200, 202, 204] {
            assert!(matches!(
                classify_result(Ok(NotificationDeliveryReceipt {
                    status,
                    retry_after: None
                })),
                DeliveryDisposition::Delivered
            ));
        }
        for status in [408, 429, 500, 503] {
            assert!(matches!(
                classify_result(Ok(NotificationDeliveryReceipt {
                    status,
                    retry_after: None
                })),
                DeliveryDisposition::Retryable(_, _)
            ));
        }
        for status in [300, 301, 400, 401, 404] {
            assert!(matches!(
                classify_result(Ok(NotificationDeliveryReceipt {
                    status,
                    retry_after: None
                })),
                DeliveryDisposition::Permanent(_)
            ));
        }
    }

    #[test]
    fn deterministic_jitter_is_bounded_and_attempt_scoped() {
        let mut claim = claim();
        let config = NotificationConfig::default();
        let first = retry_delay(&claim, config);
        assert!(first >= Duration::from_secs(4));
        assert!(first <= Duration::from_secs(6));
        assert_eq!(first, retry_delay(&claim, config));
        claim.attempt = 2;
        assert!(retry_delay(&claim, config) > first);
    }

    fn claim() -> ClaimedNotificationDelivery {
        let project_id = ProjectId::new(1).unwrap();
        let destination_id = NotificationDestinationId::from_bytes([3; 16]);
        ClaimedNotificationDelivery {
            delivery: NotificationDelivery {
                id: NotificationDeliveryId::from_bytes([1; 16]),
                project_id,
                issue_id: IssueId::from_bytes([2; 16]),
                transition_id: IssueTransitionId::from_bytes([4; 16]),
                rule_id: AlertRuleId::from_bytes([5; 16]),
                action_id: destination_id,
                destination_id,
                payload: NotificationPayload::new(br#"{"version":1}"#.to_vec()).unwrap(),
                status: NotificationDeliveryStatus::Pending,
                attempts: 1,
                next_attempt_at: Timestamp::from_unix_millis(1).unwrap(),
                last_error: None,
                created_at: Timestamp::from_unix_millis(1).unwrap(),
                delivered_at: None,
                delete_at: None,
            },
            destination: NotificationDestination {
                id: destination_id,
                project_id,
                kind: metric_domain::notifications::NotificationDestinationKind::Webhook,
                endpoint: WebhookEndpoint::new("https://example.com/hook").unwrap(),
                sealed_secret: SealedWebhookSecret::new(vec![1; 32]).unwrap(),
                smtp: None,
                enabled: true,
                created_at: Timestamp::from_unix_millis(1).unwrap(),
                updated_at: Timestamp::from_unix_millis(1).unwrap(),
            },
            attempt: 1,
            attempted_at: Timestamp::from_unix_millis(1).unwrap(),
        }
    }

    #[test]
    fn payload_has_only_stable_bounded_fields() {
        let transition = IssueNotificationTransition {
            transition_id: IssueTransitionId::from_bytes([1; 16]),
            project_id: ProjectId::new(1).unwrap(),
            issue_id: IssueId::from_bytes([2; 16]),
            kind: metric_domain::issue::IssueNotificationKind::Regression,
            event_id: EventId::from_bytes([3; 16]),
            created_at: Timestamp::from_unix_millis(4).unwrap(),
            title: metric_domain::issue::IssueTitle::new("failure").unwrap(),
        };
        let payload = notification_payload(
            &transition,
            AlertRuleId::from_bytes([4; 16]),
            NotificationDestinationId::from_bytes([5; 16]),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(payload.as_bytes()).unwrap();
        assert_eq!(value["type"], "regression");
        assert_eq!(value["version"], 1);
        assert_eq!(value.as_object().unwrap().len(), 10);
    }

    #[test]
    fn aggregate_delivery_identity_is_scheduled_window_scoped() {
        let rule = AlertRuleId::from_bytes([7; 16]);
        let scheduled = Timestamp::from_unix_millis(60_000).unwrap();
        assert_eq!(
            aggregate_transition_id(rule, scheduled, "aggregate_threshold"),
            aggregate_transition_id(rule, scheduled, "aggregate_threshold")
        );
        assert_ne!(
            aggregate_transition_id(rule, scheduled, "aggregate_threshold"),
            aggregate_transition_id(
                rule,
                Timestamp::from_unix_millis(120_000).unwrap(),
                "aggregate_threshold"
            )
        );
        assert_ne!(
            aggregate_transition_id(rule, scheduled, "aggregate_threshold"),
            aggregate_transition_id(rule, scheduled, "aggregate_resolved")
        );
    }
}
