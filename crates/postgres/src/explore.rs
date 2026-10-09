//! Closed typed SQL compiler: caller-controlled values never become SQL syntax.
use crate::PostgresDatabase;
use metric_domain::{Timestamp, explore::*};
use metric_ports::{ExploreStore, ExploreStoreError, PortFuture};
use sqlx::{Postgres, QueryBuilder, Row, postgres::PgRow};
use std::{collections::BTreeMap, fmt::Write, time::Duration};

type Result<T> = std::result::Result<T, ExploreStoreError>;
const RESPONSE_BYTES: i64 = 8 * 1024 * 1024;
const FIELDS: &[ExploreField] = &[
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

#[derive(Clone)]
pub struct PostgresExploreStore {
    database: PostgresDatabase,
}

impl PostgresExploreStore {
    #[must_use]
    pub fn new(database: PostgresDatabase) -> Self {
        Self { database }
    }

    async fn run(&self, plan: ExplorePlan) -> Result<ExploreResult> {
        validate(&plan)?;
        let compiled = compile(&plan)?;
        // A dropped/failed future rolls back both READ ONLY and local settings.
        tokio::time::timeout(plan.maximum_time, async {
            let mut transaction = self.database.pool.begin().await.map_err(storage_error)?;
            sqlx::query("SET TRANSACTION READ ONLY")
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            sqlx::query("SELECT set_config('statement_timeout',$1,true)")
                .bind(plan.maximum_time.as_millis().to_string())
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            let mut query = compiled.query;
            let rows = query
                .build()
                .fetch_all(&mut *transaction)
                .await
                .map_err(storage_error)?;
            let result = decode(&plan, &compiled.outputs, rows)?;
            transaction.commit().await.map_err(storage_error)?;
            Ok(result)
        })
        .await
        .map_err(|_| ExploreStoreError::Unavailable)?
    }
}
impl ExploreStore for PostgresExploreStore {
    fn execute(&self, plan: ExplorePlan) -> PortFuture<'_, Result<ExploreResult>> {
        Box::pin(self.run(plan))
    }
}

#[derive(Clone, Copy)]
enum ValueType {
    Integer,
    IntegerSum,
    Number,
    Bytes,
    Hex,
    Bool,
    Code(ExploreField),
}
struct Output {
    name: Box<str>,
    kind: ValueType,
}
struct Compiled {
    query: QueryBuilder<Postgres>,
    outputs: Vec<Output>,
}

