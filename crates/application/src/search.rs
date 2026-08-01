//! Bounded Error-record adapter for the Unified Query v2 language.

use std::{sync::Arc, time::Duration};

use metric_domain::{
    EventId, EventKey, ProjectId, Timestamp,
    api::{EventAnchor, EventView, SearchStorageAnchor, SearchStorageBranch, SearchStorageQuery},
    finalization::SearchToken,
    grouping::IssueId,
};
use metric_ports::{Clock, InvestigationStore, InvestigationStoreError};
use serde_json::Value;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::query::{
    DEFAULT_QUERY_ROWS, MAX_QUERY_ROWS, ParsedQuery, QueryExpression, QueryField, QueryOperator,
    QueryPredicate, QuerySource,
};

const MAX_CANDIDATES: usize = 10_000;
const DAY_MILLIS: i64 = 86_400_000;
const DEFAULT_RANGE_MILLIS: i64 = DAY_MILLIS;
const MAX_RANGE_MILLIS: i64 = 30 * DAY_MILLIS;

#[derive(Debug, Clone, Copy)]
pub struct SearchConfig {
    pub timeout: Duration,
    pub max_candidates: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            max_candidates: MAX_CANDIDATES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SearchError {
    #[error("search syntax is invalid")]
    Syntax,
    #[error("search field is not indexed")]
    FieldNotIndexed,
    #[error("search bounds are exceeded")]
    LimitExceeded,
    #[error("search requires a positive project, time, identity, or token anchor")]
    PositiveAnchorRequired,
    #[error("search cursor is invalid")]
    InvalidCursor,
    #[error("search was too broad for the bounded candidate budget")]
    TooBroad,
    #[error("search target does not exist")]
    NotFound,
    #[error("search storage is temporarily unavailable")]
    Unavailable,
    #[error("search storage returned invalid data")]
    InvalidData,
}

impl SearchError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Syntax => "query_syntax_invalid",
            Self::FieldNotIndexed => "query_capability_unavailable",
            Self::LimitExceeded => "query_limit_exceeded",
            Self::PositiveAnchorRequired => "query_requires_positive_anchor",
            Self::InvalidCursor => "invalid_cursor",
            Self::TooBroad => "query_too_broad",
            Self::NotFound => "not_found",
            Self::Unavailable | Self::InvalidData => "query_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultPage {
    pub items: Vec<EventView>,
    pub next_cursor: Option<String>,
    pub candidates_examined: usize,
}

pub struct SearchService {
    store: Arc<dyn InvestigationStore>,
    clock: Arc<dyn Clock>,
    config: SearchConfig,
}

impl SearchService {
    pub fn new(
        store: Arc<dyn InvestigationStore>,
        clock: Arc<dyn Clock>,
        config: SearchConfig,
    ) -> Result<Self, SearchError> {
        if config.timeout.is_zero() || !(1..=MAX_CANDIDATES).contains(&config.max_candidates) {
            return Err(SearchError::LimitExceeded);
        }
        Ok(Self {
            store,
            clock,
            config,
        })
    }

