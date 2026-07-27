use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::TryStreamExt;
use metric_domain::{
    EventId, ProjectId, Timestamp,
    explore::ExploreDataset,
    grouping::IssueId,
    issue::{IssueNotificationKind, IssueTitle, IssueTransitionId},
    monitors::{MonitorId, MonitorRunStatus},
    notifications::{
        AggregateAlert, AlertRule, AlertRuleId, ClaimedNotificationDelivery,
        IssueNotificationTransition, MAX_EMAIL_ADDRESS_BYTES, MonitorAlert, NotificationDelivery,
        NotificationDeliveryId, NotificationDeliveryStatus, NotificationDestination,
        NotificationDestinationId, NotificationDestinationKind, NotificationPayload,
        NotificationText, RuleName, SealedWebhookSecret, SmtpDestination, SmtpSecurity,
        WebhookEndpoint,
    },
};
use metric_ports::{NotificationStore, NotificationStoreError, PortFuture};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::{IndexOptions, ReturnDocument},
};

type FairnessKey = (i32, [u8; 16]);
type DueCandidate = ([u8; 16], i32, [u8; 16], Timestamp);

#[derive(Clone)]
pub struct MongoNotificationStore {
    database: Database,
    fairness_cursor: Arc<Mutex<Option<FairnessKey>>>,
}

impl MongoNotificationStore {
    #[must_use]
    pub fn from_database(database: Database) -> Self {
        Self {
            database,
            fairness_cursor: Arc::new(Mutex::new(None)),
        }
    }

