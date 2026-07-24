//! Durable notification values shared by the application and storage adapters.

use std::fmt;

use thiserror::Error;

use crate::{
    EventId, ProjectId, Timestamp,
    grouping::IssueId,
    issue::{IssueNotificationKind, IssueTitle, IssueTransitionId},
};

pub const MAX_RULE_NAME_BYTES: usize = 200;
pub const MAX_RULE_DESTINATIONS: usize = 32;
pub const MAX_ENDPOINT_BYTES: usize = 2_048;
pub const MAX_SEALED_SECRET_BYTES: usize = 8_192;
pub const MAX_NOTIFICATION_PAYLOAD_BYTES: usize = 16 * 1024;
pub const MAX_DIAGNOSTIC_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NotificationValueError {
    #[error("notification value must not be empty")]
    Empty,
    #[error("notification value exceeds its configured bound")]
    TooLarge,
    #[error("notification value contains a control character")]
    ControlCharacter,
    #[error("notification rule must contain a trigger and destination")]
    EmptyRule,
}

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 16]);

        impl $name {
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            #[must_use]
            pub const fn as_bytes(self) -> [u8; 16] {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}({})", stringify!($name), hex::encode(self.0))
            }
        }
    };
}

opaque_id!(AlertRuleId);
opaque_id!(NotificationDestinationId);
opaque_id!(NotificationDeliveryId);

#[derive(Clone, PartialEq, Eq)]
pub struct RuleName(Box<str>);

impl RuleName {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, NotificationValueError> {
        let value = value.into();
        validate_text(&value, MAX_RULE_NAME_BYTES)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RuleName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("RuleName").field(&self.0).finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct WebhookEndpoint(Box<str>);

impl WebhookEndpoint {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, NotificationValueError> {
        let value = value.into();
        validate_text(&value, MAX_ENDPOINT_BYTES)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WebhookEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebhookEndpoint(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SealedWebhookSecret(Box<[u8]>);

impl SealedWebhookSecret {
    pub fn new(value: impl Into<Box<[u8]>>) -> Result<Self, NotificationValueError> {
        let value = value.into();
        if value.is_empty() {
            return Err(NotificationValueError::Empty);
        }
        if value.len() > MAX_SEALED_SECRET_BYTES {
            return Err(NotificationValueError::TooLarge);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose_ciphertext(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SealedWebhookSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SealedWebhookSecret(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationDestination {
    pub id: NotificationDestinationId,
    pub project_id: ProjectId,
    pub endpoint: WebhookEndpoint,
    pub sealed_secret: SealedWebhookSecret,
    pub enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertRule {
    pub id: AlertRuleId,
    pub project_id: ProjectId,
    pub name: RuleName,
    pub enabled: bool,
    pub triggers: Box<[IssueNotificationKind]>,
    pub destination_ids: Box<[NotificationDestinationId]>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl AlertRule {
    pub fn validate(&self) -> Result<(), NotificationValueError> {
        if self.triggers.is_empty() || self.destination_ids.is_empty() {
            return Err(NotificationValueError::EmptyRule);
        }
        if self.destination_ids.len() > MAX_RULE_DESTINATIONS {
            return Err(NotificationValueError::TooLarge);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueNotificationTransition {
    pub transition_id: IssueTransitionId,
    pub project_id: ProjectId,
    pub issue_id: IssueId,
    pub kind: IssueNotificationKind,
    pub event_id: EventId,
    pub created_at: Timestamp,
    pub title: IssueTitle,
}

#[derive(Clone, PartialEq, Eq)]
pub struct NotificationPayload(Box<[u8]>);

impl NotificationPayload {
    pub fn new(value: impl Into<Box<[u8]>>) -> Result<Self, NotificationValueError> {
        let value = value.into();
        if value.is_empty() {
            return Err(NotificationValueError::Empty);
        }
        if value.len() > MAX_NOTIFICATION_PAYLOAD_BYTES {
            return Err(NotificationValueError::TooLarge);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for NotificationPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NotificationPayload")
            .field("bytes", &self.0.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationDeliveryStatus {
    Pending,
    Delivered,
    Dead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationDelivery {
    pub id: NotificationDeliveryId,
    pub project_id: ProjectId,
    pub issue_id: IssueId,
    pub transition_id: IssueTransitionId,
    pub rule_id: AlertRuleId,
    pub action_id: NotificationDestinationId,
    pub destination_id: NotificationDestinationId,
    pub payload: NotificationPayload,
    pub status: NotificationDeliveryStatus,
    pub attempts: u32,
    pub next_attempt_at: Timestamp,
    pub last_error: Option<Box<str>>,
    pub created_at: Timestamp,
    pub delivered_at: Option<Timestamp>,
    pub delete_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedNotificationDelivery {
    pub delivery: NotificationDelivery,
    pub destination: NotificationDestination,
    pub attempt: u32,
    pub attempted_at: Timestamp,
}

#[must_use]
pub fn notification_delivery_id(
    transition_id: IssueTransitionId,
    rule_id: AlertRuleId,
    action_id: NotificationDestinationId,
) -> NotificationDeliveryId {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, b"notification-delivery/v1");
    hash_part(&mut hasher, &transition_id.as_bytes());
    hash_part(&mut hasher, &rule_id.as_bytes());
    hash_part(&mut hasher, &action_id.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    NotificationDeliveryId::from_bytes(bytes)
}

fn hash_part(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn validate_text(value: &str, maximum: usize) -> Result<(), NotificationValueError> {
    if value.is_empty() {
        return Err(NotificationValueError::Empty);
    }
    if value.len() > maximum {
        return Err(NotificationValueError::TooLarge);
    }
    if value.chars().any(char::is_control) {
        return Err(NotificationValueError::ControlCharacter);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_identity_is_stable_and_action_scoped() {
        let transition = IssueTransitionId::from_bytes([1; 16]);
        let rule = AlertRuleId::from_bytes([2; 16]);
        let first = NotificationDestinationId::from_bytes([3; 16]);
        let second = NotificationDestinationId::from_bytes([4; 16]);
        assert_eq!(
            notification_delivery_id(transition, rule, first),
            notification_delivery_id(transition, rule, first)
        );
        assert_ne!(
            notification_delivery_id(transition, rule, first),
            notification_delivery_id(transition, rule, second)
        );
    }

    #[test]
    fn secret_and_endpoint_debug_are_redacted() {
        let endpoint = WebhookEndpoint::new("https://secret.example/hook").unwrap();
        let secret = SealedWebhookSecret::new(vec![7; 32]).unwrap();
        assert!(!format!("{endpoint:?}").contains("secret.example"));
        assert!(!format!("{secret:?}").contains('7'));
    }
}
