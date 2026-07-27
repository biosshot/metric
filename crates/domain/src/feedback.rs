//! Bounded User Feedback values and workflow.

use thiserror::Error;

use crate::{
    EventId, ProjectId, Timestamp, blob::EventAttachment, grouping::IssueId, signals::TraceId,
};

pub const MAX_FEEDBACK_MESSAGE_BYTES: usize = 16 * 1024;
pub const MAX_FEEDBACK_NAME_BYTES: usize = 512;
pub const MAX_FEEDBACK_CONTACT_BYTES: usize = 512;
pub const MAX_FEEDBACK_URL_BYTES: usize = 2 * 1024;
pub const MAX_FEEDBACK_ATTACHMENTS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FeedbackValueError {
    #[error("feedback value is invalid")]
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackStatus {
    Open,
    Resolved,
    Spam,
}

impl FeedbackStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Spam => "spam",
        }
    }

    pub fn parse(value: &str) -> Result<Self, FeedbackValueError> {
        match value {
            "open" => Ok(Self::Open),
            "resolved" => Ok(Self::Resolved),
            "spam" => Ok(Self::Spam),
            _ => Err(FeedbackValueError::Invalid),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackRecord {
    pub project_id: ProjectId,
    pub feedback_id: EventId,
    pub received_at: Timestamp,
    pub status: FeedbackStatus,
    pub status_changed_at: Timestamp,
    pub message: Box<str>,
    pub name: Option<Box<str>>,
    pub contact_email: Option<Box<str>>,
    pub url: Option<Box<str>>,
    pub associated_event_id: Option<EventId>,
    pub issue_id: Option<IssueId>,
    pub trace_id: Option<TraceId>,
    pub replay_id: Option<EventId>,
    pub attachments: Vec<EventAttachment>,
    pub expires_at: Timestamp,
}

impl FeedbackRecord {
    pub fn validate(&self) -> Result<(), FeedbackValueError> {
        validate_required(&self.message, MAX_FEEDBACK_MESSAGE_BYTES)?;
        validate_optional(&self.name, MAX_FEEDBACK_NAME_BYTES)?;
        validate_optional(&self.contact_email, MAX_FEEDBACK_CONTACT_BYTES)?;
        validate_optional(&self.url, MAX_FEEDBACK_URL_BYTES)?;
        if self.attachments.len() > MAX_FEEDBACK_ATTACHMENTS
            || self.status_changed_at.unix_millis() < self.received_at.unix_millis()
            || self.expires_at.unix_millis() <= self.received_at.unix_millis()
        {
            return Err(FeedbackValueError::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedbackAnchor {
    pub received_at: Timestamp,
    pub feedback_id: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackPage {
    pub items: Vec<FeedbackRecord>,
    pub next: Option<FeedbackAnchor>,
}

fn validate_required(value: &str, maximum: usize) -> Result<(), FeedbackValueError> {
    if value.trim().is_empty()
        || value.len() > maximum
        || value.chars().any(|character| character == '\0')
    {
        return Err(FeedbackValueError::Invalid);
    }
    Ok(())
}

fn validate_optional(value: &Option<Box<str>>, maximum: usize) -> Result<(), FeedbackValueError> {
    if let Some(value) = value {
        validate_required(value, maximum)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn feedback(seed: u128) -> FeedbackRecord {
        let received_at = Timestamp::from_unix_millis(1_700_000_000_000).unwrap();
        FeedbackRecord {
            project_id: ProjectId::new(7).unwrap(),
            feedback_id: EventId::from_bytes(seed.to_be_bytes()),
            received_at,
            status: FeedbackStatus::Open,
            status_changed_at: received_at,
            message: "The checkout button did not respond".into(),
            name: Some("Ada".into()),
            contact_email: Some("ada@example.com".into()),
            url: Some("https://example.com/checkout".into()),
            associated_event_id: None,
            issue_id: None,
            trace_id: None,
            replay_id: None,
            attachments: Vec::new(),
            expires_at: Timestamp::from_unix_millis(
                received_at.unix_millis() + 90 * 24 * 60 * 60 * 1000,
            )
            .unwrap(),
        }
    }

    #[test]
    fn values_and_statuses_are_bounded() {
        assert!(feedback(1).validate().is_ok());
        let mut oversized = feedback(2);
        oversized.message = "x".repeat(MAX_FEEDBACK_MESSAGE_BYTES + 1).into();
        assert_eq!(oversized.validate(), Err(FeedbackValueError::Invalid));
        assert_eq!(
            FeedbackStatus::parse("resolved"),
            Ok(FeedbackStatus::Resolved)
        );
        assert!(FeedbackStatus::parse("closed").is_err());
    }

    #[test]
    #[ignore = "retained release-mode Phase 31 Feedback validation RPS baseline"]
    fn performance_feedback_validation_rps() {
        const OPERATIONS: u128 = 100_000;
        let started = Instant::now();
        for seed in 1..=OPERATIONS {
            std::hint::black_box(feedback(seed).validate()).unwrap();
        }
        let elapsed = started.elapsed();
        let rps = OPERATIONS as f64 / elapsed.as_secs_f64();
        println!(
            "Phase 31 Feedback validation: rps={rps:.0},operations={OPERATIONS},elapsed_ms={}",
            elapsed.as_millis()
        );
        assert!(rps >= 100_000.0, "Feedback validation RPS regressed: {rps}");
    }
}
