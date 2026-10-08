//! Adapter-private, versioned query representation. Flat AST nodes avoid JSON
//! parser recursion limits for the domain's supported 256-node expressions.
use super::{DashboardStoreError, invalid};
use metric_domain::{Timestamp, explore::*};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, DashboardStoreError>;
const FIELDS: [ExploreField; 23] = [
    ExploreField::Timestamp,
    ExploreField::ReceivedAt,
    ExploreField::Level,
    ExploreField::Platform,
    ExploreField::IssueId,
    ExploreField::Message,
    ExploreField::Environment,
    ExploreField::Release,
    ExploreField::Service,
    ExploreField::TraceId,
    ExploreField::SpanId,
    ExploreField::DurationMs,
    ExploreField::OperationClass,
    ExploreField::Operation,
    ExploreField::Status,
    ExploreField::Name,
    ExploreField::IsSegment,
    ExploreField::MetricKind,
    ExploreField::Unit,
    ExploreField::MetricCount,
    ExploreField::MetricSum,
    ExploreField::MetricMin,
    ExploreField::MetricMax,
];
const OPS: [ExplorePredicateOp; 10] = [
    ExplorePredicateOp::Exact,
    ExplorePredicateOp::Contains,
    ExplorePredicateOp::StartsWith,
    ExplorePredicateOp::EndsWith,
    ExplorePredicateOp::Present,
    ExplorePredicateOp::Range,
    ExplorePredicateOp::Greater,
    ExplorePredicateOp::GreaterOrEqual,
    ExplorePredicateOp::Less,
    ExplorePredicateOp::LessOrEqual,
];
const AGGREGATES: [ExploreAggregateKind; 10] = [
    ExploreAggregateKind::Count,
    ExploreAggregateKind::Sum,
    ExploreAggregateKind::Min,
    ExploreAggregateKind::Max,
    ExploreAggregateKind::Avg,
    ExploreAggregateKind::P50,
    ExploreAggregateKind::P75,
    ExploreAggregateKind::P90,
    ExploreAggregateKind::P95,
    ExploreAggregateKind::P99,
];
const DATASETS: [ExploreDataset; 4] = [
    ExploreDataset::Errors,
    ExploreDataset::Logs,
    ExploreDataset::Spans,
    ExploreDataset::Metrics,
];
const INTERVALS: [ExploreInterval; 4] = [
    ExploreInterval::Minute,
    ExploreInterval::FiveMinutes,
    ExploreInterval::Hour,
    ExploreInterval::Day,
];

pub(super) fn encode(query: &ExploreQuery) -> Result<Value> {
    bounds(query)?;
    let predicates = query
        .predicates
        .iter()
        .map(encode_predicate)
        .collect::<Result<Vec<_>>>()?;
    let expression = query
        .expression
        .as_ref()
        .map(encode_expression)
        .transpose()?;
    Ok(
        json!({"v":1,"dataset":query.dataset.as_str(),"from":query.from.unix_millis(),"until":query.until.unix_millis(),"predicates":predicates,"expression":expression,"aggregates":query.aggregates.iter().map(|value|json!({"kind":value.kind.as_str(),"field":value.field.map(ExploreField::as_str),"alias":value.alias})).collect::<Vec<_>>(),"group_by":query.group_by.iter().map(|value|value.as_str()).collect::<Vec<_>>(),"interval":query.interval.map(ExploreInterval::as_str),"limit":query.limit}),
    )
}

