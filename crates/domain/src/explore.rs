//! Bounded, storage-independent query contract for Unified Explore.

use std::{collections::BTreeMap, fmt::Write, time::Duration};

use crate::{ProjectId, Timestamp};

pub const MAX_EXPLORE_PREDICATES: usize = 8;
pub const MAX_EXPLORE_AGGREGATES: usize = 4;
pub const MAX_EXPLORE_GROUPS: usize = 2;
pub const MAX_EXPLORE_ROWS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExploreDataset {
    Errors,
    Logs,
    Spans,
    Metrics,
}

impl ExploreDataset {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Errors => "errors",
            Self::Logs => "logs",
            Self::Spans => "spans",
            Self::Metrics => "metrics",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExploreField {
    Timestamp,
    ReceivedAt,
    Level,
    Platform,
    IssueId,
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
    IsSegment,
    MetricKind,
    Unit,
    MetricCount,
    MetricSum,
    MetricMin,
    MetricMax,
}

impl ExploreField {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timestamp => "timestamp",
            Self::ReceivedAt => "received_at",
            Self::Level => "level",
            Self::Platform => "platform",
            Self::IssueId => "issue_id",
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
            Self::IsSegment => "is_segment",
            Self::MetricKind => "metric_kind",
            Self::Unit => "unit",
            Self::MetricCount => "metric_count",
            Self::MetricSum => "metric_sum",
            Self::MetricMin => "metric_min",
            Self::MetricMax => "metric_max",
        }
    }

    #[must_use]
    pub const fn accepted_by(self, dataset: ExploreDataset) -> bool {
        match dataset {
            ExploreDataset::Errors => matches!(
                self,
                Self::Timestamp | Self::ReceivedAt | Self::Level | Self::Platform | Self::IssueId
            ),
            ExploreDataset::Logs => matches!(
                self,
                Self::Timestamp
                    | Self::ReceivedAt
                    | Self::Level
                    | Self::Message
                    | Self::Environment
                    | Self::Release
                    | Self::Service
                    | Self::TraceId
                    | Self::SpanId
            ),
            ExploreDataset::Spans => matches!(
                self,
                Self::Timestamp
                    | Self::ReceivedAt
                    | Self::DurationMs
                    | Self::OperationClass
                    | Self::Environment
                    | Self::Release
                    | Self::Service
                    | Self::TraceId
                    | Self::SpanId
                    | Self::Operation
                    | Self::Status
                    | Self::Name
                    | Self::IsSegment
            ),
            ExploreDataset::Metrics => matches!(
                self,
                Self::Timestamp
                    | Self::ReceivedAt
                    | Self::Name
                    | Self::MetricKind
                    | Self::Unit
                    | Self::TraceId
                    | Self::MetricCount
                    | Self::MetricSum
                    | Self::MetricMin
                    | Self::MetricMax
            ),
        }
    }

    #[must_use]
    pub const fn numeric(self) -> bool {
        matches!(
            self,
            Self::Timestamp
                | Self::ReceivedAt
                | Self::DurationMs
                | Self::MetricCount
                | Self::MetricSum
                | Self::MetricMin
                | Self::MetricMax
        )
    }

    #[must_use]
    pub const fn groupable(self, dataset: ExploreDataset) -> bool {
        matches!(
            (dataset, self),
            (ExploreDataset::Errors, Self::Level | Self::Platform)
                | (ExploreDataset::Logs, Self::Level)
                | (
                    ExploreDataset::Spans,
                    Self::OperationClass | Self::IsSegment
                )
                | (ExploreDataset::Metrics, Self::MetricKind | Self::Unit)
        )
    }

    #[must_use]
    pub const fn maximum_group_cardinality(self, dataset: ExploreDataset) -> Option<u32> {
        match (dataset, self) {
            (ExploreDataset::Errors, Self::Level) => Some(5),
            (ExploreDataset::Errors, Self::Platform) => Some(10),
            (ExploreDataset::Logs, Self::Level) => Some(6),
            (ExploreDataset::Spans, Self::OperationClass) => Some(12),
            (ExploreDataset::Spans, Self::IsSegment) => Some(2),
            (ExploreDataset::Metrics, Self::MetricKind) => Some(3),
            (ExploreDataset::Metrics, Self::Unit) => Some(64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExploreValue {
    String(Box<str>),
    Number(f64),
    Integer(i64),
    Bool(bool),
    Null,
}

impl ExploreValue {
    fn normalize_into(&self, output: &mut String) {
        match self {
            Self::String(value) => {
                output.push('"');
                for character in value.chars() {
                    if matches!(character, '"' | '\\') {
                        output.push('\\');
                    }
                    output.push(character);
                }
                output.push('"');
            }
            Self::Number(value) => {
                let _ = write!(output, "{value}");
            }
            Self::Integer(value) => {
                let _ = write!(output, "{value}");
            }
            Self::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
            Self::Null => output.push_str("null"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplorePredicateOp {
    Exact,
    Contains,
    StartsWith,
    EndsWith,
    Present,
    Range,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExplorePredicate {
    pub field: ExploreField,
    pub op: ExplorePredicateOp,
    pub value: Option<ExploreValue>,
    pub upper: Option<ExploreValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExploreAggregateKind {
    Count,
    Sum,
    Min,
    Max,
    Avg,
    P50,
    P75,
    P90,
    P95,
    P99,
}

impl ExploreAggregateKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Sum => "sum",
            Self::Min => "min",
            Self::Max => "max",
            Self::Avg => "avg",
            Self::P50 => "p50",
            Self::P75 => "p75",
            Self::P90 => "p90",
            Self::P95 => "p95",
            Self::P99 => "p99",
        }
    }

    #[must_use]
    pub const fn percentile(self) -> Option<f64> {
        match self {
            Self::P50 => Some(0.50),
            Self::P75 => Some(0.75),
            Self::P90 => Some(0.90),
            Self::P95 => Some(0.95),
            Self::P99 => Some(0.99),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExploreAggregate {
    pub kind: ExploreAggregateKind,
    pub field: Option<ExploreField>,
    pub alias: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExploreInterval {
    Minute,
    FiveMinutes,
    Hour,
    Day,
}

impl ExploreInterval {
    #[must_use]
    pub const fn millis(self) -> i64 {
        match self {
            Self::Minute => 60_000,
            Self::FiveMinutes => 300_000,
            Self::Hour => 3_600_000,
            Self::Day => 86_400_000,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minute => "1m",
            Self::FiveMinutes => "5m",
            Self::Hour => "1h",
            Self::Day => "1d",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExploreCursor {
    pub time: i64,
    pub id: [u8; 20],
    pub id_len: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExploreQuery {
    pub dataset: ExploreDataset,
    pub from: Timestamp,
    pub until: Timestamp,
    pub predicates: Vec<ExplorePredicate>,
    pub aggregates: Vec<ExploreAggregate>,
    pub group_by: Vec<ExploreField>,
    pub interval: Option<ExploreInterval>,
    pub cursor: Option<ExploreCursor>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExplorePlan {
    pub project_id: ProjectId,
    pub query: ExploreQuery,
    pub normalized: Box<str>,
    pub cost: u32,
    pub maximum_time: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExploreRow {
    pub values: BTreeMap<Box<str>, ExploreValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExploreResult {
    pub rows: Vec<ExploreRow>,
    pub next: Option<ExploreCursor>,
}

#[must_use]
pub fn normalize_query(query: &ExploreQuery) -> Box<str> {
    let mut output = format!(
        "v1|{}|{}|{}|limit:{}",
        query.dataset.as_str(),
        query.from.unix_millis(),
        query.until.unix_millis(),
        query.limit
    );
    let _ = write!(
        output,
        "|interval:{}",
        query.interval.map_or("-", ExploreInterval::as_str)
    );
    output.push_str("|where:");
    for predicate in &query.predicates {
        let _ = write!(
            output,
            "{}:{}:",
            predicate.field.as_str(),
            match predicate.op {
                ExplorePredicateOp::Exact => "eq",
                ExplorePredicateOp::Contains => "contains",
                ExplorePredicateOp::StartsWith => "starts_with",
                ExplorePredicateOp::EndsWith => "ends_with",
                ExplorePredicateOp::Present => "present",
                ExplorePredicateOp::Range => "range",
            }
        );
        if let Some(value) = &predicate.value {
            value.normalize_into(&mut output);
        }
        output.push(':');
        if let Some(value) = &predicate.upper {
            value.normalize_into(&mut output);
        }
        output.push(',');
    }
    output.push_str("|aggregate:");
    for aggregate in &query.aggregates {
        let _ = write!(
            output,
            "{}:{}:{},",
            aggregate.kind.as_str(),
            aggregate.field.map_or("-", ExploreField::as_str),
            aggregate.alias
        );
    }
    output.push_str("|group:");
    for field in &query.group_by {
        let _ = write!(output, "{},", field.as_str());
    }
    output.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_is_stable_and_excludes_project_scope() {
        let query = ExploreQuery {
            dataset: ExploreDataset::Logs,
            from: Timestamp::from_unix_millis(1_000).unwrap(),
            until: Timestamp::from_unix_millis(2_000).unwrap(),
            predicates: vec![ExplorePredicate {
                field: ExploreField::Service,
                op: ExplorePredicateOp::Exact,
                value: Some(ExploreValue::String("api".into())),
                upper: None,
            }],
            aggregates: vec![ExploreAggregate {
                kind: ExploreAggregateKind::Count,
                field: None,
                alias: "events".into(),
            }],
            group_by: vec![ExploreField::Level],
            interval: None,
            cursor: None,
            limit: 50,
        };
        assert_eq!(
            normalize_query(&query).as_ref(),
            "v1|logs|1000|2000|limit:50|interval:-|where:service:eq:\"api\":,|aggregate:count:-:events,|group:level,"
        );
    }
}
