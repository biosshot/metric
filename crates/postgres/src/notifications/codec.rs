use super::*;
use metric_domain::{
    explore::ExploreDataset,
    monitors::{MonitorId, MonitorRunStatus},
};
use serde_json::{Value, json};

pub(super) fn encode_destination(v: &NotificationDestination) -> Result<(Vec<u8>, usize)> {
    v.validate().map_err(invalid)?;
    let telegram = v.telegram.as_ref().map(|g| {
        json!([
            g.api_base.as_str(),
            g.message_thread_id,
            g.bot_id,
            g.bot_username.as_ref().map(NotificationText::as_str),
            g.bot_display_name.as_ref().map(NotificationText::as_str),
            g.chat_kind.map(|k| match k {
                TelegramChatKind::Private => 0,
                TelegramChatKind::Group => 1,
                TelegramChatKind::Supergroup => 2,
                TelegramChatKind::Channel => 3,
            }),
            g.chat_username.as_ref().map(NotificationText::as_str),
            g.chat_display_name.as_ref().map(NotificationText::as_str)
        ])
    });
    let smtp = v.smtp.as_ref().map(|s| {
        json!([
            s.port,
            if s.security == SmtpSecurity::StartTls {
                0
            } else {
                1
            },
            s.username.as_str(),
            s.from.as_str(),
            s.recipients
                .iter()
                .map(NotificationText::as_str)
                .collect::<Vec<_>>()
        ])
    });
    let value = json!([telegram, smtp]);
    decode_destination_config(&value)?;
    encoded(&value)
}
fn decode_destination_config(
    value: &Value,
) -> Result<(Option<TelegramDestination>, Option<SmtpDestination>)> {
    let v = array(value, 2)?;
    let telegram = optional(&v[0], |v| {
        let v = array(v, 8)?;
        Ok(TelegramDestination {
            api_base: TelegramApiBase::new(string(&v[0])?).map_err(invalid)?,
            message_thread_id: optional(&v[1], integer)?,
            bot_id: optional(&v[2], integer)?,
            bot_username: optional(&v[3], |v| text(v, MAX_TELEGRAM_USERNAME_BYTES))?,
            bot_display_name: optional(&v[4], |v| text(v, MAX_TELEGRAM_DISPLAY_NAME_BYTES))?,
            chat_kind: optional(&v[5], |v| match uint(v)? {
                0 => Ok(TelegramChatKind::Private),
                1 => Ok(TelegramChatKind::Group),
                2 => Ok(TelegramChatKind::Supergroup),
                3 => Ok(TelegramChatKind::Channel),
                _ => Err(NotificationStoreError::InvalidData),
            })?,
            chat_username: optional(&v[6], |v| text(v, MAX_TELEGRAM_USERNAME_BYTES))?,
            chat_display_name: optional(&v[7], |v| text(v, MAX_TELEGRAM_DISPLAY_NAME_BYTES))?,
        })
    })?;
    let smtp = optional(&v[1], |v| {
        let v = array(v, 5)?;
        let recipients = v[4]
            .as_array()
            .filter(|v| v.len() <= MAX_EMAIL_RECIPIENTS)
            .ok_or(NotificationStoreError::InvalidData)?;
        Ok(SmtpDestination {
            port: u16::try_from(uint(&v[0])?).map_err(invalid)?,
            security: match uint(&v[1])? {
                0 => SmtpSecurity::StartTls,
                1 => SmtpSecurity::Tls,
                _ => return Err(NotificationStoreError::InvalidData),
            },
            username: text(&v[2], MAX_EMAIL_ADDRESS_BYTES)?,
            from: text(&v[3], MAX_EMAIL_ADDRESS_BYTES)?,
            recipients: recipients
                .iter()
                .map(|v| text(v, MAX_EMAIL_ADDRESS_BYTES))
                .collect::<Result<Box<[_]>>>()?,
        })
    })?;
    Ok((telegram, smtp))
}
pub(super) fn decode_destination(row: &PgRow) -> Result<NotificationDestination> {
    let value = body(row)?;
    let (telegram, smtp) = decode_destination_config(&value)?;
    let v = NotificationDestination {
        id: NotificationDestinationId::from_bytes(fixed(row, "destination_id")?),
        project_id: project(row)?,
        kind: match row.try_get::<i16, _>("kind").map_err(invalid)? {
            0 => NotificationDestinationKind::Webhook,
            1 => NotificationDestinationKind::Telegram,
            2 => NotificationDestinationKind::SmtpEmail,
            _ => return Err(NotificationStoreError::InvalidData),
        },
        endpoint: WebhookEndpoint::new(utf8(row, "endpoint")?).map_err(invalid)?,
        sealed_secret: SealedWebhookSecret::new(
            row.try_get::<Vec<u8>, _>("sealed_secret")
                .map_err(invalid)?,
        )
        .map_err(invalid)?,
        telegram,
        smtp,
        enabled: row.try_get("enabled").map_err(invalid)?,
        created_at: timestamp(row, "created_at")?,
        updated_at: timestamp(row, "updated_at")?,
    };
    v.validate().map_err(invalid)?;
    Ok(v)
}
pub(super) fn rule_kind(rule: &AlertRule) -> i16 {
    if rule.aggregate.is_some() {
        1
    } else if rule.monitor.is_some() {
        2
    } else {
        0
    }
}
pub(super) fn mask(rule: &AlertRule) -> i16 {
    rule.triggers
        .iter()
        .fold(0, |v, k| v | (1_i16 << (*k as u8 - 1)))
}
pub(super) fn encode_rule(v: &AlertRule) -> Result<(Vec<u8>, usize)> {
    v.validate().map_err(invalid)?;
    if v.triggers.len() > 3 {
        return Err(NotificationStoreError::InvalidData);
    }
    let aggregate = v.aggregate.as_ref().map(|a| {
        json!([
            a.dataset.as_str(),
            a.lookback_minutes,
            a.evaluation_interval_minutes,
            a.threshold,
            a.environment.as_ref().map(NotificationText::as_str),
            a.release.as_ref().map(NotificationText::as_str),
            a.notify_resolved
        ])
    });
    let monitor = v.monitor.as_ref().map(|m| {
        json!([
            m.monitor_id.as_bytes(),
            m.outcomes
                .iter()
                .map(|s| match s {
                    MonitorRunStatus::Error => 2,
                    MonitorRunStatus::Timeout => 3,
                    MonitorRunStatus::Missed => 4,
                    _ => 0,
                })
                .collect::<Vec<_>>(),
            m.notify_resolved
        ])
    });
    let value = json!([
        v.triggers.iter().map(|k| *k as u8).collect::<Vec<_>>(),
        v.destination_ids
            .iter()
            .map(|id| id.as_bytes())
            .collect::<Vec<_>>(),
        aggregate,
        monitor
    ]);
    decode_rule_config(&value)?;
    encoded(&value)
}
type RuleConfig = (
    Box<[IssueNotificationKind]>,
    Box<[NotificationDestinationId]>,
    Option<AggregateAlert>,
    Option<MonitorAlert>,
);
fn decode_rule_config(value: &Value) -> Result<RuleConfig> {
    let v = array(value, 4)?;
    let triggers = bounded_array(&v[0], 3)?
        .iter()
        .map(|v| kind(i16::try_from(uint(v)?).map_err(invalid)?))
        .collect::<Result<Box<[_]>>>()?;
    let destinations = bounded_array(&v[1], MAX_RULE_DESTINATIONS)?
        .iter()
        .map(|v| bytes16(v).map(NotificationDestinationId::from_bytes))
        .collect::<Result<Box<[_]>>>()?;
    let aggregate = optional(&v[2], |v| {
        let v = array(v, 7)?;
        Ok(AggregateAlert {
            dataset: match string(&v[0])? {
                "errors" => ExploreDataset::Errors,
                "logs" => ExploreDataset::Logs,
                "spans" => ExploreDataset::Spans,
                "metrics" => ExploreDataset::Metrics,
                _ => return Err(NotificationStoreError::InvalidData),
            },
            lookback_minutes: u32value(&v[1])?,
            evaluation_interval_minutes: u32value(&v[2])?,
            threshold: uint(&v[3])?,
            environment: optional(&v[4], |v| text(v, 200))?,
            release: optional(&v[5], |v| text(v, 200))?,
            notify_resolved: boolean(&v[6])?,
        })
    })?;
    let monitor = optional(&v[3], |v| {
        let v = array(v, 3)?;
        Ok(MonitorAlert {
            monitor_id: MonitorId::from_bytes(bytes16(&v[0])?),
            outcomes: bounded_array(&v[1], 3)?
                .iter()
                .map(|v| match uint(v)? {
                    2 => Ok(MonitorRunStatus::Error),
                    3 => Ok(MonitorRunStatus::Timeout),
                    4 => Ok(MonitorRunStatus::Missed),
                    _ => Err(NotificationStoreError::InvalidData),
                })
                .collect::<Result<Box<[_]>>>()?,
            notify_resolved: boolean(&v[2])?,
        })
    })?;
    Ok((triggers, destinations, aggregate, monitor))
}
pub(super) fn decode_rule(row: &PgRow) -> Result<AlertRule> {
    let (triggers, destination_ids, aggregate, monitor) = decode_rule_config(&body(row)?)?;
    let v = AlertRule {
        id: AlertRuleId::from_bytes(fixed(row, "rule_id")?),
        project_id: project(row)?,
        name: RuleName::new(utf8(row, "name")?).map_err(invalid)?,
        enabled: row.try_get("enabled").map_err(invalid)?,
        triggers,
        aggregate,
        monitor,
        destination_ids,
        cooldown_minutes: u32::try_from(
            row.try_get::<i32, _>("cooldown_minutes").map_err(invalid)?,
        )
        .map_err(invalid)?,
        storm_limit_per_hour: u32::try_from(row.try_get::<i32, _>("storm_limit").map_err(invalid)?)
            .map_err(invalid)?,
        next_evaluation_at: optional_time(row, "next_evaluation_at")?,
        last_triggered_at: optional_time(row, "last_triggered_at")?,
        storm_window_started_at: optional_time(row, "storm_window_started_at")?,
        storm_count: u32::try_from(row.try_get::<i64, _>("storm_count").map_err(invalid)?)
            .map_err(invalid)?,
        threshold_met: row.try_get("threshold_met").map_err(invalid)?,
        created_at: timestamp(row, "created_at")?,
        updated_at: timestamp(row, "updated_at")?,
    };
    v.validate().map_err(invalid)?;
    if row.try_get::<i16, _>("kind").map_err(invalid)? != rule_kind(&v)
        || row.try_get::<i16, _>("trigger_mask").map_err(invalid)? != mask(&v)
    {
        return Err(NotificationStoreError::InvalidData);
    }
    Ok(v)
}
pub(super) fn decode_delivery(row: &PgRow) -> Result<NotificationDelivery> {
    let status = match row.try_get::<i16, _>("status").map_err(invalid)? {
        0 => NotificationDeliveryStatus::Pending,
        1 => NotificationDeliveryStatus::Delivered,
        2 => NotificationDeliveryStatus::Dead,
        _ => return Err(NotificationStoreError::InvalidData),
    };
    let terminal = optional_time(row, "terminal_at")?;
    let next = optional_time(row, "next_attempt_at")?
        .or(terminal)
        .ok_or(NotificationStoreError::InvalidData)?;
    let error: Option<String> = row.try_get("last_error").map_err(invalid)?;
    if let Some(e) = &error {
        validate_error(e)?;
    }
    Ok(NotificationDelivery {
        id: NotificationDeliveryId::from_bytes(fixed(row, "delivery_id")?),
        project_id: project(row)?,
        issue_id: IssueId::from_bytes(fixed(row, "issue_id")?),
        transition_id: IssueTransitionId::from_bytes(fixed(row, "transition_id")?),
        rule_id: AlertRuleId::from_bytes(fixed(row, "rule_id")?),
        action_id: NotificationDestinationId::from_bytes(fixed(row, "action_id")?),
        destination_id: NotificationDestinationId::from_bytes(fixed(row, "destination_id")?),
        payload: NotificationPayload::new(row.try_get::<Vec<u8>, _>("payload").map_err(invalid)?)
            .map_err(invalid)?,
        status,
        attempts: u32::try_from(row.try_get::<i64, _>("attempts").map_err(invalid)?)
            .map_err(invalid)?,
        next_attempt_at: next,
        last_error: error.map(Into::into),
        created_at: timestamp(row, "created_at")?,
        delivered_at: if status == NotificationDeliveryStatus::Delivered {
            terminal
        } else {
            None
        },
        delete_at: optional_time(row, "delete_at")?,
    })
}
pub(super) fn decode_transition(row: &PgRow) -> Result<IssueNotificationTransition> {
    Ok(IssueNotificationTransition {
        transition_id: IssueTransitionId::from_bytes(fixed(row, "transition_id")?),
        project_id: project(row)?,
        issue_id: IssueId::from_bytes(fixed(row, "issue_id")?),
        kind: kind(row.try_get("kind").map_err(invalid)?)?,
        event_id: EventId::from_bytes(fixed(row, "event_id")?),
        created_at: timestamp(row, "created_at")?,
        title: IssueTitle::new(utf8(row, "title")?).map_err(invalid)?,
    })
}
fn body(row: &PgRow) -> Result<Value> {
    let bytes = crate::codec::decode_json(
        &row.try_get::<Vec<u8>, _>("config").map_err(invalid)?,
        MAX_CONFIG,
    )
    .map_err(invalid)?;
    if bytes.len()
        != usize::try_from(row.try_get::<i32, _>("config_size").map_err(invalid)?)
            .map_err(invalid)?
    {
        return Err(NotificationStoreError::InvalidData);
    }
    serde_json::from_slice(&bytes).map_err(invalid)
}
fn encoded(value: &Value) -> Result<(Vec<u8>, usize)> {
    let (bytes, canonical) =
        crate::codec::encode_json(value, 3, 64, MAX_CONFIG).map_err(invalid)?;
    Ok((bytes, canonical.len()))
}
fn array(v: &Value, length: usize) -> Result<&[Value]> {
    let v = v
        .as_array()
        .filter(|v| v.len() == length)
        .ok_or(NotificationStoreError::InvalidData)?;
    Ok(v)
}
fn bounded_array(v: &Value, length: usize) -> Result<&[Value]> {
    v.as_array()
        .filter(|v| v.len() <= length)
        .map(Vec::as_slice)
        .ok_or(NotificationStoreError::InvalidData)
}
fn uint(v: &Value) -> Result<u64> {
    v.as_u64().ok_or(NotificationStoreError::InvalidData)
}
fn u32value(v: &Value) -> Result<u32> {
    u32::try_from(uint(v)?).map_err(invalid)
}
fn integer(v: &Value) -> Result<i64> {
    v.as_i64().ok_or(NotificationStoreError::InvalidData)
}
fn string(v: &Value) -> Result<&str> {
    v.as_str().ok_or(NotificationStoreError::InvalidData)
}
fn boolean(v: &Value) -> Result<bool> {
    v.as_bool().ok_or(NotificationStoreError::InvalidData)
}
fn text(v: &Value, max: usize) -> Result<NotificationText> {
    NotificationText::new(string(v)?, max).map_err(invalid)
}
fn optional<T>(v: &Value, decode: impl FnOnce(&Value) -> Result<T>) -> Result<Option<T>> {
    if v.is_null() {
        Ok(None)
    } else {
        decode(v).map(Some)
    }
}
fn bytes16(v: &Value) -> Result<[u8; 16]> {
    let bytes = array(v, 16)?
        .iter()
        .map(|v| u8::try_from(uint(v)?).map_err(invalid))
        .collect::<Result<Vec<_>>>()?;
    bytes.try_into().map_err(invalid)
}
pub(super) fn fixed(row: &PgRow, key: &str) -> Result<[u8; 16]> {
    row.try_get::<Vec<u8>, _>(key)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
pub(super) fn timestamp(row: &PgRow, key: &str) -> Result<Timestamp> {
    at(row.try_get(key).map_err(invalid)?)
}
pub(super) fn optional_time(row: &PgRow, key: &str) -> Result<Option<Timestamp>> {
    row.try_get::<Option<i64>, _>(key)
        .map_err(invalid)?
        .map(at)
        .transpose()
}
fn project(row: &PgRow) -> Result<ProjectId> {
    ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)
}
fn utf8(row: &PgRow, key: &str) -> Result<String> {
    crate::codec::decode_utf8(row.try_get(key).map_err(invalid)?).map_err(invalid)
}
fn kind(v: i16) -> Result<IssueNotificationKind> {
    match v {
        1 => Ok(IssueNotificationKind::NewIssue),
        2 => Ok(IssueNotificationKind::Regression),
        3 => Ok(IssueNotificationKind::Resolved),
        _ => Err(NotificationStoreError::InvalidData),
    }
}