fn table(dataset: ExploreDataset) -> &'static str {
    match dataset {
        ExploreDataset::Errors => "metric.error_events",
        ExploreDataset::Logs => "metric.logs",
        ExploreDataset::Spans => "metric.spans",
        ExploreDataset::Metrics => "metric.metric_buckets",
    }
}
fn identity(dataset: ExploreDataset) -> &'static str {
    match dataset {
        ExploreDataset::Errors => "event_key",
        ExploreDataset::Logs => "log_id",
        ExploreDataset::Spans => "record_id",
        ExploreDataset::Metrics => "bucket_id",
    }
}
fn record_id(dataset: ExploreDataset) -> &'static str {
    if dataset == ExploreDataset::Errors {
        "event_id"
    } else {
        identity(dataset)
    }
}
fn nanos(dataset: ExploreDataset) -> bool {
    matches!(dataset, ExploreDataset::Logs | ExploreDataset::Spans)
}
fn column(dataset: ExploreDataset, field: ExploreField) -> Result<&'static str> {
    use ExploreField::*;
    if !field.accepted_by(dataset) {
        return Err(ExploreStoreError::InvalidData);
    }
    Ok(match field {
        Timestamp => match dataset {
            ExploreDataset::Errors => "occurred_at",
            ExploreDataset::Logs => "occurred_at_ns",
            ExploreDataset::Spans => "started_at_ns",
            ExploreDataset::Metrics => "bucket_start",
        },
        ReceivedAt => "received_at",
        Level => {
            if dataset == ExploreDataset::Errors {
                "level"
            } else {
                "severity"
            }
        }
        Platform => "platform",
        IssueId => "issue_id",
        Message => "message",
        Environment => "environment",
        Release => "release",
        Service => "service",
        TraceId => "trace_id",
        SpanId => "span_id",
        DurationMs => "duration_ns",
        OperationClass => "operation_class",
        Operation => "operation",
        Status => "status",
        Name => "name",
        IsSegment => "is_segment",
        MetricKind => "kind",
        Unit => "unit",
        MetricCount => "measurement_count",
        MetricSum => "aggregate_sum",
        MetricMin => "minimum",
        MetricMax => "maximum",
    })
}
fn value_type(dataset: ExploreDataset, field: ExploreField) -> ValueType {
    use ExploreField::*;
    match field {
        Timestamp if nanos(dataset) => ValueType::Number,
        Timestamp | ReceivedAt | MetricCount => ValueType::Integer,
        DurationMs | MetricSum | MetricMin | MetricMax => ValueType::Number,
        Level | Platform | OperationClass | MetricKind => ValueType::Code(field),
        IssueId | TraceId | SpanId => ValueType::Hex,
        IsSegment => ValueType::Bool,
        _ => ValueType::Bytes,
    }
}
fn projection(
    q: &mut QueryBuilder<Postgres>,
    dataset: ExploreDataset,
    field: ExploreField,
) -> Result<()> {
    q.push(column(dataset, field)?);
    match value_type(dataset, field) {
        ValueType::Code(_) => {
            q.push("::integer");
        }
        ValueType::Number
            if field == ExploreField::DurationMs || field == ExploreField::Timestamp =>
        {
            q.push("::float8/1000000");
        }
        _ => {}
    }
    Ok(())
}
// The only generated SQL identifiers are adapter-owned, bounded ordinal slots.
fn slot(q: &mut QueryBuilder<Postgres>, index: usize) {
    q.push(format!("f{index}"));
}

