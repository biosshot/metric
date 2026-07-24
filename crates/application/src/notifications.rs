//! Durable notification transition expansion and bounded webhook delivery.

use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc, time::Duration};

use crate::{
    auth::{AuthError, IdentityService},
    shutdown::ShutdownSignal,
};
use faultkeep_domain::{
    Timestamp,
    auth::{AuditAction, AuthContext, Permission, RequestCorrelationId},
    notifications::{
        AlertRule, ClaimedNotificationDelivery, IssueNotificationTransition, NotificationDelivery,
        NotificationDeliveryStatus, NotificationDestination, NotificationPayload,
        notification_delivery_id,
    },
};
use faultkeep_ports::{
    Clock, NotificationStore, NotificationStoreError, WebhookDeliveryAdapter, WebhookDeliveryError,
    WebhookDeliveryReceipt,
};
use serde_json::json;
use thiserror::Error;
use tokio::{
    sync::{Mutex, Notify, mpsc},
    task::JoinHandle,
    time::{MissedTickBehavior, interval, timeout},
};

const EXPANSION_RULE_LIMIT: usize = 256;

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
}

impl NotificationError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "notification_invalid_configuration",
            Self::StorageUnavailable => "notification_temporarily_unavailable",
            Self::InvalidData => "notification_invalid_data",
            Self::Forbidden => "notification_forbidden",
        }
    }
}

pub type NotificationAccessFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), NotificationError>> + Send + 'a>>;

pub trait NotificationAdminAccess: Send + Sync + 'static {
    fn authorize<'a>(
        &'a self,
        context: &'a AuthContext,
        project_id: faultkeep_domain::ProjectId,
    ) -> NotificationAccessFuture<'a>;

    fn audit<'a>(
        &'a self,
        context: &'a AuthContext,
        request_id: RequestCorrelationId,
        project_id: faultkeep_domain::ProjectId,
        action: AuditAction,
        target_id: String,
    ) -> NotificationAccessFuture<'a>;
}

impl NotificationAdminAccess for IdentityService {
    fn authorize<'a>(
        &'a self,
        context: &'a AuthContext,
        project_id: faultkeep_domain::ProjectId,
    ) -> NotificationAccessFuture<'a> {
        Box::pin(async move {
            self.authorize_project(context, project_id, Permission::ProjectAdmin)
                .await
                .map_err(map_auth_error)
        })
    }

    fn audit<'a>(
        &'a self,
        context: &'a AuthContext,
        request_id: RequestCorrelationId,
        project_id: faultkeep_domain::ProjectId,
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
}

fn map_auth_error(error: AuthError) -> NotificationError {
    match error {
        AuthError::Forbidden | AuthError::InvalidCredentials | AuthError::InvalidCredential => {
            NotificationError::Forbidden
        }
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
    adapter: Arc<dyn WebhookDeliveryAdapter>,
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
        adapter: Arc<dyn WebhookDeliveryAdapter>,
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
        metrics::gauge!("faultkeep_notification_transition_backlog_seen").set(count as f64);
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
        metrics::counter!("faultkeep_notification_transitions_expanded_total").increment(1);
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
            metrics::counter!("faultkeep_notification_delivery_attempts_total", "outcome" => "exhausted")
                .increment(1);
            return Ok(());
        }
        let result = timeout(
            self.config.attempt_timeout,
            self.adapter.deliver(claim.clone()),
        )
        .await
        .unwrap_or(Err(WebhookDeliveryError::Timeout));
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
                metrics::counter!("faultkeep_notification_delivery_attempts_total", "outcome" => "delivered")
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
                metrics::counter!("faultkeep_notification_delivery_attempts_total", "outcome" => "dead")
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
                    metrics::counter!("faultkeep_notification_delivery_attempts_total", "outcome" => "exhausted")
                        .increment(1);
                } else {
                    let default_backoff = retry_delay(&claim, self.config);
                    let backoff = retry_after
                        .map(|value| value.min(self.config.max_delay).max(default_backoff))
                        .unwrap_or(default_backoff);
                    self.store
                        .schedule_retry(delivery_id, add_duration(now, backoff)?, code)
                        .await?;
                    metrics::counter!("faultkeep_notification_delivery_attempts_total", "outcome" => "retry")
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

impl NotificationTask {
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
    result: Result<WebhookDeliveryReceipt, WebhookDeliveryError>,
) -> DeliveryDisposition {
    match result {
        Ok(receipt) if (200..300).contains(&receipt.status) => DeliveryDisposition::Delivered,
        Ok(receipt) if matches!(receipt.status, 408 | 429) || receipt.status >= 500 => {
            DeliveryDisposition::Retryable("http_retryable", receipt.retry_after)
        }
        Ok(_) => DeliveryDisposition::Permanent("http_rejected"),
        Err(WebhookDeliveryError::Retryable) => {
            DeliveryDisposition::Retryable("network_error", None)
        }
        Err(WebhookDeliveryError::Timeout) => DeliveryDisposition::Retryable("timeout", None),
        Err(WebhookDeliveryError::ResponseTooLarge) => {
            DeliveryDisposition::Retryable("response_too_large", None)
        }
        Err(WebhookDeliveryError::Rejected) => DeliveryDisposition::Permanent("ssrf_rejected"),
        Err(WebhookDeliveryError::InvalidSecret) => {
            DeliveryDisposition::Permanent("invalid_secret")
        }
    }
}

fn notification_payload(
    transition: &IssueNotificationTransition,
    rule_id: faultkeep_domain::notifications::AlertRuleId,
    destination_id: faultkeep_domain::notifications::NotificationDestinationId,
) -> Result<NotificationPayload, NotificationError> {
    let kind = match transition.kind {
        faultkeep_domain::issue::IssueNotificationKind::NewIssue => "new_issue",
        faultkeep_domain::issue::IssueNotificationKind::Regression => "regression",
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
    use faultkeep_domain::{
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
                classify_result(Ok(WebhookDeliveryReceipt {
                    status,
                    retry_after: None
                })),
                DeliveryDisposition::Delivered
            ));
        }
        for status in [408, 429, 500, 503] {
            assert!(matches!(
                classify_result(Ok(WebhookDeliveryReceipt {
                    status,
                    retry_after: None
                })),
                DeliveryDisposition::Retryable(_, _)
            ));
        }
        for status in [300, 301, 400, 401, 404] {
            assert!(matches!(
                classify_result(Ok(WebhookDeliveryReceipt {
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
                endpoint: WebhookEndpoint::new("https://example.com/hook").unwrap(),
                sealed_secret: SealedWebhookSecret::new(vec![1; 32]).unwrap(),
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
            kind: faultkeep_domain::issue::IssueNotificationKind::Regression,
            event_id: EventId::from_bytes([3; 16]),
            created_at: Timestamp::from_unix_millis(4).unwrap(),
            title: faultkeep_domain::issue::IssueTitle::new("failure").unwrap(),
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
}