pub(super) fn decode(value: &Value) -> Result<ExploreQuery> {
    if value["v"].as_u64() != Some(1) {
        return Err(DashboardStoreError::InvalidData);
    }
    let query = ExploreQuery {
        dataset: DATASETS
            .into_iter()
            .find(|entry| Some(entry.as_str()) == value["dataset"].as_str())
            .ok_or(DashboardStoreError::InvalidData)?,
        from: Timestamp::from_unix_millis(
            value["from"]
                .as_i64()
                .ok_or(DashboardStoreError::InvalidData)?,
        )
        .map_err(invalid)?,
        until: Timestamp::from_unix_millis(
            value["until"]
                .as_i64()
                .ok_or(DashboardStoreError::InvalidData)?,
        )
        .map_err(invalid)?,
        predicates: array(&value["predicates"], MAX_EXPLORE_PREDICATES)?
            .iter()
            .map(decode_predicate)
            .collect::<Result<_>>()?,
        expression: if value["expression"].is_null() {
            None
        } else {
            Some(decode_expression(&value["expression"])?)
        },
        aggregates: array(&value["aggregates"], MAX_EXPLORE_AGGREGATES)?
            .iter()
            .map(|entry| {
                Ok(ExploreAggregate {
                    kind: AGGREGATES
                        .into_iter()
                        .find(|kind| Some(kind.as_str()) == entry["kind"].as_str())
                        .ok_or(DashboardStoreError::InvalidData)?,
                    field: if entry["field"].is_null() {
                        None
                    } else {
                        Some(field(&entry["field"])?)
                    },
                    alias: text(&entry["alias"])?.into(),
                })
            })
            .collect::<Result<_>>()?,
        group_by: array(&value["group_by"], MAX_EXPLORE_GROUPS)?
            .iter()
            .map(field)
            .collect::<Result<_>>()?,
        interval: if value["interval"].is_null() {
            None
        } else {
            Some(
                INTERVALS
                    .into_iter()
                    .find(|entry| Some(entry.as_str()) == value["interval"].as_str())
                    .ok_or(DashboardStoreError::InvalidData)?,
            )
        },
        cursor: None,
        limit: index(&value["limit"])?,
    };
    bounds(&query)?;
    Ok(query)
}

