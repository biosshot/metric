//! Deterministic validation, cost estimation and concurrency isolation for Explore.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use metric_domain::{
    ProjectId, Timestamp,
    explore::{
        ExploreAggregateKind, ExploreField, ExplorePlan, ExplorePredicateOp, ExploreQuery,
        ExploreResult, ExploreValue, MAX_EXPLORE_AGGREGATES, MAX_EXPLORE_GROUPS,
        MAX_EXPLORE_PREDICATES, MAX_EXPLORE_ROWS, normalize_query,
    },
};
use metric_ports::{ExploreStore, ExploreStoreError};
use thiserror::Error;
use tokio::sync::Semaphore;

const MAX_RANGE_MILLIS: i64 = 30 * 86_400_000;
const MAX_INTERVALS: u32 = 1_000;

#[derive(Debug, Clone, Copy)]
pub struct ExploreConfig {
    pub maximum_cost: u32,
    pub maximum_concurrency: usize,
    pub query_timeout: Duration,
}

impl Default for ExploreConfig {
    fn default() -> Self {
        Self {
            maximum_cost: 10_000,
            maximum_concurrency: 4,
            query_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ExploreError {
    #[error("Explore query is invalid")]
    InvalidQuery,
    #[error("Explore query exceeds its deterministic cost budget")]
    CostExceeded,
    #[error("Explore query concurrency is exhausted")]
    Capacity,
    #[error("Explore storage is temporarily unavailable")]
    Unavailable,
}

impl ExploreError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidQuery => "explore_invalid_query",
            Self::CostExceeded => "explore_cost_exceeded",
            Self::Capacity => "explore_capacity",
            Self::Unavailable => "explore_unavailable",
        }
    }
}

pub struct ExploreService {
    store: Arc<dyn ExploreStore>,
    config: ExploreConfig,
    concurrency: Arc<Semaphore>,
}

impl ExploreService {
    pub fn new(store: Arc<dyn ExploreStore>, config: ExploreConfig) -> Result<Self, ExploreError> {
        if config.maximum_cost == 0
            || config.maximum_concurrency == 0
            || !(Duration::from_millis(100)..=Duration::from_secs(30))
                .contains(&config.query_timeout)
        {
            return Err(ExploreError::InvalidQuery);
        }
        Ok(Self {
            store,
            config,
            concurrency: Arc::new(Semaphore::new(config.maximum_concurrency)),
        })
    }