    async fn pending_transitions_inner(
        &self,
        limit: usize,
    ) -> Result<Vec<IssueNotificationTransition>, NotificationStoreError> {
        if !(1..=10_000).contains(&limit) {
            return Err(NotificationStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("issues")
            .find(doc! { "j": true })
            .projection(doc! { "_id": 1, "p": 1, "t": 1, "n": 1 })
            .sort(doc! { "_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(i64::MAX))
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        let mut transitions = Vec::with_capacity(limit);
        while let Some(issue) = cursor
            .try_next()
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?
        {
            decode_issue_transitions(&issue, &mut transitions, limit)?;
            if transitions.len() == limit {
                break;
            }
        }
        Ok(transitions)
    }

    async fn matching_rules_inner(
        &self,
        project_id: ProjectId,
        kind: IssueNotificationKind,
        limit: usize,
    ) -> Result<Vec<AlertRule>, NotificationStoreError> {
        if !(1..=1_000).contains(&limit) {
            return Err(NotificationStoreError::InvalidData);
        }
        let trigger = trigger_name(kind);
        let mut cursor = self
            .database
            .collection::<Document>("alert_rules")
            .find(doc! {
                "p": project_id.get(),
                "e": true,
                "k": trigger,
            })
            .sort(doc! { "_id": 1 })
            .limit(i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX))
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        let mut rules = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?
        {
            rules.push(decode_rule(&document)?);
        }
        if rules.len() > limit {
            return Err(NotificationStoreError::InvalidData);
        }
        Ok(rules)
    }

    async fn expand_transition_inner(
        &self,
        transition: IssueNotificationTransition,
        deliveries: Vec<NotificationDelivery>,
    ) -> Result<(), NotificationStoreError> {
        let collection = self
            .database
            .collection::<Document>("notification_deliveries");
        for delivery in deliveries {
            if delivery.transition_id != transition.transition_id
                || delivery.project_id != transition.project_id
                || delivery.issue_id != transition.issue_id
            {
                return Err(NotificationStoreError::InvalidData);
            }
            let document = encode_delivery(&delivery)?;
            collection
                .update_one(
                    doc! { "_id": binary(delivery.id.as_bytes()) },
                    doc! { "$setOnInsert": document },
                )
                .upsert(true)
                .await
                .map_err(|_| NotificationStoreError::Unavailable)?;
        }

        let issues = self.database.collection::<Document>("issues");
        issues
            .update_one(
                doc! {
                    "_id": binary(transition.issue_id.as_bytes()),
                    "p": transition.project_id.get(),
                    "n.i": binary(transition.transition_id.as_bytes()),
                },
                vec![
                    doc! { "$set": {
                        "n": { "$filter": {
                            "input": "$n",
                            "as": "transition",
                            "cond": { "$ne": ["$$transition.i", binary(transition.transition_id.as_bytes())] },
                        } },
                    } },
                    doc! { "$set": {
                        "j": { "$cond": [
                            { "$gt": [{ "$size": "$n" }, 0_i32] },
                            true,
                            "$$REMOVE",
                        ] },
                        "n": { "$cond": [
                            { "$gt": [{ "$size": "$n" }, 0_i32] },
                            "$n",
                            "$$REMOVE",
                        ] },
                    } },
                ],
            )
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        Ok(())
    }

    async fn claim_due_inner(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        scan_limit: usize,
    ) -> Result<Option<ClaimedNotificationDelivery>, NotificationStoreError> {
        if !(1..=1_000).contains(&scan_limit) || lease_until <= now {
            return Err(NotificationStoreError::InvalidData);
        }
        let deliveries = self
            .database
            .collection::<Document>("notification_deliveries");
        let mut cursor = deliveries
            .find(doc! {
                "s": "pending",
                "n": { "$lte": date(now) },
            })
            .projection(doc! { "_id": 1, "p": 1, "d": 1, "n": 1 })
            .sort(doc! { "n": 1, "_id": 1 })
            .limit(i64::try_from(scan_limit).unwrap_or(i64::MAX))
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        let mut candidates = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?
        {
            candidates.push((
                id16(&document, "_id")?,
                document
                    .get_i32("p")
                    .map_err(|_| NotificationStoreError::InvalidData)?,
                id16(&document, "d")?,
                timestamp(&document, "n")?,
            ));
        }
        let last = *self
            .fairness_cursor
            .lock()
            .map_err(|_| NotificationStoreError::Unavailable)?;
        let candidate = choose_fair_candidate(&candidates, last);
        let Some((id, project_id, destination_id, expected_due)) = candidate else {
            return Ok(None);
        };
        let claimed = deliveries
            .find_one_and_update(
                doc! {
                    "_id": binary(id),
                    "s": "pending",
                    "n": date(expected_due),
                },
                doc! {
                    "$inc": { "a": 1_i64 },
                    "$set": { "n": date(lease_until), "y": date(now) },
                },
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        let Some(claimed) = claimed else {
            return Ok(None);
        };
        *self
            .fairness_cursor
            .lock()
            .map_err(|_| NotificationStoreError::Unavailable)? = Some((project_id, destination_id));
        let destination = self
            .database
            .collection::<Document>("notification_destinations")
            .find_one(doc! { "_id": binary(destination_id) })
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?
            .ok_or(NotificationStoreError::InvalidData)
            .and_then(|document| decode_destination(&document))?;
        let delivery = decode_delivery(&claimed)?;
        let attempt = delivery.attempts;
        Ok(Some(ClaimedNotificationDelivery {
            delivery,
            destination,
            attempt,
            attempted_at: now,
        }))
    }

    async fn mark_delivered_inner(
        &self,
        id: NotificationDeliveryId,
        delivered_at: Timestamp,
        delete_at: Timestamp,
    ) -> Result<(), NotificationStoreError> {
        terminal_update(
            &self.database,
            id,
            "delivered",
            delivered_at,
            delete_at,
            None,
        )
        .await
    }

    async fn schedule_retry_inner(
        &self,
        id: NotificationDeliveryId,
        next_attempt_at: Timestamp,
        error_code: &'static str,
    ) -> Result<(), NotificationStoreError> {
        validate_error(error_code)?;
        self.database
            .collection::<Document>("notification_deliveries")
            .update_one(
                doc! { "_id": binary(id.as_bytes()), "s": "pending" },
                doc! { "$set": { "n": date(next_attempt_at), "z": error_code }, "$unset": { "y": "" } },
            )
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        Ok(())
    }

    async fn mark_dead_inner(
        &self,
        id: NotificationDeliveryId,
        dead_at: Timestamp,
        delete_at: Timestamp,
        error_code: &'static str,
    ) -> Result<(), NotificationStoreError> {
        validate_error(error_code)?;
        terminal_update(
            &self.database,
            id,
            "dead",
            dead_at,
            delete_at,
            Some(error_code),
        )
        .await
    }

    async fn upsert_destination_inner(
        &self,
        destination: NotificationDestination,
    ) -> Result<(), NotificationStoreError> {
        destination
            .validate()
            .map_err(|_| NotificationStoreError::InvalidData)?;
        self.database
            .collection::<Document>("notification_destinations")
            .replace_one(
                doc! { "_id": binary(destination.id.as_bytes()) },
                encode_destination(&destination),
            )
            .upsert(true)
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        Ok(())
    }

    async fn upsert_rule_inner(&self, rule: AlertRule) -> Result<(), NotificationStoreError> {
        rule.validate()
            .map_err(|_| NotificationStoreError::InvalidData)?;
        self.database
            .collection::<Document>("alert_rules")
            .replace_one(
                doc! { "_id": binary(rule.id.as_bytes()) },
                encode_rule(&rule),
            )
            .upsert(true)
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        Ok(())
    }

    async fn list_destinations_inner(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<NotificationDestination>, NotificationStoreError> {
        if !(1..=1_000).contains(&limit) {
            return Err(NotificationStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("notification_destinations")
            .find(doc! { "p": project_id.get() })
            .sort(doc! { "c": 1, "_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(i64::MAX))
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        let mut values = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?
        {
            values.push(decode_destination(&document)?);
        }
        Ok(values)
    }

    async fn list_rules_inner(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<AlertRule>, NotificationStoreError> {
        if !(1..=1_000).contains(&limit) {
            return Err(NotificationStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("alert_rules")
            .find(doc! { "p": project_id.get() })
            .sort(doc! { "c": 1, "_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(i64::MAX))
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        let mut values = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?
        {
            values.push(decode_rule(&document)?);
        }
        Ok(values)
    }

    async fn claim_due_aggregate_rule_inner(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
    ) -> Result<Option<AlertRule>, NotificationStoreError> {
        if lease_until <= now {
            return Err(NotificationStoreError::InvalidData);
        }
        self.database
            .collection::<Document>("alert_rules")
            .find_one_and_update(
                doc! { "e": true, "g": { "$exists": true }, "x": { "$lte": date(now) } },
                vec![doc! { "$set": { "y": "$x", "x": date(lease_until) } }],
            )
            .sort(doc! { "x": 1, "_id": 1 })
            .return_document(ReturnDocument::After)
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?
            .map(|document| decode_rule(&document))
            .transpose()
    }

    #[allow(clippy::too_many_arguments)]
    async fn complete_aggregate_rule_inner(
        &self,
        rule_id: AlertRuleId,
        claimed_until: Timestamp,
        next_evaluation_at: Timestamp,
        threshold_met: bool,
        last_triggered_at: Option<Timestamp>,
        storm_window_started_at: Option<Timestamp>,
        storm_count: u32,
        deliveries: Vec<NotificationDelivery>,
    ) -> Result<(), NotificationStoreError> {
        let collection = self
            .database
            .collection::<Document>("notification_deliveries");
        for delivery in deliveries {
            collection
                .update_one(
                    doc! { "_id": binary(delivery.id.as_bytes()) },
                    doc! { "$setOnInsert": encode_delivery(&delivery)? },
                )
                .upsert(true)
                .await
                .map_err(|_| NotificationStoreError::Unavailable)?;
        }
        let mut set = doc! {
            "x": date(next_evaluation_at),
            "tm": threshold_met,
            "sc": i64::from(storm_count),
        };
        let mut unset = Document::new();
        for (name, value) in [("lt", last_triggered_at), ("sw", storm_window_started_at)] {
            if let Some(value) = value {
                set.insert(name, date(value));
            } else {
                unset.insert(name, "");
            }
        }
        unset.insert("y", "");
        let mut update = doc! { "$set": set };
        if !unset.is_empty() {
            update.insert("$unset", unset);
        }
        self.database
            .collection::<Document>("alert_rules")
            .update_one(
                doc! { "_id": binary(rule_id.as_bytes()), "x": date(claimed_until) },
                update,
            )
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        Ok(())
    }

    async fn enqueue_delivery_inner(
        &self,
        delivery: NotificationDelivery,
    ) -> Result<(), NotificationStoreError> {
        self.database
            .collection::<Document>("notification_deliveries")
            .update_one(
                doc! { "_id": binary(delivery.id.as_bytes()) },
                doc! { "$setOnInsert": encode_delivery(&delivery)? },
            )
            .upsert(true)
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        Ok(())
    }

    async fn complete_monitor_alert_inner(
        &self,
        rule_id: AlertRuleId,
        last_triggered_at: Timestamp,
        storm_window_started_at: Timestamp,
        storm_count: u32,
        threshold_met: bool,
        deliveries: Vec<NotificationDelivery>,
    ) -> Result<(), NotificationStoreError> {
        for delivery in deliveries {
            self.enqueue_delivery_inner(delivery).await?;
        }
        self.database
            .collection::<Document>("alert_rules")
            .update_one(
                doc! { "_id": binary(rule_id.as_bytes()) },
                doc! { "$set": {
                    "lt": date(last_triggered_at),
                    "sw": date(storm_window_started_at),
                    "sc": i64::from(storm_count),
                    "tm": threshold_met,
                    "u": date(last_triggered_at),
                }},
            )
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        Ok(())
    }

    async fn list_delivery_history_inner(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<NotificationDelivery>, NotificationStoreError> {
        if !(1..=500).contains(&limit) {
            return Err(NotificationStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("notification_deliveries")
            .find(doc! { "p": project_id.get() })
            .sort(doc! { "c": -1, "_id": -1 })
            .limit(i64::try_from(limit).unwrap_or(i64::MAX))
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?;
        let mut values = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| NotificationStoreError::Unavailable)?
        {
            values.push(decode_delivery(&document)?);
        }
        Ok(values)
    }
}

fn choose_fair_candidate(
    candidates: &[DueCandidate],
    last: Option<FairnessKey>,
) -> Option<DueCandidate> {
    candidates
        .iter()
        .find(|(_, project, destination, _)| {
            last.is_none_or(|(last_project, last_destination)| {
                *project != last_project && *destination != last_destination
            })
        })
        .or_else(|| {
            candidates.iter().find(|(_, project, _, _)| {
                last.is_none_or(|(last_project, _)| *project != last_project)
            })
        })
        .or_else(|| {
            candidates.iter().find(|(_, _, destination, _)| {
                last.is_none_or(|(_, last_destination)| *destination != last_destination)
            })
        })
        .or_else(|| candidates.first())
        .copied()
}

impl NotificationStore for MongoNotificationStore {
    fn pending_transitions(
        &self,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<IssueNotificationTransition>, NotificationStoreError>> {
        Box::pin(self.pending_transitions_inner(limit))
    }

    fn matching_rules(
        &self,
        project_id: ProjectId,
        kind: IssueNotificationKind,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<AlertRule>, NotificationStoreError>> {
        Box::pin(self.matching_rules_inner(project_id, kind, limit))
    }

    fn expand_transition(
        &self,
        transition: IssueNotificationTransition,
        deliveries: Vec<NotificationDelivery>,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>> {
        Box::pin(self.expand_transition_inner(transition, deliveries))
    }

    fn claim_due(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        scan_limit: usize,
    ) -> PortFuture<'_, Result<Option<ClaimedNotificationDelivery>, NotificationStoreError>> {
        Box::pin(self.claim_due_inner(now, lease_until, scan_limit))
    }

    fn mark_delivered(
        &self,
        delivery_id: NotificationDeliveryId,
        delivered_at: Timestamp,
        delete_at: Timestamp,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>> {
        Box::pin(self.mark_delivered_inner(delivery_id, delivered_at, delete_at))
    }

    fn schedule_retry(
        &self,
        delivery_id: NotificationDeliveryId,
        next_attempt_at: Timestamp,
        error_code: &'static str,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>> {
        Box::pin(self.schedule_retry_inner(delivery_id, next_attempt_at, error_code))
    }

    fn mark_dead(
        &self,
        delivery_id: NotificationDeliveryId,
        dead_at: Timestamp,
        delete_at: Timestamp,
        error_code: &'static str,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>> {
        Box::pin(self.mark_dead_inner(delivery_id, dead_at, delete_at, error_code))
    }

    fn upsert_destination(
        &self,
        destination: NotificationDestination,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>> {
        Box::pin(self.upsert_destination_inner(destination))
    }

    fn upsert_rule(&self, rule: AlertRule) -> PortFuture<'_, Result<(), NotificationStoreError>> {
        Box::pin(self.upsert_rule_inner(rule))
    }

    fn list_destinations(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<NotificationDestination>, NotificationStoreError>> {
        Box::pin(self.list_destinations_inner(project_id, limit))
    }

    fn list_rules(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<AlertRule>, NotificationStoreError>> {
        Box::pin(self.list_rules_inner(project_id, limit))
    }

    fn claim_due_aggregate_rule(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
    ) -> PortFuture<'_, Result<Option<AlertRule>, NotificationStoreError>> {
        Box::pin(self.claim_due_aggregate_rule_inner(now, lease_until))
    }

    fn complete_aggregate_rule(
        &self,
        rule_id: AlertRuleId,
        claimed_until: Timestamp,
        next_evaluation_at: Timestamp,
        threshold_met: bool,
        last_triggered_at: Option<Timestamp>,
        storm_window_started_at: Option<Timestamp>,
        storm_count: u32,
        deliveries: Vec<NotificationDelivery>,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>> {
        Box::pin(self.complete_aggregate_rule_inner(
            rule_id,
            claimed_until,
            next_evaluation_at,
            threshold_met,
            last_triggered_at,
            storm_window_started_at,
            storm_count,
            deliveries,
        ))
    }

    fn enqueue_delivery(
        &self,
        delivery: NotificationDelivery,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>> {
        Box::pin(self.enqueue_delivery_inner(delivery))
    }

    fn complete_monitor_alert(
        &self,
        rule_id: AlertRuleId,
        last_triggered_at: Timestamp,
        storm_window_started_at: Timestamp,
        storm_count: u32,
        threshold_met: bool,
        deliveries: Vec<NotificationDelivery>,
    ) -> PortFuture<'_, Result<(), NotificationStoreError>> {
        Box::pin(self.complete_monitor_alert_inner(
            rule_id,
            last_triggered_at,
            storm_window_started_at,
            storm_count,
            threshold_met,
            deliveries,
        ))
    }

    fn list_delivery_history(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<NotificationDelivery>, NotificationStoreError>> {
        Box::pin(self.list_delivery_history_inner(project_id, limit))
    }
}

fn decode_issue_transitions(
    issue: &Document,
    output: &mut Vec<IssueNotificationTransition>,
    limit: usize,
) -> Result<(), NotificationStoreError> {
    let issue_id = IssueId::from_bytes(id16(issue, "_id")?);
    let project_id = ProjectId::new(
        issue
            .get_i32("p")
            .map_err(|_| NotificationStoreError::InvalidData)?,
    )
    .map_err(|_| NotificationStoreError::InvalidData)?;
    let title = IssueTitle::new(
        issue
            .get_str("t")
            .map_err(|_| NotificationStoreError::InvalidData)?,
    )
    .map_err(|_| NotificationStoreError::InvalidData)?;
    let transitions = issue
        .get_array("n")
        .map_err(|_| NotificationStoreError::InvalidData)?;
    if transitions.len() > 1_024 {
        return Err(NotificationStoreError::InvalidData);
    }
    for value in transitions {
        if output.len() == limit {
            break;
        }
        let transition = value
            .as_document()
            .ok_or(NotificationStoreError::InvalidData)?;
        let kind = match transition
            .get_i32("k")
            .map_err(|_| NotificationStoreError::InvalidData)?
        {
            1 => IssueNotificationKind::NewIssue,
            2 => IssueNotificationKind::Regression,
            3 => IssueNotificationKind::Resolved,
            _ => return Err(NotificationStoreError::InvalidData),
        };
        output.push(IssueNotificationTransition {
            transition_id: IssueTransitionId::from_bytes(id16(transition, "i")?),
            project_id,
            issue_id,
            kind,
            event_id: EventId::from_bytes(id16(transition, "e")?),
            created_at: timestamp(transition, "t")?,
            title: title.clone(),
        });
    }
    Ok(())
}

fn encode_rule(rule: &AlertRule) -> Document {
    let mut document = doc! {
        "_id": binary(rule.id.as_bytes()),
        "p": rule.project_id.get(),
        "n": rule.name.as_str(),
        "e": rule.enabled,
        "k": rule.triggers.iter().copied().map(trigger_name).collect::<Vec<_>>(),
        "d": rule.destination_ids.iter().map(|id| Bson::Binary(binary(id.as_bytes()))).collect::<Vec<_>>(),
        "o": i64::from(rule.cooldown_minutes),
        "b": i64::from(rule.storm_limit_per_hour),
        "sc": i64::from(rule.storm_count),
        "tm": rule.threshold_met,
        "c": date(rule.created_at),
        "u": date(rule.updated_at),
    };
    if let Some(aggregate) = &rule.aggregate {
        document.insert(
            "g",
            doc! {
                "d": aggregate.dataset.as_str(),
                "l": i64::from(aggregate.lookback_minutes),
                "i": i64::from(aggregate.evaluation_interval_minutes),
                "t": i64::try_from(aggregate.threshold).unwrap_or(i64::MAX),
                "e": aggregate.environment.as_ref().map(NotificationText::as_str),
                "r": aggregate.release.as_ref().map(NotificationText::as_str),
                "n": aggregate.notify_resolved,
            },
        );
    }
    if let Some(monitor) = &rule.monitor {
        document.insert(
            "h",
            doc! {
                "i": binary(monitor.monitor_id.as_bytes()),
                "o": monitor.outcomes.iter().copied().map(monitor_status_tag).collect::<Vec<_>>(),
                "n": monitor.notify_resolved,
            },
        );
    }
    for (name, value) in [
        ("x", rule.next_evaluation_at),
        ("lt", rule.last_triggered_at),
        ("sw", rule.storm_window_started_at),
    ] {
        if let Some(value) = value {
            document.insert(name, date(value));
        }
    }
    document
}

fn decode_rule(document: &Document) -> Result<AlertRule, NotificationStoreError> {
    let triggers = document
        .get_array("k")
        .map_err(|_| NotificationStoreError::InvalidData)?
        .iter()
        .map(|value| match value.as_str() {
            Some("new_issue") => Ok(IssueNotificationKind::NewIssue),
            Some("regression") => Ok(IssueNotificationKind::Regression),
            Some("resolved") => Ok(IssueNotificationKind::Resolved),
            _ => Err(NotificationStoreError::InvalidData),
        })
        .collect::<Result<Box<[_]>, _>>()?;
    let destination_ids = document
        .get_array("d")
        .map_err(|_| NotificationStoreError::InvalidData)?
        .iter()
        .map(|value| {
            let Bson::Binary(value) = value else {
                return Err(NotificationStoreError::InvalidData);
            };
            value
                .bytes
                .as_slice()
                .try_into()
                .map(NotificationDestinationId::from_bytes)
                .map_err(|_| NotificationStoreError::InvalidData)
        })
        .collect::<Result<Box<[_]>, _>>()?;
    let aggregate = document
        .get_document("g")
        .ok()
        .map(|value| {
            Ok::<_, NotificationStoreError>(AggregateAlert {
                dataset: match value
                    .get_str("d")
                    .map_err(|_| NotificationStoreError::InvalidData)?
                {
                    "errors" => ExploreDataset::Errors,
                    "logs" => ExploreDataset::Logs,
                    "spans" => ExploreDataset::Spans,
                    _ => return Err(NotificationStoreError::InvalidData),
                },
                lookback_minutes: u32::try_from(
                    value
                        .get_i64("l")
                        .map_err(|_| NotificationStoreError::InvalidData)?,
                )
                .map_err(|_| NotificationStoreError::InvalidData)?,
                evaluation_interval_minutes: u32::try_from(
                    value
                        .get_i64("i")
                        .map_err(|_| NotificationStoreError::InvalidData)?,
                )
                .map_err(|_| NotificationStoreError::InvalidData)?,
                threshold: u64::try_from(
                    value
                        .get_i64("t")
                        .map_err(|_| NotificationStoreError::InvalidData)?,
                )
                .map_err(|_| NotificationStoreError::InvalidData)?,
                environment: value
                    .get_str("e")
                    .ok()
                    .map(|value| NotificationText::new(value, 200))
                    .transpose()
                    .map_err(|_| NotificationStoreError::InvalidData)?,
                release: value
                    .get_str("r")
                    .ok()
                    .map(|value| NotificationText::new(value, 200))
                    .transpose()
                    .map_err(|_| NotificationStoreError::InvalidData)?,
                notify_resolved: value
                    .get_bool("n")
                    .map_err(|_| NotificationStoreError::InvalidData)?,
            })
        })
        .transpose()?;
    let monitor = document
        .get_document("h")
        .ok()
        .map(|value| {
            Ok::<_, NotificationStoreError>(MonitorAlert {
                monitor_id: MonitorId::from_bytes(id16(value, "i")?),
                outcomes: value
                    .get_array("o")
                    .map_err(|_| NotificationStoreError::InvalidData)?
                    .iter()
                    .map(|value| match value.as_i32() {
                        Some(2) => Ok(MonitorRunStatus::Error),
                        Some(3) => Ok(MonitorRunStatus::Timeout),
                        Some(4) => Ok(MonitorRunStatus::Missed),
                        _ => Err(NotificationStoreError::InvalidData),
                    })
                    .collect::<Result<Box<[_]>, _>>()?,
                notify_resolved: value.get_bool("n").unwrap_or(false),
            })
        })
        .transpose()?;
    let rule = AlertRule {
        id: AlertRuleId::from_bytes(id16(document, "_id")?),
        project_id: ProjectId::new(
            document
                .get_i32("p")
                .map_err(|_| NotificationStoreError::InvalidData)?,
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
        name: RuleName::new(
            document
                .get_str("n")
                .map_err(|_| NotificationStoreError::InvalidData)?,
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
        enabled: document
            .get_bool("e")
            .map_err(|_| NotificationStoreError::InvalidData)?,
        triggers,
        aggregate,
        monitor,
        destination_ids,
        cooldown_minutes: u32::try_from(document.get_i64("o").unwrap_or(0))
            .map_err(|_| NotificationStoreError::InvalidData)?,
        storm_limit_per_hour: u32::try_from(document.get_i64("b").unwrap_or(100))
            .map_err(|_| NotificationStoreError::InvalidData)?,
        next_evaluation_at: optional_timestamp(document, "y")?
            .or(optional_timestamp(document, "x")?),
        last_triggered_at: optional_timestamp(document, "lt")?,
        storm_window_started_at: optional_timestamp(document, "sw")?,
        storm_count: u32::try_from(document.get_i64("sc").unwrap_or(0))
            .map_err(|_| NotificationStoreError::InvalidData)?,
        threshold_met: document.get_bool("tm").unwrap_or(false),
        created_at: timestamp(document, "c")?,
        updated_at: timestamp(document, "u")?,
    };
    rule.validate()
        .map_err(|_| NotificationStoreError::InvalidData)?;
    Ok(rule)
}

fn encode_destination(destination: &NotificationDestination) -> Document {
    let mut document = doc! {
        "_id": binary(destination.id.as_bytes()),
        "p": destination.project_id.get(),
        "k": destination_kind_name(destination.kind),
        "u": destination.endpoint.as_str(),
        "s": Binary {
            subtype: BinarySubtype::Generic,
            bytes: destination.sealed_secret.expose_ciphertext().to_vec(),
        },
        "e": destination.enabled,
        "c": date(destination.created_at),
        "m": date(destination.updated_at),
    };
    if let Some(smtp) = &destination.smtp {
        document.insert(
            "o",
            doc! {
                "p": i32::from(smtp.port),
                "t": smtp_security_name(smtp.security),
                "a": smtp.username.as_str(),
                "f": smtp.from.as_str(),
                "r": smtp.recipients.iter().map(NotificationText::as_str).collect::<Vec<_>>(),
            },
        );
    }
    document
}

fn decode_destination(
    document: &Document,
) -> Result<NotificationDestination, NotificationStoreError> {
    let kind = match document.get_str("k").unwrap_or("webhook") {
        "webhook" => NotificationDestinationKind::Webhook,
        "telegram" => NotificationDestinationKind::Telegram,
        "smtp_email" => NotificationDestinationKind::SmtpEmail,
        _ => return Err(NotificationStoreError::InvalidData),
    };
    let smtp = document
        .get_document("o")
        .ok()
        .map(|smtp| {
            let recipients = smtp
                .get_array("r")
                .map_err(|_| NotificationStoreError::InvalidData)?
                .iter()
                .map(|value| {
                    NotificationText::new(
                        value.as_str().ok_or(NotificationStoreError::InvalidData)?,
                        MAX_EMAIL_ADDRESS_BYTES,
                    )
                    .map_err(|_| NotificationStoreError::InvalidData)
                })
                .collect::<Result<Box<[_]>, _>>()?;
            Ok::<_, NotificationStoreError>(SmtpDestination {
                port: u16::try_from(
                    smtp.get_i32("p")
                        .map_err(|_| NotificationStoreError::InvalidData)?,
                )
                .map_err(|_| NotificationStoreError::InvalidData)?,
                security: match smtp
                    .get_str("t")
                    .map_err(|_| NotificationStoreError::InvalidData)?
                {
                    "starttls" => SmtpSecurity::StartTls,
                    "tls" => SmtpSecurity::Tls,
                    _ => return Err(NotificationStoreError::InvalidData),
                },
                username: NotificationText::new(
                    smtp.get_str("a")
                        .map_err(|_| NotificationStoreError::InvalidData)?,
                    MAX_EMAIL_ADDRESS_BYTES,
                )
                .map_err(|_| NotificationStoreError::InvalidData)?,
                from: NotificationText::new(
                    smtp.get_str("f")
                        .map_err(|_| NotificationStoreError::InvalidData)?,
                    MAX_EMAIL_ADDRESS_BYTES,
                )
                .map_err(|_| NotificationStoreError::InvalidData)?,
                recipients,
            })
        })
        .transpose()?;
    let destination = NotificationDestination {
        id: NotificationDestinationId::from_bytes(id16(document, "_id")?),
        project_id: ProjectId::new(
            document
                .get_i32("p")
                .map_err(|_| NotificationStoreError::InvalidData)?,
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
        kind,
        endpoint: WebhookEndpoint::new(
            document
                .get_str("u")
                .map_err(|_| NotificationStoreError::InvalidData)?,
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
        smtp,
        sealed_secret: SealedWebhookSecret::new(
            document
                .get_binary_generic("s")
                .map_err(|_| NotificationStoreError::InvalidData)?
                .to_vec(),
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
        enabled: document
            .get_bool("e")
            .map_err(|_| NotificationStoreError::InvalidData)?,
        created_at: timestamp(document, "c")?,
        updated_at: timestamp(document, "m")?,
    };
    destination
        .validate()
        .map_err(|_| NotificationStoreError::InvalidData)?;
    Ok(destination)
}

fn encode_delivery(delivery: &NotificationDelivery) -> Result<Document, NotificationStoreError> {
    if delivery.payload.as_bytes().is_empty() {
        return Err(NotificationStoreError::InvalidData);
    }
    Ok(doc! {
        "_id": binary(delivery.id.as_bytes()),
        "p": delivery.project_id.get(),
        "u": binary(delivery.issue_id.as_bytes()),
        "t": binary(delivery.transition_id.as_bytes()),
        "r": binary(delivery.rule_id.as_bytes()),
        "q": binary(delivery.action_id.as_bytes()),
        "d": binary(delivery.destination_id.as_bytes()),
        "b": "webhook",
        "v": Binary { subtype: BinarySubtype::Generic, bytes: delivery.payload.as_bytes().to_vec() },
        "s": "pending",
        "a": i64::from(delivery.attempts),
        "n": date(delivery.next_attempt_at),
        "c": date(delivery.created_at),
    })
}

fn decode_delivery(document: &Document) -> Result<NotificationDelivery, NotificationStoreError> {
    let status = match document
        .get_str("s")
        .map_err(|_| NotificationStoreError::InvalidData)?
    {
        "pending" => NotificationDeliveryStatus::Pending,
        "delivered" => NotificationDeliveryStatus::Delivered,
        "dead" => NotificationDeliveryStatus::Dead,
        _ => return Err(NotificationStoreError::InvalidData),
    };
    Ok(NotificationDelivery {
        id: NotificationDeliveryId::from_bytes(id16(document, "_id")?),
        project_id: ProjectId::new(
            document
                .get_i32("p")
                .map_err(|_| NotificationStoreError::InvalidData)?,
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
        issue_id: IssueId::from_bytes(id16(document, "u")?),
        transition_id: IssueTransitionId::from_bytes(id16(document, "t")?),
        rule_id: AlertRuleId::from_bytes(id16(document, "r")?),
        action_id: NotificationDestinationId::from_bytes(id16(document, "q")?),
        destination_id: NotificationDestinationId::from_bytes(id16(document, "d")?),
        payload: NotificationPayload::new(
            document
                .get_binary_generic("v")
                .map_err(|_| NotificationStoreError::InvalidData)?
                .to_vec(),
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
        status,
        attempts: u32::try_from(
            document
                .get_i64("a")
                .map_err(|_| NotificationStoreError::InvalidData)?,
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
        next_attempt_at: timestamp(document, "n")?,
        last_error: document.get_str("z").ok().map(Into::into),
        created_at: timestamp(document, "c")?,
        delivered_at: if status == NotificationDeliveryStatus::Delivered {
            optional_timestamp(document, "l")?
        } else {
            None
        },
        delete_at: optional_timestamp(document, "x")?,
    })
}

async fn terminal_update(
    database: &Database,
    id: NotificationDeliveryId,
    status: &str,
    terminal_at: Timestamp,
    delete_at: Timestamp,
    error: Option<&str>,
) -> Result<(), NotificationStoreError> {
    let mut set = doc! {
        "s": status,
        "l": date(terminal_at),
        "x": date(delete_at),
    };
    if let Some(error) = error {
        set.insert("z", error);
    }
    database
        .collection::<Document>("notification_deliveries")
        .find_one_and_update(
            doc! { "_id": binary(id.as_bytes()), "s": "pending" },
            doc! { "$set": set, "$unset": { "n": "", "y": "" } },
        )
        .return_document(ReturnDocument::After)
        .await
        .map_err(|_| NotificationStoreError::Unavailable)?;
    Ok(())
}

fn validate_error(value: &str) -> Result<(), NotificationStoreError> {
    if value.is_empty() || value.len() > 64 || value.chars().any(char::is_control) {
        return Err(NotificationStoreError::InvalidData);
    }
    Ok(())
}

fn trigger_name(kind: IssueNotificationKind) -> &'static str {
    match kind {
        IssueNotificationKind::NewIssue => "new_issue",
        IssueNotificationKind::Regression => "regression",
        IssueNotificationKind::Resolved => "resolved",
    }
}

fn monitor_status_tag(status: MonitorRunStatus) -> i32 {
    match status {
        MonitorRunStatus::Error => 2,
        MonitorRunStatus::Timeout => 3,
        MonitorRunStatus::Missed => 4,
        MonitorRunStatus::InProgress | MonitorRunStatus::Success => -1,
    }
}

fn destination_kind_name(kind: NotificationDestinationKind) -> &'static str {
    match kind {
        NotificationDestinationKind::Webhook => "webhook",
        NotificationDestinationKind::Telegram => "telegram",
        NotificationDestinationKind::SmtpEmail => "smtp_email",
    }
}

fn smtp_security_name(security: SmtpSecurity) -> &'static str {
    match security {
        SmtpSecurity::StartTls => "starttls",
        SmtpSecurity::Tls => "tls",
    }
}

fn id16(document: &Document, field: &str) -> Result<[u8; 16], NotificationStoreError> {
    document
        .get_binary_generic(field)
        .map_err(|_| NotificationStoreError::InvalidData)?
        .as_slice()
        .try_into()
        .map_err(|_| NotificationStoreError::InvalidData)
}

fn timestamp(document: &Document, field: &str) -> Result<Timestamp, NotificationStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(field)
            .map_err(|_| NotificationStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| NotificationStoreError::InvalidData)
}

fn optional_timestamp(
    document: &Document,
    field: &str,
) -> Result<Option<Timestamp>, NotificationStoreError> {
    document
        .get_datetime(field)
        .ok()
        .map(|value| {
            Timestamp::from_unix_millis(value.timestamp_millis())
                .map_err(|_| NotificationStoreError::InvalidData)
        })
        .transpose()
}

fn binary(bytes: impl AsRef<[u8]>) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.as_ref().to_vec(),
    }
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

pub(crate) fn destination_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "k", "u", "s", "e", "c", "m"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "p": { "bsonType": "int", "minimum": 1 },
            "k": { "enum": ["webhook", "telegram", "smtp_email"] },
            "u": { "bsonType": "string", "maxLength": 2048 },
            "s": { "bsonType": "binData" },
            "e": { "bsonType": "bool" },
            "c": { "bsonType": "date" },
            "m": { "bsonType": "date" },
            "o": {
                "bsonType": "object",
                "required": ["p", "t", "a", "f", "r"],
                "additionalProperties": false,
                "properties": {
                    "p": { "bsonType": "int", "minimum": 1, "maximum": 65535 },
                    "t": { "enum": ["starttls", "tls"] },
                    "a": { "bsonType": "string", "maxLength": 320 },
                    "f": { "bsonType": "string", "maxLength": 320 },
                    "r": { "bsonType": "array", "minItems": 1, "maxItems": 16, "items": { "bsonType": "string", "maxLength": 320 } },
                },
            },
        },
    }}
}

pub(crate) fn rule_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "n", "e", "k", "d", "o", "b", "sc", "tm", "c", "u"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "p": { "bsonType": "int", "minimum": 1 },
            "n": { "bsonType": "string", "maxLength": 200 },
            "e": { "bsonType": "bool" },
            "k": { "bsonType": "array", "maxItems": 3, "items": { "enum": ["new_issue", "regression", "resolved"] } },
            "d": { "bsonType": "array", "minItems": 1, "maxItems": 32, "items": { "bsonType": "binData" } },
            "c": { "bsonType": "date" },
            "u": { "bsonType": "date" },
            "g": { "bsonType": "object" },
            "h": { "bsonType": "object" },
            "o": { "bsonType": "long", "minimum": 0 },
            "b": { "bsonType": "long", "minimum": 1 },
            "sc": { "bsonType": "long", "minimum": 0 },
            "tm": { "bsonType": "bool" },
            "x": { "bsonType": "date" },
            "lt": { "bsonType": "date" },
            "sw": { "bsonType": "date" },
            "y": { "bsonType": "date" },
        },
    }}
}

pub(crate) fn delivery_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "u", "t", "r", "q", "d", "b", "v", "s", "a", "c"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "p": { "bsonType": "int", "minimum": 1 },
            "u": { "bsonType": "binData" },
            "t": { "bsonType": "binData" },
            "r": { "bsonType": "binData" },
            "q": { "bsonType": "binData" },
            "d": { "bsonType": "binData" },
            "b": { "enum": ["webhook"] },
            "v": { "bsonType": "binData" },
            "s": { "enum": ["pending", "delivered", "dead"] },
            "a": { "bsonType": "long", "minimum": 0 },
            "n": { "bsonType": "date" },
            "z": { "bsonType": "string", "maxLength": 64 },
            "c": { "bsonType": "date" },
            "l": { "bsonType": "date" },
            "x": { "bsonType": "date" },
            "y": { "bsonType": "date" },
        },
    }}
}

pub(crate) fn notification_indexes(collection: &str) -> Vec<IndexModel> {
    match collection {
        "notification_destinations" => vec![named_index(
            doc! { "p": 1, "e": 1, "_id": 1 },
            "notification_destination_project",
            None,
        )],
        "alert_rules" => vec![
            named_index(
                doc! { "p": 1, "e": 1, "k": 1, "_id": 1 },
                "alert_rule_match",
                None,
            ),
            named_index(
                doc! { "e": 1, "x": 1, "_id": 1 },
                "alert_rule_aggregate_due",
                Some(doc! { "e": true, "g": { "$exists": true } }),
            ),
        ],
        "notification_deliveries" => vec![
            named_index(
                doc! { "s": 1, "n": 1, "_id": 1 },
                "notification_delivery_due",
                Some(doc! { "s": "pending" }),
            ),
            IndexModel::builder()
                .keys(doc! { "x": 1 })
                .options(
                    IndexOptions::builder()
                        .name("notification_delivery_expiry".to_owned())
                        .expire_after(Duration::ZERO)
                        .partial_filter_expression(doc! { "s": { "$in": ["delivered", "dead"] } })
                        .build(),
                )
                .build(),
            named_index(
                doc! { "p": 1, "c": -1, "_id": -1 },
                "notification_delivery_history",
                None,
            ),
        ],
        _ => Vec::new(),
    }
}

pub(crate) fn notification_index_names(collection: &str) -> BTreeSet<&'static str> {
    match collection {
        "notification_destinations" => BTreeSet::from(["_id_", "notification_destination_project"]),
        "alert_rules" => BTreeSet::from(["_id_", "alert_rule_aggregate_due", "alert_rule_match"]),
        "notification_deliveries" => BTreeSet::from([
            "_id_",
            "notification_delivery_due",
            "notification_delivery_expiry",
            "notification_delivery_history",
        ]),
        _ => BTreeSet::new(),
    }
}

fn named_index(keys: Document, name: &str, partial: Option<Document>) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .partial_filter_expression(partial)
                .build(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_selection_rotates_away_from_a_noisy_destination() {
        let now = Timestamp::from_unix_millis(1).unwrap();
        let candidates = [
            ([1; 16], 1, [9; 16], now),
            ([2; 16], 1, [8; 16], now),
            ([3; 16], 2, [7; 16], now),
        ];
        assert_eq!(
            choose_fair_candidate(&candidates, Some((1, [9; 16])))
                .unwrap()
                .0,
            [3; 16]
        );
        assert_eq!(
            choose_fair_candidate(&candidates, Some((7, [6; 16])))
                .unwrap()
                .0,
            [1; 16]
        );
    }
}