fn compile(plan: &ExplorePlan) -> Result<Compiled> {
    let dataset = plan.query.dataset;
    let raw = plan.query.aggregates.is_empty();
    let mut outputs = Vec::new();
    let mut q = QueryBuilder::new("WITH selected AS MATERIALIZED (SELECT ");
    if raw {
        q.push(column(dataset, ExploreField::Timestamp)?)
            .push(" AS page_time,")
            .push(identity(dataset))
            .push(" AS page_id,")
            .push(record_id(dataset))
            .push(" AS order_id,");
        for field in FIELDS
            .iter()
            .copied()
            .filter(|field| field.accepted_by(dataset))
        {
            if !outputs.is_empty() {
                q.push(",");
            }
            projection(&mut q, dataset, field)?;
            q.push(" AS ");
            slot(&mut q, outputs.len());
            outputs.push(Output {
                name: field.as_str().into(),
                kind: value_type(dataset, field),
            });
        }
    } else {
        if let Some(interval) = plan.query.interval {
            q.push("(floor((")
                .push(column(dataset, ExploreField::Timestamp)?);
            // Convert nanoseconds to the shared millisecond date first, truncating
            // toward zero; UTC interval flooring then handles negative dates.
            if nanos(dataset) {
                q.push("/1000000");
            }
            q.push(")::numeric/").push_bind(interval.millis());
            q.push(")*")
                .push_bind(interval.millis())
                .push(")::bigint AS f0");
            outputs.push(Output {
                name: "timestamp".into(),
                kind: ValueType::Integer,
            });
        }
        for field in &plan.query.group_by {
            if !outputs.is_empty() {
                q.push(",");
            }
            projection(&mut q, dataset, *field)?;
            q.push(" AS ");
            slot(&mut q, outputs.len());
            outputs.push(Output {
                name: field.as_str().into(),
                kind: value_type(dataset, *field),
            });
        }
        for aggregate in &plan.query.aggregates {
            if !outputs.is_empty() {
                q.push(",");
            }
            let kind = aggregate_sql(&mut q, dataset, aggregate)?;
            q.push(" AS ");
            slot(&mut q, outputs.len());
            outputs.push(Output {
                name: aggregate.alias.clone(),
                kind,
            });
        }
    }
    q.push(" FROM ")
        .push(table(dataset))
        .push(" WHERE project_id=")
        .push_bind(plan.project_id.get());
    if dataset == ExploreDataset::Errors {
        q.push(" AND processing_state=2");
    }
    let scale = if nanos(dataset) { 1_000_000 } else { 1 };
    let from = plan
        .query
        .from
        .unix_millis()
        .checked_mul(scale)
        .ok_or(ExploreStoreError::InvalidData)?;
    let until = plan
        .query
        .until
        .unix_millis()
        .checked_mul(scale)
        .ok_or(ExploreStoreError::InvalidData)?;
    let time = column(dataset, ExploreField::Timestamp)?;
    q.push(" AND ")
        .push(time)
        .push(">=")
        .push_bind(from)
        .push(" AND ")
        .push(time)
        .push("<")
        .push_bind(until);
    if let Some(expression) = &plan.query.expression {
        q.push(" AND ");
        expression_sql(&mut q, dataset, expression)?;
    } else {
        for predicate in &plan.query.predicates {
            q.push(" AND ");
            predicate_sql(&mut q, dataset, predicate)?;
        }
    }
    if let Some(cursor) = plan.query.cursor {
        // Error keys include the project prefix externally, but native ordering
        // uses the indexed 16-byte identity. Retain foreign-prefix comparison
        // semantics without ever widening the trusted project scope.
        let prefix = plan.project_id.get().to_be_bytes();
        let comparison = if dataset == ExploreDataset::Errors {
            prefix.as_slice().cmp(&cursor.id[..4])
        } else {
            std::cmp::Ordering::Equal
        };
        if comparison == std::cmp::Ordering::Equal {
            q.push(" AND (")
                .push(time)
                .push(",")
                .push(record_id(dataset))
                .push(")<(")
                .push_bind(cursor.time)
                .push(",")
                .push_bind(
                    cursor.id[if dataset == ExploreDataset::Errors {
                        4
                    } else {
                        0
                    }..usize::from(cursor.id_len)]
                        .to_vec(),
                )
                .push(")");
        } else {
            q.push(" AND ")
                .push(time)
                .push(if comparison == std::cmp::Ordering::Less {
                    "<="
                } else {
                    "<"
                })
                .push_bind(cursor.time);
        }
    }
    let groups = plan.query.group_by.len() + usize::from(plan.query.interval.is_some());
    if !raw {
        if groups > 0 {
            q.push(" GROUP BY ");
            for index in 0..groups {
                if index > 0 {
                    q.push(",");
                }
                slot(&mut q, index);
            }
        }
        q.push(" HAVING count(*)>0");
    }
    let order = if raw {
        "page_time DESC,order_id DESC".to_owned()
    } else if groups == 0 {
        "f0".to_owned()
    } else {
        (0..groups)
            .map(|index| format!("f{index} ASC NULLS FIRST"))
            .collect::<Vec<_>>()
            .join(",")
    };
    q.push(" ORDER BY ")
        .push(&order)
        .push(" LIMIT ")
        .push_bind((plan.query.limit + usize::from(raw)) as i64);
    q.push("), bounded AS (SELECT *,sum(1024::bigint");
    for (index, output) in outputs.iter().enumerate() {
        if matches!(output.kind, ValueType::Bytes) {
            q.push("+coalesce(octet_length(");
            slot(&mut q, index);
            q.push("),0)");
        }
    }
    q.push(") OVER(ORDER BY ")
        .push(&order)
        .push(" ROWS UNBOUNDED PRECEDING)::bigint AS response_bytes,row_number() OVER(ORDER BY ")
        .push(&order)
        .push(") AS result_rank FROM selected) SELECT response_bytes");
    if raw {
        q.push(",page_time,page_id");
    }
    for index in 0..outputs.len() {
        q.push(",CASE WHEN response_bytes<=")
            .push_bind(RESPONSE_BYTES)
            .push(" THEN ");
        slot(&mut q, index);
        q.push(" END AS ");
        slot(&mut q, index);
    }
    q.push(" FROM bounded ORDER BY result_rank");
    Ok(Compiled { query: q, outputs })
}