    pub async fn search(
        &self,
        project_id: ProjectId,
        parsed: &ParsedQuery,
        from: Option<Timestamp>,
        until: Option<Timestamp>,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<SearchResultPage, SearchError> {
        if parsed.source != QuerySource::Errors {
            return Err(SearchError::FieldNotIndexed);
        }
        let page_size = limit.unwrap_or(DEFAULT_QUERY_ROWS);
        if !(1..=MAX_QUERY_ROWS).contains(&page_size) {
            return Err(SearchError::LimitExceeded);
        }
        let now = self.clock.now();
        let (from, until) = error_time_range(now, from, until)?;
        let normalized = format!(
            "{}|from:{}|until:{}",
            parsed.normalized,
            from.unix_millis(),
            until.unix_millis()
        );
        let digest = cursor_digest(project_id, &normalized, CursorKind::Event);
        let before = cursor
            .map(|value| decode_event_cursor(value, digest))
            .transpose()?;
        let branches = storage_branches(project_id, parsed.expression.as_ref(), from, until)?;
        let candidates = tokio::time::timeout(
            self.config.timeout,
            self.store.search_candidates(
                project_id,
                SearchStorageQuery {
                    branches,
                    before,
                    candidate_limit: self.config.max_candidates,
                },
            ),
        )
        .await
        .map_err(|_| SearchError::TooBroad)?
        .map_err(map_store_error)?;
        let mut matches = Vec::with_capacity(page_size.saturating_add(1));
        for event in candidates.items {
            if matches_event(parsed.expression.as_ref(), &event)? {
                matches.push(event);
                if matches.len() > page_size {
                    break;
                }
            }
        }
        if matches.len() <= page_size
            && candidates.candidates_examined >= self.config.max_candidates
        {
            return Err(SearchError::TooBroad);
        }
        let has_more = matches.len() > page_size;
        matches.truncate(page_size);
        let next_cursor = has_more.then(|| matches.last()).flatten().map(|event| {
            encode_cursor(
                CursorKind::Event,
                event.occurred_at,
                &event.key.as_bytes(),
                digest,
            )
        });
        Ok(SearchResultPage {
            items: matches,
            next_cursor,
            candidates_examined: candidates.candidates_examined,
        })
    }
}

fn error_time_range(
    now: Timestamp,
    from: Option<Timestamp>,
    until: Option<Timestamp>,
) -> Result<(Timestamp, Timestamp), SearchError> {
    let until = until.unwrap_or_else(|| timestamp_saturating_add(now, 1));
    let from = from.unwrap_or_else(|| timestamp_saturating_sub(until, DEFAULT_RANGE_MILLIS));
    if from >= until || until.unix_millis().saturating_sub(from.unix_millis()) > MAX_RANGE_MILLIS {
        return Err(SearchError::LimitExceeded);
    }
    Ok((from, until))
}

fn storage_branches(
    project_id: ProjectId,
    expression: Option<&QueryExpression>,
    from: Timestamp,
    until: Timestamp,
) -> Result<Vec<SearchStorageBranch>, SearchError> {
    let expressions = match expression {
        Some(QueryExpression::Or(values)) => values.iter().collect::<Vec<_>>(),
        Some(value) => vec![value],
        None => Vec::new(),
    };
    if expressions.is_empty() {
        return Ok(vec![SearchStorageBranch {
            anchor: SearchStorageAnchor::ProjectTimeline,
            from,
            until,
        }]);
    }
    expressions
        .into_iter()
        .map(|value| {
            Ok(SearchStorageBranch {
                anchor: guaranteed_anchor(project_id, value)
                    .unwrap_or(SearchStorageAnchor::ProjectTimeline),
                from,
                until,
            })
        })
        .collect()
}

fn guaranteed_anchor(
    project_id: ProjectId,
    expression: &QueryExpression,
) -> Option<SearchStorageAnchor> {
    match expression {
        QueryExpression::Predicate(predicate) if predicate.operator == QueryOperator::Equal => {
            match predicate.field {
                QueryField::EventId => EventId::parse(&predicate.value)
                    .ok()
                    .map(|value| SearchStorageAnchor::Event(EventKey::new(project_id, value))),
                QueryField::IssueId => parse_hex_16(&predicate.value)
                    .map(IssueId::from_bytes)
                    .map(SearchStorageAnchor::Issue),
                QueryField::Environment => Some(SearchStorageAnchor::Token(
                    SearchToken::environment(&predicate.value),
                )),
                QueryField::Release => Some(SearchStorageAnchor::Token(SearchToken::release(
                    &predicate.value,
                ))),
                QueryField::UserId => Some(SearchStorageAnchor::Token(SearchToken::user_id(
                    &predicate.value,
                ))),
                _ => None,
            }
        }
        QueryExpression::And(values) => values
            .iter()
            .find_map(|value| guaranteed_anchor(project_id, value)),
        QueryExpression::Not(_) | QueryExpression::Or(_) | QueryExpression::Predicate(_) => None,
    }
}

fn matches_event(
    expression: Option<&QueryExpression>,
    event: &EventView,
) -> Result<bool, SearchError> {
    let Some(expression) = expression else {
        return Ok(true);
    };
    let payload: Value =
        serde_json::from_slice(event.payload.as_bytes()).map_err(|_| SearchError::InvalidData)?;
    Ok(matches_expression(expression, event, &payload))
}

fn matches_expression(expression: &QueryExpression, event: &EventView, payload: &Value) -> bool {
    match expression {
        QueryExpression::Predicate(predicate) => matches_predicate(predicate, event, payload),
        QueryExpression::Not(value) => !matches_expression(value, event, payload),
        QueryExpression::And(values) => values
            .iter()
            .all(|value| matches_expression(value, event, payload)),
        QueryExpression::Or(values) => values
            .iter()
            .any(|value| matches_expression(value, event, payload)),
    }
}

fn matches_predicate(predicate: &QueryPredicate, event: &EventView, payload: &Value) -> bool {
    let candidate = match predicate.field {
        QueryField::EventId => event.key.event_id().to_string(),
        QueryField::IssueId => event.issue_id.to_string(),
        QueryField::Timestamp => {
            return parse_timestamp(&predicate.value).is_some_and(|value| {
                compare_i64(event.occurred_at.unix_millis(), value, predicate.operator)
            });
        }
        QueryField::Level => event.level.as_str().to_owned(),
        QueryField::Platform => event.platform.as_str().to_owned(),
        QueryField::Environment => payload
            .get("environment")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        QueryField::Release => payload
            .get("release")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        QueryField::UserId => payload
            .get("user")
            .and_then(Value::as_object)
            .and_then(|user| user.get("id"))
            .map(|value| match value {
                Value::String(value) => value.clone(),
                Value::Number(value) => value.to_string(),
                _ => String::new(),
            })
            .unwrap_or_default(),
        _ => return false,
    };
    predicate.operator == QueryOperator::Equal && candidate == predicate.value.as_ref()
}

fn compare_i64(left: i64, right: i64, operator: QueryOperator) -> bool {
    match operator {
        QueryOperator::Equal => left == right,
        QueryOperator::Greater => left > right,
        QueryOperator::GreaterOrEqual => left >= right,
        QueryOperator::Less => left < right,
        QueryOperator::LessOrEqual => left <= right,
        QueryOperator::Contains => false,
    }
}

fn parse_timestamp(value: &str) -> Option<i64> {
    value.parse::<i64>().ok().or_else(|| {
        OffsetDateTime::parse(value, &Rfc3339)
            .ok()
            .and_then(|value| {
                i64::try_from(value.unix_timestamp_nanos().div_euclid(1_000_000)).ok()
            })
    })
}

fn map_store_error(error: InvestigationStoreError) -> SearchError {
    match error {
        InvestigationStoreError::NotFound => SearchError::NotFound,
        InvestigationStoreError::InvalidData => SearchError::InvalidData,
        InvestigationStoreError::Unavailable => SearchError::Unavailable,
    }
}

fn timestamp_saturating_add(timestamp: Timestamp, millis: i64) -> Timestamp {
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_add(millis)).unwrap_or(timestamp)
}