fn bounds(query: &ExploreQuery) -> Result<()> {
    if query.cursor.is_some()
        || query.from >= query.until
        || query.predicates.len() > MAX_EXPLORE_PREDICATES
        || query.aggregates.len() > MAX_EXPLORE_AGGREGATES
        || query.group_by.len() > MAX_EXPLORE_GROUPS
        || !(1..=MAX_EXPLORE_ROWS).contains(&query.limit)
        || query
            .aggregates
            .iter()
            .any(|value| value.alias.is_empty() || value.alias.len() > 32)
    {
        return Err(DashboardStoreError::InvalidData);
    }
    Ok(())
}
fn encode_expression(root: &ExploreExpression) -> Result<Value> {
    let mut nodes = vec![Value::Null];
    let mut pending = vec![(0, root)];
    let mut predicates = 0;
    let mut alternatives = 0;
    while let Some((index, node)) = pending.pop() {
        match node {
            ExploreExpression::Predicate(value) => {
                predicates += 1;
                if predicates > MAX_EXPLORE_PREDICATES {
                    return Err(DashboardStoreError::InvalidData);
                }
                nodes[index] = json!(["predicate", encode_predicate(value)?]);
            }
            ExploreExpression::Not(value) => {
                let next = nodes.len();
                nodes.push(Value::Null);
                pending.push((next, value));
                nodes[index] = json!(["not", [next]]);
            }
            ExploreExpression::And(values) | ExploreExpression::Or(values) => {
                let is_or = matches!(node, ExploreExpression::Or(_));
                if is_or {
                    alternatives += values.len();
                    if alternatives > MAX_EXPLORE_OR_ALTERNATIVES {
                        return Err(DashboardStoreError::InvalidData);
                    }
                }
                if nodes.len().saturating_add(values.len()) > MAX_EXPLORE_EXPRESSION_NODES {
                    return Err(DashboardStoreError::InvalidData);
                }
                let children: Vec<_> = values
                    .iter()
                    .map(|value| {
                        let next = nodes.len();
                        nodes.push(Value::Null);
                        pending.push((next, value));
                        next
                    })
                    .collect();
                nodes[index] = json!([if is_or { "or" } else { "and" }, children]);
            }
        }
        if nodes.len() > MAX_EXPLORE_EXPRESSION_NODES {
            return Err(DashboardStoreError::InvalidData);
        }
    }
    Ok(json!(nodes))
}
fn decode_expression(value: &Value) -> Result<ExploreExpression> {
    let nodes = array(value, MAX_EXPLORE_EXPRESSION_NODES)?;
    if nodes.is_empty() {
        return Err(DashboardStoreError::InvalidData);
    }
    let mut decoded: Vec<Option<ExploreExpression>> = vec![None; nodes.len()];
    let mut predicates = 0;
    let mut alternatives = 0;
    for current in (0..nodes.len()).rev() {
        let node = array(&nodes[current], 2)?;
        if node.len() != 2 {
            return Err(DashboardStoreError::InvalidData);
        }
        let kind = text(&node[0])?;
        let expression = if kind == "predicate" {
            predicates += 1;
            ExploreExpression::Predicate(decode_predicate(&node[1])?)
        } else {
            let ids = array(&node[1], MAX_EXPLORE_EXPRESSION_NODES)?;
            let mut children = Vec::with_capacity(ids.len());
            for id in ids {
                let id = index(id)?;
                if id <= current {
                    return Err(DashboardStoreError::InvalidData);
                }
                children.push(
                    decoded
                        .get_mut(id)
                        .and_then(Option::take)
                        .ok_or(DashboardStoreError::InvalidData)?,
                );
            }
            match kind {
                "and" => ExploreExpression::And(children),
                "or" => {
                    alternatives += children.len();
                    ExploreExpression::Or(children)
                }
                "not" if children.len() == 1 => ExploreExpression::Not(Box::new(
                    children.pop().ok_or(DashboardStoreError::InvalidData)?,
                )),
                _ => return Err(DashboardStoreError::InvalidData),
            }
        };
        decoded[current] = Some(expression);
    }
    let root = decoded[0].take().ok_or(DashboardStoreError::InvalidData)?;
    if decoded.iter().any(Option::is_some)
        || predicates > MAX_EXPLORE_PREDICATES
        || alternatives > MAX_EXPLORE_OR_ALTERNATIVES
    {
        return Err(DashboardStoreError::InvalidData);
    }
    Ok(root)
}
fn encode_predicate(value: &ExplorePredicate) -> Result<Value> {
    let op = OPS
        .iter()
        .position(|op| *op == value.op)
        .ok_or(DashboardStoreError::InvalidData)?;
    Ok(json!([
        value.field.as_str(),
        op,
        value.value.as_ref().map(encode_value).transpose()?,
        value.upper.as_ref().map(encode_value).transpose()?
    ]))
}
fn decode_predicate(value: &Value) -> Result<ExplorePredicate> {
    let value = array(value, 4)?;
    if value.len() != 4 {
        return Err(DashboardStoreError::InvalidData);
    }
    Ok(ExplorePredicate {
        field: field(&value[0])?,
        op: *OPS
            .get(index(&value[1])?)
            .ok_or(DashboardStoreError::InvalidData)?,
        value: if value[2].is_null() {
            None
        } else {
            Some(decode_value(&value[2])?)
        },
        upper: if value[3].is_null() {
            None
        } else {
            Some(decode_value(&value[3])?)
        },
    })
}
fn encode_value(value: &ExploreValue) -> Result<Value> {
    Ok(match value {
        ExploreValue::String(value) if value.len() <= 256 => json!(["s", value]),
        ExploreValue::Integer(value) => json!(["i", value]),
        ExploreValue::Number(value) if value.is_finite() => json!(["f", value]),
        ExploreValue::Bool(value) => json!(["b", value]),
        ExploreValue::Null => json!(["n", null]),
        _ => return Err(DashboardStoreError::InvalidData),
    })
}
fn decode_value(value: &Value) -> Result<ExploreValue> {
    let value = array(value, 2)?;
    if value.len() != 2 {
        return Err(DashboardStoreError::InvalidData);
    }
    match text(&value[0])? {
        "s" if text(&value[1])?.len() <= 256 => Ok(ExploreValue::String(text(&value[1])?.into())),
        "i" => Ok(ExploreValue::Integer(
            value[1].as_i64().ok_or(DashboardStoreError::InvalidData)?,
        )),
        "f" => Ok(ExploreValue::Number(
            value[1]
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or(DashboardStoreError::InvalidData)?,
        )),
        "b" => Ok(ExploreValue::Bool(
            value[1].as_bool().ok_or(DashboardStoreError::InvalidData)?,
        )),
        "n" if value[1].is_null() => Ok(ExploreValue::Null),
        _ => Err(DashboardStoreError::InvalidData),
    }
}
fn field(value: &Value) -> Result<ExploreField> {
    FIELDS
        .into_iter()
        .find(|field| Some(field.as_str()) == value.as_str())
        .ok_or(DashboardStoreError::InvalidData)
}
pub(super) fn array(value: &Value, maximum: usize) -> Result<&Vec<Value>> {
    value
        .as_array()
        .filter(|values| values.len() <= maximum)
        .ok_or(DashboardStoreError::InvalidData)
}
pub(super) fn text(value: &Value) -> Result<&str> {
    value.as_str().ok_or(DashboardStoreError::InvalidData)
}
fn index(value: &Value) -> Result<usize> {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(DashboardStoreError::InvalidData)
}