fn aggregate_sql(
    q: &mut QueryBuilder<Postgres>,
    dataset: ExploreDataset,
    a: &ExploreAggregate,
) -> Result<ValueType> {
    use ExploreAggregateKind::*;
    if a.kind == Count {
        q.push("count(*)::bigint");
        return Ok(ValueType::Integer);
    }
    let field = a.field.ok_or(ExploreStoreError::InvalidData)?;
    let integer = matches!(value_type(dataset, field), ValueType::Integer)
        && matches!(a.kind, Sum | Min | Max);
    let kind = if integer && a.kind == Sum {
        ValueType::IntegerSum
    } else if integer {
        ValueType::Integer
    } else {
        ValueType::Number
    };
    if let Some(percentile) = a.kind.percentile() {
        q.push("percentile_cont(")
            .push_bind(percentile)
            .push(") WITHIN GROUP(ORDER BY ");
        projection(q, dataset, field)?;
        q.push("::float8)");
    } else {
        if a.kind == Sum {
            q.push("coalesce(");
        }
        q.push(match a.kind {
            Sum => "sum(",
            Min => "min(",
            Max => "max(",
            Avg => "avg(",
            _ => return Err(ExploreStoreError::InvalidData),
        });
        projection(q, dataset, field)?;
        q.push(")");
        if a.kind == Sum {
            q.push(",0)");
        }
        q.push(if integer && a.kind == Sum {
            "::text"
        } else if integer {
            "::bigint"
        } else {
            "::float8"
        });
    }
    Ok(kind)
}