fn timestamp_saturating_sub(timestamp: Timestamp, millis: i64) -> Timestamp {
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_sub(millis)).unwrap_or(timestamp)
}

fn parse_hex_16(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(value, &mut bytes).ok()?;
    Some(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CursorKind {
    Issue = 1,
    Event = 2,
    Activity = 3,
    Release = 4,
    Environment = 5,
    Feedback = 6,
    MonitorRun = 7,
    Replay = 8,
}

#[must_use]
pub fn cursor_digest(project_id: ProjectId, normalized: &str, kind: CursorKind) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric/api-cursor/v1");
    hasher.update(&[kind as u8]);
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(&(normalized.len() as u64).to_be_bytes());
    hasher.update(normalized.as_bytes());
    hasher.finalize().as_bytes()[..16]
        .try_into()
        .expect("digest prefix")
}

#[must_use]
pub fn encode_cursor(
    kind: CursorKind,
    timestamp: Timestamp,
    id: &[u8],
    digest: [u8; 16],
) -> String {
    let mut value = Vec::with_capacity(27 + id.len());
    value.push(1);
    value.push(kind as u8);
    value.extend_from_slice(&timestamp.unix_millis().to_be_bytes());
    value.push(u8::try_from(id.len()).expect("cursor identifier is bounded"));
    value.extend_from_slice(id);
    value.extend_from_slice(&digest);
    hex::encode(value)
}

pub fn decode_cursor(
    encoded: &str,
    kind: CursorKind,
    id_len: usize,
    digest: [u8; 16],
) -> Result<(Timestamp, Vec<u8>), SearchError> {
    if encoded.len() > 128 {
        return Err(SearchError::InvalidCursor);
    }
    let value = hex::decode(encoded).map_err(|_| SearchError::InvalidCursor)?;
    if value.len() != 27 + id_len
        || value[0] != 1
        || value[1] != kind as u8
        || usize::from(value[10]) != id_len
        || value[(11 + id_len)..] != digest
    {
        return Err(SearchError::InvalidCursor);
    }
    let timestamp = Timestamp::from_unix_millis(i64::from_be_bytes(
        value[2..10]
            .try_into()
            .map_err(|_| SearchError::InvalidCursor)?,
    ))
    .map_err(|_| SearchError::InvalidCursor)?;
    Ok((timestamp, value[11..(11 + id_len)].to_vec()))
}

fn decode_event_cursor(encoded: &str, digest: [u8; 16]) -> Result<EventAnchor, SearchError> {
    let (occurred_at, id) = decode_cursor(encoded, CursorKind::Event, 20, digest)?;
    Ok(EventAnchor {
        occurred_at,
        event_key: EventKey::from_bytes(id.try_into().map_err(|_| SearchError::InvalidCursor)?)
            .map_err(|_| SearchError::InvalidCursor)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_is_golden_and_bound_to_project_query_and_kind() {
        let project = ProjectId::new(7).unwrap();
        let digest = cursor_digest(project, "level:eq:error", CursorKind::Event);
        let timestamp = Timestamp::from_unix_millis(1_700_000_000_123).unwrap();
        let key = EventKey::new(project, EventId::from_bytes([9; 16]));
        let cursor = encode_cursor(CursorKind::Event, timestamp, &key.as_bytes(), digest);
        assert_eq!(
            decode_event_cursor(&cursor, digest).unwrap(),
            EventAnchor {
                occurred_at: timestamp,
                event_key: key,
            }
        );
        assert_eq!(
            decode_event_cursor(
                &cursor,
                cursor_digest(project, "level:eq:fatal", CursorKind::Event)
            ),
            Err(SearchError::InvalidCursor)
        );
    }
}
