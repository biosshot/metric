//! Unified Query v2 language, source schemas and stable normalization.

use std::fmt::Write;

use metric_domain::explore::{
    ExploreDataset, ExploreExpression, ExploreField, ExplorePredicate, ExplorePredicateOp,
    ExploreValue,
};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub const MAX_QUERY_BYTES: usize = 32 * 1024;
pub const MAX_QUERY_NODES: usize = 256;
pub const MAX_QUERY_PREDICATES: usize = 128;
pub const MAX_QUERY_OR_ALTERNATIVES: usize = 64;
pub const MAX_QUERY_NESTING: usize = 16;
pub const DEFAULT_QUERY_ROWS: usize = 50;
pub const MAX_QUERY_ROWS: usize = 500;
pub const MAX_QUERY_VALUES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QuerySource {
    Issues,
    Errors,
    Logs,
    Traces,
    Metrics,
    Replays,
    Feedback,
    Releases,
}

impl QuerySource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Issues => "issues",
            Self::Errors => "errors",
            Self::Logs => "logs",
            Self::Traces => "traces",
            Self::Metrics => "metrics",
            Self::Replays => "replays",
            Self::Feedback => "feedback",
            Self::Releases => "releases",
        }
    }

    pub fn parse(value: &str) -> Result<Self, QueryError> {
        match value {
            "issues" => Ok(Self::Issues),
            "errors" => Ok(Self::Errors),
            "logs" => Ok(Self::Logs),
            "traces" => Ok(Self::Traces),
            "metrics" => Ok(Self::Metrics),
            "replays" => Ok(Self::Replays),
            "feedback" => Ok(Self::Feedback),
            "releases" => Ok(Self::Releases),
            _ => Err(QueryError::CapabilityUnavailable),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QueryField {
    EventId,
    IssueId,
    FeedbackId,
    ReplayId,
    Timestamp,
    ReceivedAt,
    Title,
    Level,
    Platform,
    Message,
    Environment,
    Release,
    Service,
    TraceId,
    SpanId,
    DurationMs,
    OperationClass,
    Operation,
    Status,
    Name,
    Url,
    UserId,
    IsSegment,
    MetricName,
    MetricKind,
    Unit,
    MetricCount,
    MetricSum,
    MetricMin,
    MetricMax,
}

impl QueryField {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EventId => "event.id",
            Self::IssueId => "issue_id",
            Self::FeedbackId => "feedback_id",
            Self::ReplayId => "replay_id",
            Self::Timestamp => "timestamp",
            Self::ReceivedAt => "received_at",
            Self::Title => "title",
            Self::Level => "level",
            Self::Platform => "platform",
            Self::Message => "message",
            Self::Environment => "environment",
            Self::Release => "release",
            Self::Service => "service",
            Self::TraceId => "trace_id",
            Self::SpanId => "span_id",
            Self::DurationMs => "duration_ms",
            Self::OperationClass => "operation_class",
            Self::Operation => "operation",
            Self::Status => "status",
            Self::Name => "name",
            Self::Url => "url",
            Self::UserId => "user.id",
            Self::IsSegment => "is_segment",
            Self::MetricName => "metric_name",
            Self::MetricKind => "metric_kind",
            Self::Unit => "unit",
            Self::MetricCount => "metric_count",
            Self::MetricSum => "metric_sum",
            Self::MetricMin => "metric_min",
            Self::MetricMax => "metric_max",
        }
    }
}