fn expression_sql(
    q: &mut QueryBuilder<Postgres>,
    dataset: ExploreDataset,
    expression: &ExploreExpression,
) -> Result<()> {
    match expression {
        ExploreExpression::Predicate(p) => predicate_sql(q, dataset, p)?,
        ExploreExpression::Not(child) => {
            // WHERE already treats unknown as false. Only NOT needs an explicit
            // missing-value conversion; keeping positive predicates unwrapped
            // lets PostgreSQL use their native index conditions.
            q.push("(NOT coalesce((");
            expression_sql(q, dataset, child)?;
            q.push("),false))");
        }
        ExploreExpression::And(children) | ExploreExpression::Or(children) => {
            if children.is_empty() {
                q.push("TRUE");
            } else {
                q.push("(");
                for (index, child) in children.iter().enumerate() {
                    if index > 0 {
                        q.push(if matches!(expression, ExploreExpression::And(_)) {
                            " AND "
                        } else {
                            " OR "
                        });
                    }
                    expression_sql(q, dataset, child)?;
                }
                q.push(")");
            }
        }
    }
    Ok(())
}
fn predicate_sql(
    q: &mut QueryBuilder<Postgres>,
    dataset: ExploreDataset,
    p: &ExplorePredicate,
) -> Result<()> {
    use ExplorePredicateOp::*;
    let col = column(dataset, p.field)?;
    if p.op == Present {
        let Some(ExploreValue::Bool(present)) = p.value else {
            return Err(ExploreStoreError::InvalidData);
        };
        q.push("(").push(col).push(if present {
            " IS NOT NULL)"
        } else {
            " IS NULL)"
        });
        return Ok(());
    }
    let value = p.value.as_ref().ok_or(ExploreStoreError::InvalidData)?;
    q.push("(");
    match p.op {
        Contains | StartsWith | EndsWith => {
            let ExploreValue::String(value) = value else {
                return Err(ExploreStoreError::InvalidData);
            };
            match p.op {
                Contains => {
                    q.push("position(")
                        .push_bind(value.as_bytes().to_vec())
                        .push(" IN ")
                        .push(col)
                        .push(")>0");
                }
                StartsWith => {
                    q.push("substring(")
                        .push(col)
                        .push(" FROM 1 FOR ")
                        .push_bind(value.len() as i32)
                        .push(")=")
                        .push_bind(value.as_bytes().to_vec());
                }
                EndsWith => {
                    suffix_sql(q, col, value.as_bytes());
                    // Retain the existing Mongo `$` anchor's final-LF behavior,
                    // using literal binary equality rather than executing regex.
                    q.push(" OR ");
                    let mut before_lf = value.as_bytes().to_vec();
                    before_lf.push(b'\n');
                    suffix_sql(q, col, &before_lf);
                }
                _ => unreachable!(),
            }
        }
        _ => {
            if p.op == Exact && dataset == ExploreDataset::Metrics && p.field == ExploreField::Name
            {
                let ExploreValue::String(value) = value else {
                    return Err(ExploreStoreError::InvalidData);
                };
                q.push("name_key=sha256(")
                    .push_bind(value.as_bytes().to_vec())
                    .push(") AND ");
            }
            q.push(col);
            if p.field == ExploreField::MetricCount {
                q.push("::numeric");
            }
            q.push(match p.op {
                Exact => "=",
                Range | GreaterOrEqual => ">=",
                Greater => ">",
                Less => "<",
                LessOrEqual => "<=",
                _ => return Err(ExploreStoreError::InvalidData),
            });
            bind_value(q, dataset, p.field, value)?;
            if p.op == Range {
                q.push(" AND ").push(col);
                if p.field == ExploreField::MetricCount {
                    q.push("::numeric");
                }
                q.push("<");
                bind_value(
                    q,
                    dataset,
                    p.field,
                    p.upper.as_ref().ok_or(ExploreStoreError::InvalidData)?,
                )?;
            }
        }
    }
    q.push(")");
    Ok(())
}
fn suffix_sql(q: &mut QueryBuilder<Postgres>, column: &'static str, value: &[u8]) {
    q.push("substring(")
        .push(column)
        .push(" FROM greatest(1,octet_length(")
        .push(column)
        .push(")-")
        .push_bind(value.len() as i32)
        .push("+1))=")
        .push_bind(value.to_vec());
}
fn bind_value(
    q: &mut QueryBuilder<Postgres>,
    dataset: ExploreDataset,
    field: ExploreField,
    value: &ExploreValue,
) -> Result<()> {
    // Avoid rounding a bigint column to float8 when a numeric predicate uses
    // a floating value beyond 2^53. Decimal bind syntax is still a parameter.
    if field == ExploreField::MetricCount {
        match value {
            ExploreValue::Integer(value) => {
                q.push_bind(*value).push("::numeric");
            }
            ExploreValue::Number(value) if value.is_finite() => {
                q.push_bind(value.to_string()).push("::numeric");
            }
            _ => return Err(ExploreStoreError::InvalidData),
        }
        return Ok(());
    }
    if let ValueType::Code(code_field) = value_type(dataset, field) {
        let ExploreValue::String(value) = value else {
            return Err(ExploreStoreError::InvalidData);
        };
        let value = match (code_field, value.as_ref()) {
            (ExploreField::Level, "warn") => "warning",
            (ExploreField::Platform, "node") => "javascript",
            (ExploreField::Platform, "cocoa") => "native",
            _ => value,
        };
        let code = labels(dataset, code_field)
            .iter()
            .position(|label| *label == value)
            .ok_or(ExploreStoreError::InvalidData)?;
        q.push_bind(code as i32 + code_base(dataset, code_field));
        return Ok(());
    }
    if field == ExploreField::DurationMs || (field == ExploreField::Timestamp && nanos(dataset)) {
        let value = match value {
            ExploreValue::Integer(value) => value
                .checked_mul(1_000_000)
                .ok_or(ExploreStoreError::InvalidData)?,
            ExploreValue::Number(value) if field == ExploreField::DurationMs => {
                let scaled = (value * 1_000_000.0).round();
                if !(0.0..9_223_372_036_854_775_808.0).contains(&scaled) {
                    return Err(ExploreStoreError::InvalidData);
                }
                scaled as i64
            }
            _ => return Err(ExploreStoreError::InvalidData),
        };
        q.push_bind(value);
        return Ok(());
    }
    match (value_type(dataset, field), value) {
        (ValueType::Bytes, ExploreValue::String(value)) => {
            q.push_bind(value.as_bytes().to_vec());
        }
        (ValueType::Hex, ExploreValue::String(value)) => {
            q.push("decode(")
                .push_bind(value.to_string())
                .push(",'hex')");
        }
        (ValueType::Bool, ExploreValue::Bool(value)) => {
            q.push_bind(*value);
        }
        (ValueType::Integer | ValueType::Number, ExploreValue::Integer(value)) => {
            q.push_bind(*value);
        }
        (ValueType::Integer | ValueType::Number, ExploreValue::Number(value))
            if value.is_finite() =>
        {
            q.push_bind(*value);
        }
        _ => return Err(ExploreStoreError::InvalidData),
    }
    Ok(())
}
fn code_base(dataset: ExploreDataset, field: ExploreField) -> i32 {
    i32::from(
        field == ExploreField::MetricKind
            || (field == ExploreField::Level && dataset == ExploreDataset::Logs),
    )
}
fn labels(dataset: ExploreDataset, field: ExploreField) -> &'static [&'static str] {
    match field {
        ExploreField::Level if dataset == ExploreDataset::Errors => {
            &["error", "debug", "info", "warning", "fatal"]
        }
        ExploreField::Level => &["trace", "debug", "info", "warning", "error", "fatal"],
        ExploreField::Platform => &[
            "other",
            "python",
            "javascript",
            "native",
            "java",
            "php",
            "ruby",
            "dotnet",
            "go",
            "rust",
        ],
        ExploreField::OperationClass => &[
            "other",
            "http.server",
            "http.client",
            "database",
            "cache",
            "queue",
            "file",
            "rpc",
            "function",
            "task",
            "ui",
            "resource",
        ],
        ExploreField::MetricKind => &["counter", "gauge", "distribution"],
        _ => &[],
    }
}

