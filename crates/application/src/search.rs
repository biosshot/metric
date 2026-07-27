//! Bounded Search v1 parser, compiler, cursor binding, and exact post-verification.

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

const MAX_QUERY_BYTES: usize = 4_096;
const MAX_PREDICATES: usize = 16;
const MAX_OR_BRANCHES: usize = 8;
const MAX_NESTING: usize = 4;
const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 100;
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
            Self::Syntax => "search_syntax_invalid",
            Self::FieldNotIndexed => "search_field_not_indexed",
            Self::LimitExceeded => "search_limit_exceeded",
            Self::PositiveAnchorRequired => "search_requires_positive_anchor",
            Self::InvalidCursor => "invalid_cursor",
            Self::TooBroad => "search_too_broad",
            Self::NotFound => "not_found",
            Self::Unavailable | Self::InvalidData => "temporarily_unavailable",
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
        text: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<SearchResultPage, SearchError> {
        let page_size = limit.unwrap_or(DEFAULT_PAGE_SIZE);
        if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
            return Err(SearchError::LimitExceeded);
        }
        let parsed = ParsedSearch::parse(text)?;
        let digest = cursor_digest(project_id, &parsed.normalized, CursorKind::Event);
        let before = cursor
            .map(|value| decode_event_cursor(value, digest))
            .transpose()?;
        let branches = parsed.storage_branches(project_id, self.clock.now())?;
        if branches.is_empty() {
            return Ok(SearchResultPage {
                items: Vec::new(),
                next_cursor: None,
                candidates_examined: 0,
            });
        }
        let query = SearchStorageQuery {
            branches,
            before,
            candidate_limit: self.config.max_candidates,
        };
        let candidates = tokio::time::timeout(
            self.config.timeout,
            self.store.search_candidates(project_id, query),
        )
        .await
        .map_err(|_| SearchError::TooBroad)?
        .map_err(map_store_error)?;
        let mut matches = Vec::with_capacity(page_size.saturating_add(1));
        for event in candidates.items {
            if parsed.matches(&event)? {
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

fn map_store_error(error: InvestigationStoreError) -> SearchError {
    match error {
        InvestigationStoreError::NotFound => SearchError::NotFound,
        InvestigationStoreError::InvalidData => SearchError::InvalidData,
        InvestigationStoreError::Unavailable => SearchError::Unavailable,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSearch {
    expression: Expression,
    normalized: String,
}

impl ParsedSearch {
    fn parse(input: &str) -> Result<Self, SearchError> {
        if input.is_empty() || input.len() > MAX_QUERY_BYTES {
            return Err(if input.len() > MAX_QUERY_BYTES {
                SearchError::LimitExceeded
            } else {
                SearchError::Syntax
            });
        }
        let tokens = tokenize(input)?;
        let mut parser = Parser {
            tokens: &tokens,
            position: 0,
            predicates: 0,
        };
        let expression = parser.parse_or(0)?;
        if parser.position != tokens.len() || parser.predicates > MAX_PREDICATES {
            return Err(if parser.predicates > MAX_PREDICATES {
                SearchError::LimitExceeded
            } else {
                SearchError::Syntax
            });
        }
        let normalized = expression.normalized();
        let branches = dnf(&expression, false)?;
        if branches.len() > MAX_OR_BRANCHES {
            return Err(SearchError::LimitExceeded);
        }
        Ok(Self {
            expression,
            normalized,
        })
    }

    fn storage_branches(
        &self,
        project_id: ProjectId,
        now: Timestamp,
    ) -> Result<Vec<SearchStorageBranch>, SearchError> {
        let branches = dnf(&self.expression, false)?;
        let mut output = Vec::with_capacity(branches.len());
        for branch in branches {
            let has_explicit_time = branch.iter().any(|signed| {
                !signed.negated && matches!(signed.predicate, Predicate::Timestamp(_, _))
            });
            let mut from = timestamp_saturating_sub(
                now,
                if has_explicit_time {
                    MAX_RANGE_MILLIS
                } else {
                    DEFAULT_RANGE_MILLIS
                },
            );
            let mut until = timestamp_saturating_add(now, 1);
            let mut selected = SearchStorageAnchor::ProjectTimeline;
            let mut has_positive = false;
            for signed in branch {
                if signed.negated {
                    continue;
                }
                has_positive = true;
                match signed.predicate {
                    Predicate::EventId(id) => {
                        selected = SearchStorageAnchor::Event(EventKey::new(project_id, id));
                    }
                    Predicate::Issue(id)
                        if matches!(selected, SearchStorageAnchor::ProjectTimeline) =>
                    {
                        selected = SearchStorageAnchor::Issue(id);
                    }
                    Predicate::Environment(ref value)
                        if matches!(selected, SearchStorageAnchor::ProjectTimeline) =>
                    {
                        selected = SearchStorageAnchor::Token(SearchToken::environment(value));
                    }
                    Predicate::Release(ref value)
                        if matches!(selected, SearchStorageAnchor::ProjectTimeline) =>
                    {
                        selected = SearchStorageAnchor::Token(SearchToken::release(value));
                    }
                    Predicate::UserId(ref value)
                        if matches!(selected, SearchStorageAnchor::ProjectTimeline) =>
                    {
                        selected = SearchStorageAnchor::Token(SearchToken::user_id(value));
                    }
                    Predicate::Timestamp(comparison, value) => {
                        constrain_range(&mut from, &mut until, comparison, value);
                    }
                    Predicate::Level(_) | Predicate::Platform(_) => {}
                    _ => {}
                }
            }
            if !has_positive {
                return Err(SearchError::PositiveAnchorRequired);
            }
            let identity_timeline = matches!(
                selected,
                SearchStorageAnchor::Event(_) | SearchStorageAnchor::Issue(_)
            );
            if identity_timeline && !has_explicit_time {
                from = Timestamp::from_unix_millis(-62_135_596_800_000)
                    .expect("domain minimum timestamp is valid");
                until = Timestamp::from_unix_millis(253_402_300_799_999)
                    .expect("domain maximum timestamp is valid");
            }
            if from >= until {
                continue;
            }
            if !identity_timeline
                && until.unix_millis().saturating_sub(from.unix_millis()) > MAX_RANGE_MILLIS
            {
                return Err(SearchError::LimitExceeded);
            }
            output.push(SearchStorageBranch {
                anchor: selected,
                from,
                until,
            });
        }
        Ok(output)
    }

    fn matches(&self, event: &EventView) -> Result<bool, SearchError> {
        let payload: Value = serde_json::from_slice(event.payload.as_bytes())
            .map_err(|_| SearchError::InvalidData)?;
        Ok(self.expression.matches(event, &payload))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Expression {
    Predicate(Predicate),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
}

impl Expression {
    fn normalized(&self) -> String {
        match self {
            Self::Predicate(value) => value.normalized(),
            Self::Not(value) => format!("!({})", value.normalized()),
            Self::And(values) => format!(
                "and({})",
                values
                    .iter()
                    .map(Self::normalized)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::Or(values) => format!(
                "or({})",
                values
                    .iter()
                    .map(Self::normalized)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }

    fn matches(&self, event: &EventView, payload: &Value) -> bool {
        match self {
            Self::Predicate(predicate) => predicate.matches(event, payload),
            Self::Not(value) => !value.matches(event, payload),
            Self::And(values) => values.iter().all(|value| value.matches(event, payload)),
            Self::Or(values) => values.iter().any(|value| value.matches(event, payload)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Predicate {
    EventId(EventId),
    Issue(IssueId),
    Timestamp(Comparison, Timestamp),
    Level(Box<str>),
    Platform(Box<str>),
    Environment(Box<str>),
    Release(Box<str>),
    UserId(Box<str>),
}

impl Predicate {
    fn normalized(&self) -> String {
        match self {
            Self::EventId(value) => format!("event.id:{value}"),
            Self::Issue(value) => format!("issue:{value}"),
            Self::Timestamp(comparison, value) => {
                format!("timestamp:{}{}", comparison.symbol(), value.unix_millis())
            }
            Self::Level(value) => format!("level:{}", escaped(value)),
            Self::Platform(value) => format!("platform:{}", escaped(value)),
            Self::Environment(value) => format!("environment:{}", escaped(value)),
            Self::Release(value) => format!("release:{}", escaped(value)),
            Self::UserId(value) => format!("user.id:{}", escaped(value)),
        }
    }

    fn matches(&self, event: &EventView, payload: &Value) -> bool {
        match self {
            Self::EventId(value) => event.key.event_id() == *value,
            Self::Issue(value) => event.issue_id == *value,
            Self::Timestamp(comparison, value) => comparison.matches(event.occurred_at, *value),
            Self::Level(value) => event.level.as_str() == value.as_ref(),
            Self::Platform(value) => event.platform.as_str() == value.as_ref(),
            Self::Environment(value) => {
                payload.get("environment").and_then(Value::as_str) == Some(value.as_ref())
            }
            Self::Release(value) => {
                payload.get("release").and_then(Value::as_str) == Some(value.as_ref())
            }
            Self::UserId(value) => payload
                .get("user")
                .and_then(Value::as_object)
                .and_then(|user| user.get("id"))
                .is_some_and(|candidate| match candidate {
                    Value::String(candidate) => candidate == value.as_ref(),
                    Value::Number(candidate) => candidate.to_string() == value.as_ref(),
                    _ => false,
                }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comparison {
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
}

impl Comparison {
    const fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ge => ">=",
            Self::Gt => ">",
            Self::Le => "<=",
            Self::Lt => "<",
        }
    }

    const fn matches(self, left: Timestamp, right: Timestamp) -> bool {
        match self {
            Self::Eq => left.unix_millis() == right.unix_millis(),
            Self::Ge => left.unix_millis() >= right.unix_millis(),
            Self::Gt => left.unix_millis() > right.unix_millis(),
            Self::Le => left.unix_millis() <= right.unix_millis(),
            Self::Lt => left.unix_millis() < right.unix_millis(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Term(Box<str>, Box<str>),
    Or,
    Bang,
    Left,
    Right,
}

fn tokenize(input: &str) -> Result<Vec<Token>, SearchError> {
    let bytes = input.as_bytes();
    let mut position = 0;
    let mut output = Vec::new();
    while position < bytes.len() {
        if bytes[position].is_ascii_whitespace() {
            position += 1;
            continue;
        }
        match bytes[position] {
            b'(' => {
                output.push(Token::Left);
                position += 1;
            }
            b')' => {
                output.push(Token::Right);
                position += 1;
            }
            b'!' => {
                output.push(Token::Bang);
                position += 1;
            }
            _ if input[position..].starts_with("OR")
                && bytes.get(position + 2).is_none_or(|value| {
                    value.is_ascii_whitespace() || matches!(value, b'(' | b')')
                }) =>
            {
                output.push(Token::Or);
                position += 2;
            }
            _ => {
                let field_start = position;
                while position < bytes.len()
                    && !bytes[position].is_ascii_whitespace()
                    && !matches!(bytes[position], b':' | b'(' | b')' | b'!')
                {
                    position += 1;
                }
                if position == field_start || bytes.get(position) != Some(&b':') {
                    return Err(SearchError::Syntax);
                }
                let field = &input[field_start..position];
                position += 1;
                let value = if bytes.get(position) == Some(&b'"') {
                    position += 1;
                    let mut value = String::new();
                    let mut closed = false;
                    while position < bytes.len() {
                        match bytes[position] {
                            b'"' => {
                                position += 1;
                                closed = true;
                                break;
                            }
                            b'\\'
                                if position + 1 < bytes.len()
                                    && matches!(bytes[position + 1], b'\\' | b'"') =>
                            {
                                value.push(char::from(bytes[position + 1]));
                                position += 2;
                            }
                            byte if byte.is_ascii_control() => return Err(SearchError::Syntax),
                            _ => {
                                let character = input[position..]
                                    .chars()
                                    .next()
                                    .ok_or(SearchError::Syntax)?;
                                value.push(character);
                                position += character.len_utf8();
                            }
                        }
                    }
                    if !closed
                        || bytes.get(position).is_some_and(|value| {
                            !value.is_ascii_whitespace() && !matches!(value, b')' | b'(')
                        })
                    {
                        return Err(SearchError::Syntax);
                    }
                    value
                } else {
                    let value_start = position;
                    while position < bytes.len()
                        && !bytes[position].is_ascii_whitespace()
                        && !matches!(bytes[position], b'(' | b')')
                    {
                        position += 1;
                    }
                    if position == value_start {
                        return Err(SearchError::Syntax);
                    }
                    input[value_start..position].to_owned()
                };
                if value.is_empty() {
                    return Err(SearchError::Syntax);
                }
                output.push(Token::Term(field.into(), value.into()));
            }
        }
    }
    if output.is_empty() {
        return Err(SearchError::Syntax);
    }
    Ok(output)
}

struct Parser<'a> {
    tokens: &'a [Token],
    position: usize,
    predicates: usize,
}

impl Parser<'_> {
    fn parse_or(&mut self, nesting: usize) -> Result<Expression, SearchError> {
        let mut values = vec![self.parse_and(nesting)?];
        while self.tokens.get(self.position) == Some(&Token::Or) {
            self.position += 1;
            values.push(self.parse_and(nesting)?);
        }
        Ok(if values.len() == 1 {
            values.pop().expect("one expression")
        } else {
            Expression::Or(values)
        })
    }

    fn parse_and(&mut self, nesting: usize) -> Result<Expression, SearchError> {
        let mut values = Vec::new();
        while self.position < self.tokens.len()
            && !matches!(self.tokens[self.position], Token::Or | Token::Right)
        {
            values.push(self.parse_unary(nesting)?);
        }
        if values.is_empty() {
            return Err(SearchError::Syntax);
        }
        Ok(if values.len() == 1 {
            values.pop().expect("one expression")
        } else {
            Expression::And(values)
        })
    }

    fn parse_unary(&mut self, nesting: usize) -> Result<Expression, SearchError> {
        if self.tokens.get(self.position) == Some(&Token::Bang) {
            self.position += 1;
            return Ok(Expression::Not(Box::new(self.parse_unary(nesting)?)));
        }
        match self.tokens.get(self.position) {
            Some(Token::Left) => {
                if nesting >= MAX_NESTING {
                    return Err(SearchError::LimitExceeded);
                }
                self.position += 1;
                let expression = self.parse_or(nesting + 1)?;
                if self.tokens.get(self.position) != Some(&Token::Right) {
                    return Err(SearchError::Syntax);
                }
                self.position += 1;
                Ok(expression)
            }
            Some(Token::Term(field, value)) => {
                self.position += 1;
                self.predicates += 1;
                if self.predicates > MAX_PREDICATES {
                    return Err(SearchError::LimitExceeded);
                }
                Ok(Expression::Predicate(parse_predicate(field, value)?))
            }
            _ => Err(SearchError::Syntax),
        }
    }
}

fn parse_predicate(field: &str, raw: &str) -> Result<Predicate, SearchError> {
    match field {
        "event.id" => EventId::parse(raw)
            .map(Predicate::EventId)
            .map_err(|_| SearchError::Syntax),
        "issue" => parse_hex_16(raw)
            .map(IssueId::from_bytes)
            .map(Predicate::Issue)
            .ok_or(SearchError::Syntax),
        "timestamp" => {
            let (comparison, value) = if let Some(value) = raw.strip_prefix(">=") {
                (Comparison::Ge, value)
            } else if let Some(value) = raw.strip_prefix("<=") {
                (Comparison::Le, value)
            } else if let Some(value) = raw.strip_prefix('>') {
                (Comparison::Gt, value)
            } else if let Some(value) = raw.strip_prefix('<') {
                (Comparison::Lt, value)
            } else if let Some(value) = raw.strip_prefix('=') {
                (Comparison::Eq, value)
            } else {
                (Comparison::Eq, raw)
            };
            parse_timestamp(value).map(|value| Predicate::Timestamp(comparison, value))
        }
        "level" if matches!(raw, "debug" | "info" | "warning" | "error" | "fatal") => {
            Ok(Predicate::Level(raw.into()))
        }
        "platform" => Ok(Predicate::Platform(raw.into())),
        "environment" => Ok(Predicate::Environment(raw.into())),
        "release" => Ok(Predicate::Release(raw.into())),
        "user.id" => Ok(Predicate::UserId(raw.into())),
        "level" => Err(SearchError::Syntax),
        _ => Err(SearchError::FieldNotIndexed),
    }
}

fn parse_timestamp(value: &str) -> Result<Timestamp, SearchError> {
    let parsed = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| SearchError::Syntax)?;
    let millis = parsed.unix_timestamp_nanos().div_euclid(1_000_000);
    i64::try_from(millis)
        .ok()
        .and_then(|value| Timestamp::from_unix_millis(value).ok())
        .ok_or(SearchError::Syntax)
}

#[derive(Debug, Clone)]
struct SignedPredicate {
    predicate: Predicate,
    negated: bool,
}

fn dnf(expression: &Expression, negated: bool) -> Result<Vec<Vec<SignedPredicate>>, SearchError> {
    let result = match expression {
        Expression::Predicate(predicate) => vec![vec![SignedPredicate {
            predicate: predicate.clone(),
            negated,
        }]],
        Expression::Not(value) => dnf(value, !negated)?,
        Expression::And(values) if !negated => {
            let mut result = vec![Vec::new()];
            for value in values {
                result = cross_product(result, dnf(value, false)?)?;
            }
            result
        }
        Expression::Or(values) if negated => {
            let mut result = vec![Vec::new()];
            for value in values {
                result = cross_product(result, dnf(value, true)?)?;
            }
            result
        }
        Expression::Or(values) => {
            let mut result = Vec::new();
            for value in values {
                result.extend(dnf(value, false)?);
                if result.len() > MAX_OR_BRANCHES {
                    return Err(SearchError::LimitExceeded);
                }
            }
            result
        }
        Expression::And(values) => {
            let mut result = Vec::new();
            for value in values {
                result.extend(dnf(value, true)?);
                if result.len() > MAX_OR_BRANCHES {
                    return Err(SearchError::LimitExceeded);
                }
            }
            result
        }
    };
    Ok(result)
}

fn cross_product(
    left: Vec<Vec<SignedPredicate>>,
    right: Vec<Vec<SignedPredicate>>,
) -> Result<Vec<Vec<SignedPredicate>>, SearchError> {
    if left.len().saturating_mul(right.len()) > MAX_OR_BRANCHES {
        return Err(SearchError::LimitExceeded);
    }
    let mut result = Vec::with_capacity(left.len().saturating_mul(right.len()));
    for left in left {
        for right in &right {
            let mut branch = left.clone();
            branch.extend(right.iter().cloned());
            result.push(branch);
        }
    }
    Ok(result)
}

fn constrain_range(
    from: &mut Timestamp,
    until: &mut Timestamp,
    comparison: Comparison,
    value: Timestamp,
) {
    match comparison {
        Comparison::Eq => {
            *from = (*from).max(value);
            *until = (*until).min(timestamp_saturating_add(value, 1));
        }
        Comparison::Ge => *from = (*from).max(value),
        Comparison::Gt => *from = (*from).max(timestamp_saturating_add(value, 1)),
        Comparison::Le => *until = (*until).min(timestamp_saturating_add(value, 1)),
        Comparison::Lt => *until = (*until).min(value),
    }
}

fn timestamp_saturating_add(timestamp: Timestamp, millis: i64) -> Timestamp {
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_add(millis)).unwrap_or(timestamp)
}

fn timestamp_saturating_sub(timestamp: Timestamp, millis: i64) -> Timestamp {
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_sub(millis)).unwrap_or(timestamp)
}

fn escaped(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
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
    fn grammar_normalizes_boolean_queries_and_rejects_unsupported_fields() {
        let parsed = ParsedSearch::parse(
            r#"(environment:"Production West" release:backend@2 OR !level:debug) platform:rust"#,
        )
        .unwrap();
        assert!(parsed.normalized.contains("environment"));
        assert_eq!(
            ParsedSearch::parse("message:panic"),
            Err(SearchError::FieldNotIndexed)
        );
        assert_eq!(ParsedSearch::parse("bare text"), Err(SearchError::Syntax));
    }

    #[test]
    fn grammar_enforces_predicate_branch_and_nesting_bounds() {
        let predicates = (0..17).map(|_| "level:error").collect::<Vec<_>>().join(" ");
        assert_eq!(
            ParsedSearch::parse(&predicates),
            Err(SearchError::LimitExceeded)
        );
        let branches = (0..9)
            .map(|index| format!("environment:e{index}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        assert_eq!(
            ParsedSearch::parse(&branches),
            Err(SearchError::LimitExceeded)
        );
        assert_eq!(
            ParsedSearch::parse("(((((level:error)))))"),
            Err(SearchError::LimitExceeded)
        );
    }

    #[test]
    fn cursor_is_golden_and_bound_to_project_query_and_kind() {
        let project = ProjectId::new(7).unwrap();
        let digest = cursor_digest(project, "level:\"error\"", CursorKind::Event);
        let timestamp = Timestamp::from_unix_millis(1_700_000_000_123).unwrap();
        let key = EventKey::new(project, EventId::from_bytes([9; 16]));
        let cursor = encode_cursor(CursorKind::Event, timestamp, &key.as_bytes(), digest);
        assert_eq!(
            cursor,
            "01020000018bcfe5687b140000000709090909090909090909090909090909bb4fa713fd15d704be738ca462b32ab2"
        );
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
                cursor_digest(project, "level:\"fatal\"", CursorKind::Event)
            ),
            Err(SearchError::InvalidCursor)
        );
    }

    #[test]
    fn timestamps_and_quoted_values_are_exact() {
        let parsed = ParsedSearch::parse(
            r#"timestamp:>=2026-07-20T00:00:00Z timestamp:<2026-07-21T00:00:00Z release:"A B""#,
        )
        .unwrap();
        assert!(parsed.normalized.contains("1784505600000"));
        assert!(parsed.normalized.contains(r#"release:"A B""#));
    }
}