    /// Injects trusted project scope before validation or storage planning.
    pub fn plan(
        &self,
        project_id: ProjectId,
        query: ExploreQuery,
    ) -> Result<ExplorePlan, ExploreError> {
        let range = query
            .until
            .unix_millis()
            .saturating_sub(query.from.unix_millis());
        if range <= 0
            || range > MAX_RANGE_MILLIS
            || query.predicates.len() > MAX_EXPLORE_PREDICATES
            || query.aggregates.len() > MAX_EXPLORE_AGGREGATES
            || query.group_by.len() > MAX_EXPLORE_GROUPS
            || query.limit == 0
            || query.limit > MAX_EXPLORE_ROWS
            || (query.aggregates.is_empty()
                && (!query.group_by.is_empty() || query.interval.is_some()))
            || (!query.aggregates.is_empty() && query.cursor.is_some())
        {
            return Err(ExploreError::InvalidQuery);
        }
        if query.interval.is_some() && query.aggregates.is_empty() {
            return Err(ExploreError::InvalidQuery);
        }
        for predicate in &query.predicates {
            if !predicate.field.accepted_by(query.dataset)
                || !valid_predicate(
                    query.dataset,
                    predicate.op,
                    predicate.field,
                    predicate.value.as_ref(),
                    predicate.upper.as_ref(),
                )
            {
                return Err(ExploreError::InvalidQuery);
            }
        }
        for field in &query.group_by {
            if !field.groupable(query.dataset) {
                return Err(ExploreError::InvalidQuery);
            }
        }
        let mut aliases = BTreeSet::new();
        for aggregate in &query.aggregates {
            let valid = match aggregate.kind {
                ExploreAggregateKind::Count => aggregate.field.is_none(),
                _ => aggregate
                    .field
                    .is_some_and(|field| field.accepted_by(query.dataset) && field.numeric()),
            };
            if !valid
                || aggregate.alias.is_empty()
                || aggregate.alias.len() > 32
                || !aggregate
                    .alias
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                || !aliases.insert(aggregate.alias.as_ref())
            {
                return Err(ExploreError::InvalidQuery);
            }
        }
        let interval_count = query.interval.map_or(1, |interval| {
            u32::try_from(range.saturating_add(interval.millis() - 1) / interval.millis())
                .unwrap_or(u32::MAX)
        });
        if interval_count > MAX_INTERVALS {
            return Err(ExploreError::CostExceeded);
        }
        let range_hours =
            u32::try_from(range.saturating_add(3_599_999) / 3_600_000).unwrap_or(u32::MAX);
        let predicate_cost = u32::try_from(query.predicates.len()).unwrap_or(u32::MAX) * 40;
        let group_fanout = query.group_by.iter().fold(1_u32, |fanout, field| {
            fanout.saturating_mul(
                field
                    .maximum_group_cardinality(query.dataset)
                    .unwrap_or(u32::MAX),
            )
        });
        let aggregate_fanout =
            1_u32.saturating_add(u32::try_from(query.aggregates.len()).unwrap_or(u32::MAX) * 2);
        let row_cost = if query.aggregates.is_empty() {
            u32::try_from(query.limit).unwrap_or(u32::MAX)
        } else {
            interval_count
        };
        let cost = 100_u32
            .saturating_add(range_hours.saturating_mul(2))
            .saturating_add(predicate_cost)
            .saturating_add(
                row_cost
                    .saturating_mul(group_fanout)
                    .saturating_mul(aggregate_fanout),
            );
        if cost > self.config.maximum_cost {
            return Err(ExploreError::CostExceeded);
        }
        let normalized = normalize_query(&query);
        Ok(ExplorePlan {
            project_id,
            query,
            normalized,
            cost,
            maximum_time: self.config.query_timeout,
        })
    }

    pub async fn execute(&self, plan: ExplorePlan) -> Result<ExploreResult, ExploreError> {
        let _permit = Arc::clone(&self.concurrency)
            .try_acquire_owned()
            .map_err(|_| ExploreError::Capacity)?;
        self.store.execute(plan).await.map_err(|error| match error {
            ExploreStoreError::InvalidData => ExploreError::InvalidQuery,
            ExploreStoreError::Unavailable => ExploreError::Unavailable,
        })
    }
}

fn valid_predicate(
    dataset: metric_domain::explore::ExploreDataset,
    operation: ExplorePredicateOp,
    field: ExploreField,
    value: Option<&ExploreValue>,
    upper: Option<&ExploreValue>,
) -> bool {
    match operation {
        ExplorePredicateOp::Present => {
            matches!(value, Some(ExploreValue::Bool(_))) && upper.is_none()
        }
        ExplorePredicateOp::Exact => {
            value.is_some_and(|value| value_matches(dataset, field, value)) && upper.is_none()
        }
        ExplorePredicateOp::Range => {
            field.numeric()
                && value.is_some_and(|value| value_matches(dataset, field, value))
                && upper.is_some_and(|value| value_matches(dataset, field, value))
                && value
                    .zip(upper)
                    .is_some_and(|(lower, upper)| value_less_than(lower, upper))
        }
    }
}