fn validate(plan: &ExplorePlan) -> Result<()> {
    let q = &plan.query;
    if !(1..=MAX_EXPLORE_ROWS).contains(&q.limit)
        || q.from >= q.until
        || (q.expression.is_none() && q.predicates.len() > MAX_EXPLORE_PREDICATES)
        || q.aggregates.len() > MAX_EXPLORE_AGGREGATES
        || q.group_by.len() > MAX_EXPLORE_GROUPS
        || plan.maximum_time < Duration::from_millis(100)
        || plan.maximum_time > Duration::from_secs(30)
        || (q.aggregates.is_empty() && (!q.group_by.is_empty() || q.interval.is_some()))
        || (!q.aggregates.is_empty() && q.cursor.is_some())
        || q.group_by.iter().any(|field| !field.groupable(q.dataset))
    {
        return Err(ExploreStoreError::InvalidData);
    }
    if let Some(cursor) = q.cursor
        && cursor.id_len
            != if q.dataset == ExploreDataset::Errors {
                20
            } else {
                16
            }
    {
        return Err(ExploreStoreError::InvalidData);
    }
    let mut nodes = 0;
    let mut predicates = 0;
    let mut alternatives = 0;
    let mut pending: Vec<_> = q.expression.iter().collect();
    while let Some(expression) = pending.pop() {
        nodes += 1;
        match expression {
            ExploreExpression::Predicate(p) => {
                predicates += 1;
                validate_predicate(q.dataset, p)?;
            }
            ExploreExpression::Not(child) => pending.push(child),
            ExploreExpression::And(children) => {
                if children.len() > MAX_EXPLORE_EXPRESSION_NODES {
                    return Err(ExploreStoreError::InvalidData);
                }
                pending.extend(children);
            }
            ExploreExpression::Or(children) => {
                if children.is_empty() {
                    return Err(ExploreStoreError::InvalidData);
                }
                alternatives += children.len();
                if alternatives > MAX_EXPLORE_OR_ALTERNATIVES {
                    return Err(ExploreStoreError::InvalidData);
                }
                pending.extend(children);
            }
        }
        if nodes > MAX_EXPLORE_EXPRESSION_NODES || predicates > MAX_EXPLORE_PREDICATES {
            return Err(ExploreStoreError::InvalidData);
        }
    }
    if q.expression.is_none() {
        for p in &q.predicates {
            validate_predicate(q.dataset, p)?;
        }
    }
    let mut aliases = std::collections::BTreeSet::new();
    for a in &q.aggregates {
        if a.alias.is_empty()
            || a.alias.len() > 32
            || !a
                .alias
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            || !aliases.insert(a.alias.as_ref())
            || (a.kind == ExploreAggregateKind::Count && a.field.is_some())
            || (a.kind != ExploreAggregateKind::Count
                && !a
                    .field
                    .is_some_and(|field| field.accepted_by(q.dataset) && field.numeric()))
        {
            return Err(ExploreStoreError::InvalidData);
        }
    }
    Ok(())
}
fn validate_predicate(dataset: ExploreDataset, p: &ExplorePredicate) -> Result<()> {
    use ExplorePredicateOp::*;
    let kind = value_type(dataset, p.field);
    column(dataset, p.field)?;
    let valid_value = |v: &ExploreValue| match (kind, v) {
        (ValueType::Bytes, ExploreValue::String(v)) => v.len() <= 256,
        (ValueType::Hex, ExploreValue::String(v)) => {
            v.len()
                == if p.field == ExploreField::SpanId {
                    16
                } else {
                    32
                }
                && v.bytes().all(|b| b.is_ascii_hexdigit())
        }
        (ValueType::Code(_), ExploreValue::String(_)) => true, // closed label map validated during compilation
        (ValueType::Bool, ExploreValue::Bool(_)) => true,
        (ValueType::Integer | ValueType::Number, ExploreValue::Integer(v)) => {
            if matches!(p.field, ExploreField::Timestamp | ExploreField::ReceivedAt) {
                Timestamp::from_unix_millis(*v).is_ok()
            } else {
                !matches!(
                    p.field,
                    ExploreField::DurationMs | ExploreField::MetricCount
                ) || *v >= 0
            }
        }
        (ValueType::Integer | ValueType::Number, ExploreValue::Number(v)) => {
            v.is_finite()
                && !matches!(p.field, ExploreField::Timestamp | ExploreField::ReceivedAt)
                && (!matches!(
                    p.field,
                    ExploreField::DurationMs | ExploreField::MetricCount
                ) || *v >= 0.0)
        }
        _ => false,
    };
    let valid = match p.op {
        Present => matches!(p.value, Some(ExploreValue::Bool(_))) && p.upper.is_none(),
        Contains | StartsWith | EndsWith => {
            matches!(kind, ValueType::Bytes)
                && matches!(&p.value,Some(ExploreValue::String(v)) if !v.is_empty()&&v.len()<=256)
                && p.upper.is_none()
        }
        Exact => p.value.as_ref().is_some_and(valid_value) && p.upper.is_none(),
        Range => {
            p.field.numeric()
                && p.value.as_ref().is_some_and(valid_value)
                && p.upper.as_ref().is_some_and(valid_value)
                && p.value
                    .as_ref()
                    .zip(p.upper.as_ref())
                    .is_some_and(|(a, b)| match (a, b) {
                        (ExploreValue::Integer(a), ExploreValue::Integer(b)) => a < b,
                        (ExploreValue::Number(a), ExploreValue::Number(b)) => a < b,
                        (ExploreValue::Integer(a), ExploreValue::Number(b)) => (*a as f64) < *b,
                        (ExploreValue::Number(a), ExploreValue::Integer(b)) => *a < (*b as f64),
                        _ => false,
                    })
        }
        _ => p.field.numeric() && p.value.as_ref().is_some_and(valid_value) && p.upper.is_none(),
    };
    if valid {
        Ok(())
    } else {
        Err(ExploreStoreError::InvalidData)
    }
}

