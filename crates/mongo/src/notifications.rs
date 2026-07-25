use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use metric_domain::{
    EventId, ProjectId, Timestamp,
    grouping::IssueId,
    issue::{IssueNotificationKind, IssueTitle, IssueTransitionId},
    notifications::{
        AlertRule, AlertRuleId, ClaimedNotificationDelivery, IssueNotificationTransition,
        NotificationDelivery, NotificationDeliveryId, NotificationDeliveryStatus,
        NotificationDestination, NotificationDestinationId, NotificationPayload, RuleName,
        SealedWebhookSecret, WebhookEndpoint,
    },
};
use metric_ports::{NotificationStore, NotificationStoreError, PortFuture};
use futures_util::TryStreamExt;
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
    doc! {
        "_id": binary(rule.id.as_bytes()),
        "p": rule.project_id.get(),
        "n": rule.name.as_str(),
        "e": rule.enabled,
        "k": rule.triggers.iter().copied().map(trigger_name).collect::<Vec<_>>(),
        "d": rule.destination_ids.iter().map(|id| Bson::Binary(binary(id.as_bytes()))).collect::<Vec<_>>(),
        "c": date(rule.created_at),
        "u": date(rule.updated_at),
    }
}

fn decode_rule(document: &Document) -> Result<AlertRule, NotificationStoreError> {
    let triggers = document
        .get_array("k")
        .map_err(|_| NotificationStoreError::InvalidData)?
        .iter()
        .map(|value| match value.as_str() {
            Some("new_issue") => Ok(IssueNotificationKind::NewIssue),
            Some("regression") => Ok(IssueNotificationKind::Regression),
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
        destination_ids,
        created_at: timestamp(document, "c")?,
        updated_at: timestamp(document, "u")?,
    };
    rule.validate()
        .map_err(|_| NotificationStoreError::InvalidData)?;
    Ok(rule)
}

fn encode_destination(destination: &NotificationDestination) -> Document {
    doc! {
        "_id": binary(destination.id.as_bytes()),
        "p": destination.project_id.get(),
        "u": destination.endpoint.as_str(),
        "s": Binary {
            subtype: BinarySubtype::Generic,
            bytes: destination.sealed_secret.expose_ciphertext().to_vec(),
        },
        "e": destination.enabled,
        "c": date(destination.created_at),
        "m": date(destination.updated_at),
    }
}

fn decode_destination(
    document: &Document,
) -> Result<NotificationDestination, NotificationStoreError> {
    Ok(NotificationDestination {
        id: NotificationDestinationId::from_bytes(id16(document, "_id")?),
        project_id: ProjectId::new(
            document
                .get_i32("p")
                .map_err(|_| NotificationStoreError::InvalidData)?,
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
        endpoint: WebhookEndpoint::new(
            document
                .get_str("u")
                .map_err(|_| NotificationStoreError::InvalidData)?,
        )
        .map_err(|_| NotificationStoreError::InvalidData)?,
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
    })
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
        "required": ["_id", "p", "u", "s", "e", "c", "m"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "p": { "bsonType": "int", "minimum": 1 },
            "u": { "bsonType": "string", "maxLength": 2048 },
            "s": { "bsonType": "binData" },
            "e": { "bsonType": "bool" },
            "c": { "bsonType": "date" },
            "m": { "bsonType": "date" },
        },
    }}
}

pub(crate) fn rule_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "n", "e", "k", "d", "c", "u"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "p": { "bsonType": "int", "minimum": 1 },
            "n": { "bsonType": "string", "maxLength": 200 },
            "e": { "bsonType": "bool" },
            "k": { "bsonType": "array", "minItems": 1, "maxItems": 2, "items": { "enum": ["new_issue", "regression"] } },
            "d": { "bsonType": "array", "minItems": 1, "maxItems": 32, "items": { "bsonType": "binData" } },
            "c": { "bsonType": "date" },
            "u": { "bsonType": "date" },
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
        "alert_rules" => vec![named_index(
            doc! { "p": 1, "e": 1, "k": 1, "_id": 1 },
            "alert_rule_match",
            None,
        )],
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
        "alert_rules" => BTreeSet::from(["_id_", "alert_rule_match"]),
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