fn value_matches(
    dataset: metric_domain::explore::ExploreDataset,
    field: ExploreField,
    value: &ExploreValue,
) -> bool {
    match field {
        ExploreField::Timestamp | ExploreField::ReceivedAt => {
            matches!(value, ExploreValue::Integer(value) if Timestamp::from_unix_millis(*value).is_ok())
        }
        ExploreField::DurationMs => {
            matches!(value, ExploreValue::Integer(value) if *value >= 0)
                || matches!(value, ExploreValue::Number(value) if value.is_finite() && *value >= 0.0)
        }
        ExploreField::IsSegment => matches!(value, ExploreValue::Bool(_)),
        ExploreField::TraceId => {
            matches!(value, ExploreValue::String(value) if valid_hex(value, 32))
        }
        ExploreField::SpanId => {
            matches!(value, ExploreValue::String(value) if valid_hex(value, 16))
        }
        ExploreField::IssueId => {
            matches!(value, ExploreValue::String(value) if valid_hex(value, 32))
        }
        ExploreField::Level => {
            matches!(
                (dataset, value),
                (
                    metric_domain::explore::ExploreDataset::Errors,
                    ExploreValue::String(value)
                ) if matches!(value.as_ref(), "debug" | "info" | "warn" | "warning" | "error" | "fatal")
            ) || matches!(
                (dataset, value),
                (
                    metric_domain::explore::ExploreDataset::Logs,
                    ExploreValue::String(value)
                ) if matches!(value.as_ref(), "trace" | "debug" | "info" | "warn" | "warning" | "error" | "fatal")
            )
        }
        ExploreField::Platform => matches!(
            value,
            ExploreValue::String(value)
                if matches!(
                    value.as_ref(),
                    "other" | "python" | "javascript" | "node" | "native" | "cocoa"
                        | "java" | "php" | "ruby" | "dotnet" | "go" | "rust"
                )
        ),
        ExploreField::OperationClass => matches!(
            value,
            ExploreValue::String(value)
                if matches!(
                    value.as_ref(),
                    "other" | "http.server" | "http.client" | "database" | "cache"
                        | "queue" | "file" | "rpc" | "function" | "task" | "ui" | "resource"
                )
        ),
        _ => matches!(value, ExploreValue::String(value) if value.len() <= 256),
    }
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn value_less_than(lower: &ExploreValue, upper: &ExploreValue) -> bool {
    match (lower, upper) {
        (ExploreValue::Integer(lower), ExploreValue::Integer(upper)) => lower < upper,
        (ExploreValue::Number(lower), ExploreValue::Number(upper)) => lower < upper,
        (ExploreValue::Integer(lower), ExploreValue::Number(upper)) => (*lower as f64) < *upper,
        (ExploreValue::Number(lower), ExploreValue::Integer(upper)) => *lower < (*upper as f64),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metric_domain::{Timestamp, explore::*};
    use metric_ports::PortFuture;

    struct Noop;
    impl ExploreStore for Noop {
        fn execute(
            &self,
            _: ExplorePlan,
        ) -> PortFuture<'_, Result<ExploreResult, ExploreStoreError>> {
            Box::pin(async {
                Ok(ExploreResult {
                    rows: Vec::new(),
                    next: None,
                })
            })
        }
    }

    fn service() -> ExploreService {
        ExploreService::new(Arc::new(Noop), ExploreConfig::default()).unwrap()
    }

    #[test]
    fn project_scope_is_injected_and_invalid_fields_fail_before_storage() {
        let mut query = ExploreQuery {
            dataset: ExploreDataset::Logs,
            from: Timestamp::from_unix_millis(0).unwrap(),
            until: Timestamp::from_unix_millis(3_600_000).unwrap(),
            predicates: Vec::new(),
            aggregates: Vec::new(),
            group_by: Vec::new(),
            interval: None,
            cursor: None,
            limit: 50,
        };
        let plan = service()
            .plan(ProjectId::new(7).unwrap(), query.clone())
            .unwrap();
        assert_eq!(plan.project_id.get(), 7);
        query.predicates.push(ExplorePredicate {
            field: ExploreField::DurationMs,
            op: ExplorePredicateOp::Range,
            value: Some(ExploreValue::Number(1.0)),
            upper: Some(ExploreValue::Number(2.0)),
        });
        assert_eq!(
            service().plan(ProjectId::new(7).unwrap(), query),
            Err(ExploreError::InvalidQuery)
        );
    }

    #[test]
    fn adversarial_interval_cardinality_is_rejected_deterministically() {
        let query = ExploreQuery {
            dataset: ExploreDataset::Spans,
            from: Timestamp::from_unix_millis(0).unwrap(),
            until: Timestamp::from_unix_millis(30 * 86_400_000).unwrap(),
            predicates: Vec::new(),
            aggregates: vec![ExploreAggregate {
                kind: ExploreAggregateKind::Count,
                field: None,
                alias: "count".into(),
            }],
            group_by: vec![ExploreField::OperationClass, ExploreField::IsSegment],
            interval: Some(ExploreInterval::Minute),
            cursor: None,
            limit: 50,
        };
        assert_eq!(
            service().plan(ProjectId::new(1).unwrap(), query),
            Err(ExploreError::CostExceeded)
        );
    }
}