fn decode(plan: &ExplorePlan, outputs: &[Output], rows: Vec<PgRow>) -> Result<ExploreResult> {
    let raw = plan.query.aggregates.is_empty();
    let mut result = ExploreResult {
        rows: Vec::new(),
        next: None,
    };
    let mut last = None;
    for row in rows {
        if row
            .try_get::<i64, _>("response_bytes")
            .map_err(storage_error)?
            > RESPONSE_BYTES
            || result.rows.len() == plan.query.limit
        {
            if !raw {
                return Err(ExploreStoreError::InvalidData);
            }
            result.next = last;
            break;
        }
        let mut values = BTreeMap::new();
        if raw {
            let bytes: Vec<u8> = row.try_get("page_id").map_err(storage_error)?;
            let expected = if plan.query.dataset == ExploreDataset::Errors {
                20
            } else {
                16
            };
            if bytes.len() != expected {
                return Err(ExploreStoreError::InvalidData);
            }
            let mut id = [0; 20];
            id[..bytes.len()].copy_from_slice(&bytes);
            last = Some(ExploreCursor {
                time: row.try_get("page_time").map_err(storage_error)?,
                id,
                id_len: bytes.len() as u8,
            });
            values.insert("id".into(), ExploreValue::String(hex(&bytes).into()));
        }
        for (index, output) in outputs.iter().enumerate() {
            let name = format!("f{index}");
            let value = match output.kind {
                ValueType::IntegerSum => row
                    .try_get::<Option<String>, _>(name.as_str())
                    .map_err(storage_error)?
                    .map(|value| {
                        if let Ok(value) = value.parse::<i64>() {
                            return Ok(ExploreValue::Integer(value));
                        }
                        let value = value
                            .parse::<f64>()
                            .map_err(|_| ExploreStoreError::InvalidData)?;
                        if value.is_finite() {
                            Ok(ExploreValue::Number(value))
                        } else {
                            Err(ExploreStoreError::InvalidData)
                        }
                    })
                    .transpose()?,
                ValueType::Integer => row
                    .try_get::<Option<i64>, _>(name.as_str())
                    .map_err(storage_error)?
                    .map(ExploreValue::Integer),
                ValueType::Number => row
                    .try_get::<Option<f64>, _>(name.as_str())
                    .map_err(storage_error)?
                    .map(|v| {
                        if v.is_finite() {
                            Ok(ExploreValue::Number(v))
                        } else {
                            Err(ExploreStoreError::InvalidData)
                        }
                    })
                    .transpose()?,
                ValueType::Bool => row
                    .try_get::<Option<bool>, _>(name.as_str())
                    .map_err(storage_error)?
                    .map(ExploreValue::Bool),
                ValueType::Bytes | ValueType::Hex => row
                    .try_get::<Option<Vec<u8>>, _>(name.as_str())
                    .map_err(storage_error)?
                    .map(|bytes| {
                        if matches!(output.kind, ValueType::Hex) {
                            Ok(hex(&bytes))
                        } else {
                            String::from_utf8(bytes).map_err(|_| ExploreStoreError::InvalidData)
                        }
                    })
                    .transpose()?
                    .map(|s| ExploreValue::String(s.into())),
                ValueType::Code(field) => row
                    .try_get::<Option<i32>, _>(name.as_str())
                    .map_err(storage_error)?
                    .map(|code| {
                        let index = usize::try_from(code - code_base(plan.query.dataset, field))
                            .map_err(|_| ExploreStoreError::InvalidData)?;
                        labels(plan.query.dataset, field)
                            .get(index)
                            .map(|s| ExploreValue::String((*s).into()))
                            .ok_or(ExploreStoreError::InvalidData)
                    })
                    .transpose()?,
            };
            values.insert(output.name.clone(), value.unwrap_or(ExploreValue::Null));
        }
        result.rows.push(ExploreRow { values });
    }
    Ok(result)
}
fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(value, "{byte:02x}");
    }
    value
}
fn storage_error(error: sqlx::Error) -> ExploreStoreError {
    if matches!(
        error,
        sqlx::Error::Decode(_)
            | sqlx::Error::ColumnDecode { .. }
            | sqlx::Error::ColumnNotFound(_)
            | sqlx::Error::TypeNotFound { .. }
    ) || error
        .as_database_error()
        .is_some_and(|e| e.code().is_some_and(|c| c.starts_with("22")))
    {
        ExploreStoreError::InvalidData
    } else {
        ExploreStoreError::Unavailable
    }
}

#[cfg(test)]
mod tests;