pub fn parse_query_field(source: QuerySource, value: &str) -> Result<QueryField, QueryError> {
    let field = resolve_field(value).ok_or(QueryError::CapabilityUnavailable)?;
    if field_accepted(source, field) {
        Ok(field)
    } else {
        Err(QueryError::CapabilityUnavailable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryOperator {
    Equal,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
    Contains,
}

impl QueryOperator {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Equal => "eq",
            Self::Greater => "gt",
            Self::GreaterOrEqual => "gte",
            Self::Less => "lt",
            Self::LessOrEqual => "lte",
            Self::Contains => "contains",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPredicate {
    pub field: QueryField,
    pub operator: QueryOperator,
    pub value: Box<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryExpression {
    Predicate(QueryPredicate),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
}

impl QueryExpression {
    #[must_use]
    pub fn normalized(&self) -> String {
        match self {
            Self::Predicate(predicate) => format!(
                "{}:{}:{}",
                predicate.field.as_str(),
                predicate.operator.as_str(),
                escaped(&predicate.value)
            ),
            Self::Not(value) => format!("not({})", value.normalized()),
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

    #[must_use]
    pub fn predicates(&self) -> Vec<&QueryPredicate> {
        let mut output = Vec::new();
        self.collect_predicates(&mut output);
        output
    }

    fn collect_predicates<'a>(&'a self, output: &mut Vec<&'a QueryPredicate>) {
        match self {
            Self::Predicate(value) => output.push(value),
            Self::Not(value) => value.collect_predicates(output),
            Self::And(values) | Self::Or(values) => {
                for value in values {
                    value.collect_predicates(output);
                }
            }
        }
    }
}

pub fn matches_expression(
    expression: Option<&QueryExpression>,
    predicate: &mut impl FnMut(&QueryPredicate) -> bool,
) -> bool {
    fn matches_inner(
        expression: &QueryExpression,
        predicate: &mut impl FnMut(&QueryPredicate) -> bool,
    ) -> bool {
        match expression {
            QueryExpression::Predicate(value) => predicate(value),
            QueryExpression::Not(value) => !matches_inner(value, predicate),
            QueryExpression::And(values) => {
                values.iter().all(|value| matches_inner(value, predicate))
            }
            QueryExpression::Or(values) => {
                values.iter().any(|value| matches_inner(value, predicate))
            }
        }
    }
    expression.is_none_or(|value| matches_inner(value, predicate))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    pub source: QuerySource,
    pub expression: Option<QueryExpression>,
    pub normalized: Box<str>,
}

impl ParsedQuery {
    pub fn parse(source: QuerySource, input: &str) -> Result<Self, QueryError> {
        let input = input.trim();
        if input.len() > MAX_QUERY_BYTES {
            return Err(QueryError::LimitExceeded);
        }
        if input.is_empty() {
            return Ok(Self {
                source,
                expression: None,
                normalized: format!("v2|{}|all", source.as_str()).into(),
            });
        }
        let tokens = tokenize(input)?;
        let mut parser = Parser {
            source,
            tokens: &tokens,
            position: 0,
            predicates: 0,
            nodes: 0,
            or_alternatives: 0,
        };
        let expression = parser.parse_or(0)?;
        if parser.position != tokens.len() {
            return Err(QueryError::Syntax);
        }
        let normalized = format!("v2|{}|{}", source.as_str(), expression.normalized()).into();
        Ok(Self {
            source,
            expression: Some(expression),
            normalized,
        })
    }

    pub fn explore_expression(&self) -> Result<Option<ExploreExpression>, QueryError> {
        let dataset = match self.source {
            QuerySource::Errors => ExploreDataset::Errors,
            QuerySource::Logs => ExploreDataset::Logs,
            QuerySource::Traces => ExploreDataset::Spans,
            QuerySource::Metrics => ExploreDataset::Metrics,
            _ => return Err(QueryError::CapabilityUnavailable),
        };
        self.expression
            .as_ref()
            .map(|value| to_explore_expression(dataset, value))
            .transpose()
    }
}

fn to_explore_expression(
    dataset: ExploreDataset,
    expression: &QueryExpression,
) -> Result<ExploreExpression, QueryError> {
    Ok(match expression {
        QueryExpression::Predicate(predicate) => {
            ExploreExpression::Predicate(to_explore_predicate(dataset, predicate)?)
        }
        QueryExpression::Not(value) => {
            ExploreExpression::Not(Box::new(to_explore_expression(dataset, value)?))
        }
        QueryExpression::And(values) => ExploreExpression::And(
            values
                .iter()
                .map(|value| to_explore_expression(dataset, value))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        QueryExpression::Or(values) => ExploreExpression::Or(
            values
                .iter()
                .map(|value| to_explore_expression(dataset, value))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    })
}

fn to_explore_predicate(
    dataset: ExploreDataset,
    predicate: &QueryPredicate,
) -> Result<ExplorePredicate, QueryError> {
    let field = match predicate.field {
        QueryField::Timestamp => ExploreField::Timestamp,
        QueryField::ReceivedAt => ExploreField::ReceivedAt,
        QueryField::Level => ExploreField::Level,
        QueryField::Platform => ExploreField::Platform,
        QueryField::IssueId => ExploreField::IssueId,
        QueryField::Message => ExploreField::Message,
        QueryField::Environment => ExploreField::Environment,
        QueryField::Release => ExploreField::Release,
        QueryField::Service => ExploreField::Service,
        QueryField::TraceId => ExploreField::TraceId,
        QueryField::SpanId => ExploreField::SpanId,
        QueryField::DurationMs => ExploreField::DurationMs,
        QueryField::OperationClass => ExploreField::OperationClass,
        QueryField::Operation => ExploreField::Operation,
        QueryField::Status => ExploreField::Status,
        QueryField::Name => ExploreField::Name,
        QueryField::IsSegment => ExploreField::IsSegment,
        QueryField::MetricName => ExploreField::Name,
        QueryField::MetricKind => ExploreField::MetricKind,
        QueryField::Unit => ExploreField::Unit,
        QueryField::MetricCount => ExploreField::MetricCount,
        QueryField::MetricSum => ExploreField::MetricSum,
        QueryField::MetricMin => ExploreField::MetricMin,
        QueryField::MetricMax => ExploreField::MetricMax,
        _ => return Err(QueryError::CapabilityUnavailable),
    };
    if !field.accepted_by(dataset) {
        return Err(QueryError::CapabilityUnavailable);
    }
    let op = match predicate.operator {
        QueryOperator::Equal => ExplorePredicateOp::Exact,
        QueryOperator::Greater => ExplorePredicateOp::Greater,
        QueryOperator::GreaterOrEqual => ExplorePredicateOp::GreaterOrEqual,
        QueryOperator::Less => ExplorePredicateOp::Less,
        QueryOperator::LessOrEqual => ExplorePredicateOp::LessOrEqual,
        QueryOperator::Contains => ExplorePredicateOp::Contains,
    };
    Ok(ExplorePredicate {
        field,
        op,
        value: Some(explore_value(field, &predicate.value)?),
        upper: None,
    })
}

fn explore_value(field: ExploreField, value: &str) -> Result<ExploreValue, QueryError> {
    match field {
        ExploreField::Timestamp | ExploreField::ReceivedAt => {
            let millis = value.parse::<i64>().ok().or_else(|| {
                OffsetDateTime::parse(value, &Rfc3339)
                    .ok()
                    .and_then(|value| {
                        i64::try_from(value.unix_timestamp_nanos().div_euclid(1_000_000)).ok()
                    })
            });
            millis.map(ExploreValue::Integer).ok_or(QueryError::Syntax)
        }
        ExploreField::DurationMs
        | ExploreField::MetricSum
        | ExploreField::MetricMin
        | ExploreField::MetricMax => value
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(ExploreValue::Number)
            .ok_or(QueryError::Syntax),
        ExploreField::MetricCount => value
            .parse::<i64>()
            .ok()
            .map(ExploreValue::Integer)
            .ok_or(QueryError::Syntax),
        ExploreField::IsSegment => value
            .parse::<bool>()
            .ok()
            .map(ExploreValue::Bool)
            .ok_or(QueryError::Syntax),
        _ => Ok(ExploreValue::String(value.into())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum QueryError {
    #[error("query syntax is invalid")]
    Syntax,
    #[error("query capability is unavailable for this source")]
    CapabilityUnavailable,
    #[error("query structural limit is exceeded")]
    LimitExceeded,
    #[error("query cost budget is exceeded")]
    CostExceeded,
    #[error("query capacity is exhausted")]
    Capacity,
    #[error("query cursor is invalid")]
    InvalidCursor,
    #[error("query storage is temporarily unavailable")]
    Unavailable,
}

impl QueryError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Syntax => "query_syntax_invalid",
            Self::CapabilityUnavailable => "query_capability_unavailable",
            Self::LimitExceeded => "query_limit_exceeded",
            Self::CostExceeded => "query_cost_exceeded",
            Self::Capacity => "query_capacity",
            Self::InvalidCursor => "invalid_cursor",
            Self::Unavailable => "query_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Predicate(Box<str>, Box<str>),
    Bare(Box<str>),
    And,
    Or,
    Bang,
    Left,
    Right,
}

fn tokenize(input: &str) -> Result<Vec<Token>, QueryError> {
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
            b'"' => {
                output.push(Token::Bare(read_quoted(input, &mut position)?.into()));
            }
            _ => {
                let start = position;
                while position < bytes.len()
                    && !bytes[position].is_ascii_whitespace()
                    && !matches!(bytes[position], b':' | b'(' | b')' | b'!')
                {
                    position += 1;
                }
                if position == start {
                    return Err(QueryError::Syntax);
                }
                let word = &input[start..position];
                if bytes.get(position) == Some(&b':') {
                    position += 1;
                    let value = if bytes.get(position) == Some(&b'"') {
                        read_quoted(input, &mut position)?
                    } else {
                        let value_start = position;
                        while position < bytes.len()
                            && !bytes[position].is_ascii_whitespace()
                            && !matches!(bytes[position], b'(' | b')')
                        {
                            position += 1;
                        }
                        if value_start == position {
                            return Err(QueryError::Syntax);
                        }
                        input[value_start..position].to_owned()
                    };
                    output.push(Token::Predicate(word.into(), value.into()));
                } else if word == "AND" {
                    output.push(Token::And);
                } else if word == "OR" {
                    output.push(Token::Or);
                } else {
                    output.push(Token::Bare(word.into()));
                }
            }
        }
    }
    if output.is_empty() {
        return Err(QueryError::Syntax);
    }
    Ok(output)
}

fn read_quoted(input: &str, position: &mut usize) -> Result<String, QueryError> {
    let bytes = input.as_bytes();
    if bytes.get(*position) != Some(&b'"') {
        return Err(QueryError::Syntax);
    }
    *position += 1;
    let mut value = String::new();
    let mut closed = false;
    while *position < bytes.len() {
        match bytes[*position] {
            b'"' => {
                *position += 1;
                closed = true;
                break;
            }
            b'\\'
                if *position + 1 < bytes.len() && matches!(bytes[*position + 1], b'\\' | b'"') =>
            {
                value.push(char::from(bytes[*position + 1]));
                *position += 2;
            }
            byte if byte.is_ascii_control() => return Err(QueryError::Syntax),
            _ => {
                let character = input[*position..]
                    .chars()
                    .next()
                    .ok_or(QueryError::Syntax)?;
                value.push(character);
                *position += character.len_utf8();
            }
        }
    }
    if !closed || value.is_empty() {
        return Err(QueryError::Syntax);
    }
    Ok(value)
}

struct Parser<'a> {
    source: QuerySource,
    tokens: &'a [Token],
    position: usize,
    predicates: usize,
    nodes: usize,
    or_alternatives: usize,
}

impl Parser<'_> {
    fn parse_or(&mut self, nesting: usize) -> Result<QueryExpression, QueryError> {
        let mut values = vec![self.parse_and(nesting)?];
        while self.tokens.get(self.position) == Some(&Token::Or) {
            self.position += 1;
            values.push(self.parse_and(nesting)?);
        }
        if values.len() == 1 {
            return Ok(values.pop().expect("one query expression"));
        }
        self.or_alternatives = self.or_alternatives.saturating_add(values.len());
        if self.or_alternatives > MAX_QUERY_OR_ALTERNATIVES {
            return Err(QueryError::LimitExceeded);
        }
        self.node(QueryExpression::Or(values))
    }

    fn parse_and(&mut self, nesting: usize) -> Result<QueryExpression, QueryError> {
        let mut values = Vec::new();
        while self.position < self.tokens.len()
            && !matches!(self.tokens[self.position], Token::Or | Token::Right)
        {
            if self.tokens[self.position] == Token::And {
                if values.is_empty() {
                    return Err(QueryError::Syntax);
                }
                self.position += 1;
                if self.position == self.tokens.len()
                    || matches!(
                        self.tokens[self.position],
                        Token::And | Token::Or | Token::Right
                    )
                {
                    return Err(QueryError::Syntax);
                }
            }
            values.push(self.parse_unary(nesting)?);
        }
        if values.is_empty() {
            return Err(QueryError::Syntax);
        }
        if values.len() == 1 {
            Ok(values.pop().expect("one query expression"))
        } else {
            self.node(QueryExpression::And(values))
        }
    }

    fn parse_unary(&mut self, nesting: usize) -> Result<QueryExpression, QueryError> {
        if self.tokens.get(self.position) == Some(&Token::Bang) {
            self.position += 1;
            let value = self.parse_unary(nesting)?;
            return self.node(QueryExpression::Not(Box::new(value)));
        }
        match self.tokens.get(self.position).cloned() {
            Some(Token::Left) => {
                if nesting >= MAX_QUERY_NESTING {
                    return Err(QueryError::LimitExceeded);
                }
                self.position += 1;
                let expression = self.parse_or(nesting + 1)?;
                if self.tokens.get(self.position) != Some(&Token::Right) {
                    return Err(QueryError::Syntax);
                }
                self.position += 1;
                Ok(expression)
            }
            Some(Token::Predicate(field, value)) => {
                self.position += 1;
                self.predicate(parse_predicate(self.source, &field, &value)?)
            }
            Some(Token::Bare(value)) => {
                self.position += 1;
                self.default_expression(&value)
            }
            _ => Err(QueryError::Syntax),
        }
    }

    fn default_expression(&mut self, value: &str) -> Result<QueryExpression, QueryError> {
        let predicate = |field| QueryPredicate {
            field,
            operator: QueryOperator::Contains,
            value: value.into(),
        };
        match self.source {
            QuerySource::Issues => self.predicate(predicate(QueryField::Title)),
            QuerySource::Logs => self.predicate(predicate(QueryField::Message)),
            QuerySource::Traces => {
                let name = self.predicate(predicate(QueryField::Name))?;
                let operation = self.predicate(predicate(QueryField::Operation))?;
                self.or_alternatives = self.or_alternatives.saturating_add(2);
                if self.or_alternatives > MAX_QUERY_OR_ALTERNATIVES {
                    return Err(QueryError::LimitExceeded);
                }
                self.node(QueryExpression::Or(vec![name, operation]))
            }
            QuerySource::Metrics => self.predicate(predicate(QueryField::MetricName)),
            QuerySource::Replays => self.predicate(predicate(QueryField::Url)),
            QuerySource::Feedback => self.predicate(predicate(QueryField::Message)),
            QuerySource::Releases => self.predicate(QueryPredicate {
                field: QueryField::Release,
                operator: QueryOperator::Equal,
                value: value.into(),
            }),
            QuerySource::Errors => Err(QueryError::CapabilityUnavailable),
        }
    }

    fn predicate(&mut self, value: QueryPredicate) -> Result<QueryExpression, QueryError> {
        self.predicates += 1;
        if self.predicates > MAX_QUERY_PREDICATES {
            return Err(QueryError::LimitExceeded);
        }
        self.node(QueryExpression::Predicate(value))
    }

    fn node(&mut self, value: QueryExpression) -> Result<QueryExpression, QueryError> {
        self.nodes += 1;
        if self.nodes > MAX_QUERY_NODES {
            Err(QueryError::LimitExceeded)
        } else {
            Ok(value)
        }
    }
}

fn parse_predicate(
    source: QuerySource,
    raw_field: &str,
    raw_value: &str,
) -> Result<QueryPredicate, QueryError> {
    let field = resolve_field(raw_field).ok_or(QueryError::CapabilityUnavailable)?;
    if !field_accepted(source, field) {
        return Err(QueryError::CapabilityUnavailable);
    }
    let (operator, value) = if let Some(value) = raw_value.strip_prefix(">=") {
        (QueryOperator::GreaterOrEqual, value)
    } else if let Some(value) = raw_value.strip_prefix("<=") {
        (QueryOperator::LessOrEqual, value)
    } else if let Some(value) = raw_value.strip_prefix('>') {
        (QueryOperator::Greater, value)
    } else if let Some(value) = raw_value.strip_prefix('<') {
        (QueryOperator::Less, value)
    } else if let Some(value) = raw_value.strip_prefix('=') {
        (QueryOperator::Equal, value)
    } else {
        (QueryOperator::Equal, raw_value)
    };
    if value.is_empty() || value.len() > 16 * 1024 || !operator_accepted(field, operator) {
        return Err(QueryError::Syntax);
    }
    validate_value(source, field, operator, value)?;
    Ok(QueryPredicate {
        field,
        operator,
        value: value.into(),
    })
}

fn validate_value(
    source: QuerySource,
    field: QueryField,
    operator: QueryOperator,
    value: &str,
) -> Result<(), QueryError> {
    if operator == QueryOperator::Contains {
        return Ok(());
    }
    let valid = match field {
        QueryField::EventId
        | QueryField::IssueId
        | QueryField::FeedbackId
        | QueryField::ReplayId
        | QueryField::TraceId => valid_hex(value, 32),
        QueryField::SpanId => valid_hex(value, 16),
        QueryField::Timestamp | QueryField::ReceivedAt => {
            value.parse::<i64>().is_ok() || OffsetDateTime::parse(value, &Rfc3339).is_ok()
        }
        QueryField::DurationMs
        | QueryField::MetricSum
        | QueryField::MetricMin
        | QueryField::MetricMax => value.parse::<f64>().is_ok_and(|value| value.is_finite()),
        QueryField::MetricCount => value.parse::<u64>().is_ok(),
        QueryField::IsSegment => matches!(value, "true" | "false"),
        QueryField::Level => match source {
            QuerySource::Errors => {
                matches!(
                    value,
                    "debug" | "info" | "warn" | "warning" | "error" | "fatal"
                )
            }
            QuerySource::Logs => matches!(
                value,
                "trace" | "debug" | "info" | "warn" | "warning" | "error" | "fatal"
            ),
            _ => false,
        },
        QueryField::Status => match source {
            QuerySource::Issues => matches!(value, "open" | "resolved" | "ignored"),
            QuerySource::Feedback => matches!(value, "open" | "resolved" | "spam"),
            QuerySource::Traces => value.len() <= 256,
            _ => false,
        },
        QueryField::MetricKind => matches!(value, "counter" | "gauge" | "distribution"),
        _ => value.len() <= 16 * 1024,
    };
    if valid {
        Ok(())
    } else {
        Err(QueryError::Syntax)
    }
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn resolve_field(value: &str) -> Option<QueryField> {
    Some(match value {
        "event.id" | "event_id" => QueryField::EventId,
        "issue" | "issue_id" => QueryField::IssueId,
        "feedback" | "feedback_id" => QueryField::FeedbackId,
        "replay" | "replay_id" => QueryField::ReplayId,
        "timestamp" => QueryField::Timestamp,
        "received_at" => QueryField::ReceivedAt,
        "title" => QueryField::Title,
        "level" => QueryField::Level,
        "platform" => QueryField::Platform,
        "msg" | "message" => QueryField::Message,
        "env" | "environment" => QueryField::Environment,
        "rel" | "release" | "version" => QueryField::Release,
        "svc" | "service" => QueryField::Service,
        "trace" | "trace_id" => QueryField::TraceId,
        "span" | "span_id" => QueryField::SpanId,
        "dur" | "duration_ms" => QueryField::DurationMs,
        "operation_class" => QueryField::OperationClass,
        "op" | "operation" => QueryField::Operation,
        "status" => QueryField::Status,
        "name" => QueryField::Name,
        "url" => QueryField::Url,
        "user" | "user.id" => QueryField::UserId,
        "is_segment" => QueryField::IsSegment,
        "metric" | "metric_name" => QueryField::MetricName,
        "kind" | "metric_kind" => QueryField::MetricKind,
        "unit" => QueryField::Unit,
        "metric_count" => QueryField::MetricCount,
        "metric_sum" => QueryField::MetricSum,
        "metric_min" => QueryField::MetricMin,
        "metric_max" => QueryField::MetricMax,
        _ => return None,
    })
}

const fn field_accepted(source: QuerySource, field: QueryField) -> bool {
    match source {
        QuerySource::Issues => matches!(
            field,
            QueryField::IssueId | QueryField::Timestamp | QueryField::Title | QueryField::Status
        ),
        QuerySource::Errors => matches!(
            field,
            QueryField::EventId
                | QueryField::IssueId
                | QueryField::Timestamp
                | QueryField::Level
                | QueryField::Platform
                | QueryField::Environment
                | QueryField::Release
                | QueryField::UserId
        ),
        QuerySource::Logs => matches!(
            field,
            QueryField::Timestamp
                | QueryField::ReceivedAt
                | QueryField::Level
                | QueryField::Message
                | QueryField::Environment
                | QueryField::Release
                | QueryField::Service
                | QueryField::TraceId
                | QueryField::SpanId
        ),
        QuerySource::Traces => matches!(
            field,
            QueryField::Timestamp
                | QueryField::ReceivedAt
                | QueryField::DurationMs
                | QueryField::OperationClass
                | QueryField::Environment
                | QueryField::Release
                | QueryField::Service
                | QueryField::TraceId
                | QueryField::SpanId
                | QueryField::Operation
                | QueryField::Status
                | QueryField::Name
                | QueryField::IsSegment
        ),
        QuerySource::Metrics => matches!(
            field,
            QueryField::Timestamp
                | QueryField::ReceivedAt
                | QueryField::MetricName
                | QueryField::MetricKind
                | QueryField::Unit
                | QueryField::TraceId
                | QueryField::MetricCount
                | QueryField::MetricSum
                | QueryField::MetricMin
                | QueryField::MetricMax
        ),
        QuerySource::Replays => matches!(
            field,
            QueryField::ReplayId
                | QueryField::Timestamp
                | QueryField::Url
                | QueryField::Environment
                | QueryField::Release
        ),
        QuerySource::Feedback => matches!(
            field,
            QueryField::FeedbackId
                | QueryField::Timestamp
                | QueryField::Status
                | QueryField::Message
                | QueryField::ReplayId
        ),
        QuerySource::Releases => matches!(field, QueryField::Release | QueryField::Timestamp),
    }
}

const fn operator_accepted(field: QueryField, operator: QueryOperator) -> bool {
    let ordered = matches!(
        field,
        QueryField::Timestamp
            | QueryField::ReceivedAt
            | QueryField::DurationMs
            | QueryField::MetricCount
            | QueryField::MetricSum
            | QueryField::MetricMin
            | QueryField::MetricMax
    );
    match operator {
        QueryOperator::Equal => true,
        QueryOperator::Greater
        | QueryOperator::GreaterOrEqual
        | QueryOperator::Less
        | QueryOperator::LessOrEqual => ordered,
        QueryOperator::Contains => matches!(
            field,
            QueryField::Title
                | QueryField::Message
                | QueryField::Name
                | QueryField::Operation
                | QueryField::Url
                | QueryField::MetricName
        ),
    }
}

fn escaped(value: &str) -> String {
    let mut output = String::new();
    output.push('"');
    for character in value.chars() {
        if matches!(character, '"' | '\\') {
            output.push('\\');
        }
        let _ = output.write_char(character);
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_boolean_comparisons_and_bare_terms_normalize_canonically() {
        let parsed = ParsedQuery::parse(
            QuerySource::Logs,
            r#"level:error AND (svc:api OR svc:worker) !env:development "connection refused""#,
        )
        .unwrap();
        assert!(parsed.normalized.contains("service:eq"));
        assert!(parsed.normalized.contains("environment:eq"));
        assert!(parsed.normalized.contains("message:contains"));
        assert!(!parsed.normalized.contains("svc:"));
    }

    #[test]
    fn source_capabilities_and_structural_limits_fail_explicitly() {
        assert_eq!(
            ParsedQuery::parse(QuerySource::Errors, "message:panic"),
            Err(QueryError::CapabilityUnavailable)
        );
        assert_eq!(
            ParsedQuery::parse(QuerySource::Errors, "panic"),
            Err(QueryError::CapabilityUnavailable)
        );
        let predicates = (0..=MAX_QUERY_PREDICATES)
            .map(|_| "level:error")
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            ParsedQuery::parse(QuerySource::Logs, &predicates),
            Err(QueryError::LimitExceeded)
        );
    }

    #[test]
    fn parser_does_not_expand_boolean_expression_to_dnf() {
        let query = (0..16)
            .map(|index| format!("(svc:a{index} OR svc:b{index})"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(ParsedQuery::parse(QuerySource::Logs, &query).is_ok());
    }

    #[test]
    fn nesting_or_and_byte_ceilings_are_enforced_without_panics() {
        let nested = format!(
            "{}level:error{}",
            "(".repeat(MAX_QUERY_NESTING + 1),
            ")".repeat(MAX_QUERY_NESTING + 1)
        );
        assert_eq!(
            ParsedQuery::parse(QuerySource::Logs, &nested),
            Err(QueryError::LimitExceeded)
        );
        let alternatives = (0..=MAX_QUERY_OR_ALTERNATIVES)
            .map(|index| format!("svc:service-{index}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        assert_eq!(
            ParsedQuery::parse(QuerySource::Logs, &alternatives),
            Err(QueryError::LimitExceeded)
        );
        assert_eq!(
            ParsedQuery::parse(QuerySource::Logs, &"x".repeat(MAX_QUERY_BYTES + 1)),
            Err(QueryError::LimitExceeded)
        );
    }

    #[test]
    fn source_fields_and_typed_values_are_validated_before_planning() {
        assert!(ParsedQuery::parse(QuerySource::Traces, "dur:>=500 op:http.server").is_ok());
        assert!(ParsedQuery::parse(QuerySource::Metrics, "metric:requests kind:counter").is_ok());
        assert_eq!(
            ParsedQuery::parse(QuerySource::Replays, "level:error"),
            Err(QueryError::CapabilityUnavailable)
        );
        assert_eq!(
            ParsedQuery::parse(QuerySource::Issues, "status:unknown"),
            Err(QueryError::Syntax)
        );
    }
}
