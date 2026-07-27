//! MongoDB dataset adapters for the closed Unified Explore plan.

use std::collections::BTreeMap;

use futures_util::TryStreamExt;
use metric_domain::explore::{
    ExploreAggregateKind, ExploreCursor, ExploreDataset, ExploreField, ExplorePlan,
    ExplorePredicateOp, ExploreResult, ExploreRow, ExploreValue,
};
use metric_ports::{ExploreStore, ExploreStoreError, PortFuture};
use mongodb::{
    Database,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
};

#[derive(Clone)]
pub struct MongoExploreStore {
    database: Database,
}

impl MongoExploreStore {
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }

    async fn execute_inner(&self, plan: ExplorePlan) -> Result<ExploreResult, ExploreStoreError> {
        let collection = self
            .database
            .collection::<Document>(collection_name(plan.query.dataset));
        let filter = build_filter(&plan)?;
        if plan.query.aggregates.is_empty() {
            let mut cursor = collection
                .find(filter)
                .sort(doc! { "o": -1, "_id": -1 })
                .limit(i64::try_from(plan.query.limit.saturating_add(1)).unwrap_or(101))
                .max_time(plan.maximum_time)
                .await
                .map_err(unavailable)?;
            let mut documents = Vec::with_capacity(plan.query.limit.saturating_add(1));
            while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
                documents.push(document);
            }
            let has_more = documents.len() > plan.query.limit;
            documents.truncate(plan.query.limit);
            let rows = documents
                .iter()
                .map(|document| raw_row(plan.query.dataset, document))
                .collect::<Result<Vec<_>, _>>()?;
            let next = if has_more {
                documents
                    .last()
                    .map(|document| raw_cursor(plan.query.dataset, document))
                    .transpose()?
            } else {
                None
            };
            return Ok(ExploreResult { rows, next });
        }

        let pipeline = aggregate_pipeline(&plan, filter)?;
        let mut cursor = collection
            .aggregate(pipeline)
            .max_time(plan.maximum_time)
            .await
            .map_err(unavailable)?;
        let mut rows = Vec::new();
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            rows.push(aggregate_row(&plan, &document)?);
        }
        Ok(ExploreResult { rows, next: None })
    }
}

impl ExploreStore for MongoExploreStore {
    fn execute(
        &self,
        plan: ExplorePlan,
    ) -> PortFuture<'_, Result<ExploreResult, ExploreStoreError>> {
        Box::pin(self.execute_inner(plan))
    }
}

fn collection_name(dataset: ExploreDataset) -> &'static str {
    match dataset {
        ExploreDataset::Errors => "error_events",
        ExploreDataset::Logs => "logs",
        ExploreDataset::Spans => "spans",
    }
}

fn build_filter(plan: &ExplorePlan) -> Result<Document, ExploreStoreError> {
    let dataset = plan.query.dataset;
    let (from, until) = match dataset {
        ExploreDataset::Errors => (
            Bson::DateTime(DateTime::from_millis(plan.query.from.unix_millis())),
            Bson::DateTime(DateTime::from_millis(plan.query.until.unix_millis())),
        ),
        ExploreDataset::Logs | ExploreDataset::Spans => (
            Bson::Int64(
                plan.query
                    .from
                    .unix_millis()
                    .checked_mul(1_000_000)
                    .ok_or(ExploreStoreError::InvalidData)?,
            ),
            Bson::Int64(
                plan.query
                    .until
                    .unix_millis()
                    .checked_mul(1_000_000)
                    .ok_or(ExploreStoreError::InvalidData)?,
            ),
        ),
    };
    let mut clauses = vec![
        doc! { "p": plan.project_id.get() },
        doc! { "o": { "$gte": from, "$lt": until } },
    ];
    if dataset == ExploreDataset::Errors {
        clauses.push(doc! { "q": { "$exists": false } });
    }
    for predicate in &plan.query.predicates {
        let field = physical_field(dataset, predicate.field)?;
        let clause = match predicate.op {
            ExplorePredicateOp::Present => {
                let present = matches!(predicate.value, Some(ExploreValue::Bool(true)));
                if matches!(
                    (dataset, predicate.field),
                    (ExploreDataset::Errors, ExploreField::Level)
                        | (ExploreDataset::Spans, ExploreField::IsSegment)
                ) {
                    if present {
                        Document::new()
                    } else {
                        doc! { "_id": { "$exists": false } }
                    }
                } else {
                    doc! { field: { "$exists": present } }
                }
            }
            ExplorePredicateOp::Exact => {
                let value = predicate
                    .value
                    .as_ref()
                    .ok_or(ExploreStoreError::InvalidData)?;
                if dataset == ExploreDataset::Errors
                    && predicate.field == ExploreField::Level
                    && matches!(value, ExploreValue::String(value) if value.as_ref() == "error")
                {
                    doc! { field: { "$exists": false } }
                } else if dataset == ExploreDataset::Spans
                    && predicate.field == ExploreField::IsSegment
                    && matches!(value, ExploreValue::Bool(false))
                {
                    doc! { field: { "$ne": true } }
                } else {
                    doc! { field: physical_value(dataset, predicate.field, value)? }
                }
            }
            ExplorePredicateOp::Range => {
                let lower = predicate
                    .value
                    .as_ref()
                    .ok_or(ExploreStoreError::InvalidData)?;
                let upper = predicate
                    .upper
                    .as_ref()
                    .ok_or(ExploreStoreError::InvalidData)?;
                doc! { field: {
                    "$gte": physical_value(dataset, predicate.field, lower)?,
                    "$lt": physical_value(dataset, predicate.field, upper)?,
                }}
            }
        };
        clauses.push(clause);
    }
    if let Some(cursor) = plan.query.cursor {
        let id = Binary {
            subtype: BinarySubtype::Generic,
            bytes: cursor.id[..usize::from(cursor.id_len)].to_vec(),
        };
        let time = match dataset {
            ExploreDataset::Errors => Bson::DateTime(DateTime::from_millis(cursor.time)),
            ExploreDataset::Logs | ExploreDataset::Spans => Bson::Int64(cursor.time),
        };
        clauses.push(doc! {
            "$or": [
                { "o": { "$lt": time.clone() } },
                { "o": time, "_id": { "$lt": id } },
            ]
        });
    }
    Ok(doc! { "$and": clauses })
}

fn aggregate_pipeline(
    plan: &ExplorePlan,
    filter: Document,
) -> Result<Vec<Document>, ExploreStoreError> {
    let mut id = Document::new();
    if let Some(interval) = plan.query.interval {
        id.insert(
            "bucket",
            doc! {
                "$dateTrunc": {
                    "date": timestamp_expression(plan.query.dataset),
                    "unit": "millisecond",
                    "binSize": interval.millis(),
                }
            },
        );
    }
    for field in &plan.query.group_by {
        id.insert(
            field.as_str(),
            format!("${}", physical_field(plan.query.dataset, *field)?),
        );
    }
    let mut group = doc! { "_id": id };
    for aggregate in &plan.query.aggregates {
        let expression = aggregate
            .field
            .map(|field| numeric_expression(plan.query.dataset, field))
            .transpose()?;
        let accumulator = match aggregate.kind {
            ExploreAggregateKind::Count => doc! { "$sum": 1_i32 },
            ExploreAggregateKind::Sum => doc! {
                "$sum": expression.ok_or(ExploreStoreError::InvalidData)?
            },
            ExploreAggregateKind::Min => doc! {
                "$min": expression.ok_or(ExploreStoreError::InvalidData)?
            },
            ExploreAggregateKind::Max => doc! {
                "$max": expression.ok_or(ExploreStoreError::InvalidData)?
            },
            ExploreAggregateKind::Avg => doc! {
                "$avg": expression.ok_or(ExploreStoreError::InvalidData)?
            },
            percentile => doc! {
                "$percentile": {
                    "input": expression.ok_or(ExploreStoreError::InvalidData)?,
                    "p": [percentile.percentile().ok_or(ExploreStoreError::InvalidData)?],
                    "method": "approximate",
                }
            },
        };
        group.insert(aggregate.alias.as_ref(), accumulator);
    }
    Ok(vec![
        doc! { "$match": filter },
        doc! { "$group": group },
        doc! { "$sort": { "_id.bucket": 1, "_id": 1 } },
        doc! { "$limit": i64::try_from(plan.query.limit).unwrap_or(100) },
    ])
}

fn aggregate_row(plan: &ExplorePlan, document: &Document) -> Result<ExploreRow, ExploreStoreError> {
    let mut values = BTreeMap::new();
    let id = document
        .get_document("_id")
        .map_err(|_| ExploreStoreError::InvalidData)?;
    if plan.query.interval.is_some() {
        values.insert(
            "timestamp".into(),
            bson_value(id.get("bucket").ok_or(ExploreStoreError::InvalidData)?)?,
        );
    }
    for field in &plan.query.group_by {
        values.insert(
            field.as_str().into(),
            id.get(field.as_str())
                .map(|value| display_bson_value(plan.query.dataset, *field, value))
                .transpose()?
                .unwrap_or_else(|| default_value(plan.query.dataset, *field)),
        );
    }
    for aggregate in &plan.query.aggregates {
        let value = document
            .get(aggregate.alias.as_ref())
            .ok_or(ExploreStoreError::InvalidData)?;
        let value = match value {
            Bson::Array(values) if values.len() == 1 => bson_value(&values[0])?,
            value => bson_value(value)?,
        };
        values.insert(aggregate.alias.clone(), value);
    }
    Ok(ExploreRow { values })
}

fn raw_cursor(
    dataset: ExploreDataset,
    document: &Document,
) -> Result<ExploreCursor, ExploreStoreError> {
    let id = document
        .get_binary_generic("_id")
        .map_err(|_| ExploreStoreError::InvalidData)?;
    let mut padded = [0_u8; 20];
    if id.len() > padded.len() {
        return Err(ExploreStoreError::InvalidData);
    }
    padded[..id.len()].copy_from_slice(id);
    let time = match dataset {
        ExploreDataset::Errors => document
            .get_datetime("o")
            .map_err(|_| ExploreStoreError::InvalidData)?
            .timestamp_millis(),
        ExploreDataset::Logs | ExploreDataset::Spans => document
            .get_i64("o")
            .map_err(|_| ExploreStoreError::InvalidData)?,
    };
    Ok(ExploreCursor {
        time,
        id: padded,
        id_len: u8::try_from(id.len()).map_err(|_| ExploreStoreError::InvalidData)?,
    })
}

fn raw_row(dataset: ExploreDataset, document: &Document) -> Result<ExploreRow, ExploreStoreError> {
    let mut values = BTreeMap::new();
    values.insert(
        "id".into(),
        ExploreValue::String(
            hex::encode(
                document
                    .get_binary_generic("_id")
                    .map_err(|_| ExploreStoreError::InvalidData)?,
            )
            .into(),
        ),
    );
    for field in fields(dataset) {
        let key = physical_field(dataset, *field)?;
        let value = document
            .get(key)
            .map(|value| display_bson_value(dataset, *field, value))
            .transpose()?
            .unwrap_or_else(|| default_value(dataset, *field));
        values.insert(field.as_str().into(), value);
    }
    Ok(ExploreRow { values })
}

fn fields(dataset: ExploreDataset) -> &'static [ExploreField] {
    match dataset {
        ExploreDataset::Errors => &[
            ExploreField::Timestamp,
            ExploreField::ReceivedAt,
            ExploreField::Level,
            ExploreField::Platform,
            ExploreField::IssueId,
        ],
        ExploreDataset::Logs => &[
            ExploreField::Timestamp,
            ExploreField::ReceivedAt,
            ExploreField::Level,
            ExploreField::Message,
            ExploreField::Environment,
            ExploreField::Release,
            ExploreField::Service,
            ExploreField::TraceId,
            ExploreField::SpanId,
        ],
        ExploreDataset::Spans => &[
            ExploreField::Timestamp,
            ExploreField::ReceivedAt,
            ExploreField::DurationMs,
            ExploreField::OperationClass,
            ExploreField::Name,
            ExploreField::Operation,
            ExploreField::Status,
            ExploreField::Environment,
            ExploreField::Release,
            ExploreField::Service,
            ExploreField::TraceId,
            ExploreField::SpanId,
            ExploreField::IsSegment,
        ],
    }
}

fn physical_field(
    dataset: ExploreDataset,
    field: ExploreField,
) -> Result<&'static str, ExploreStoreError> {
    let value = match (dataset, field) {
        (_, ExploreField::Timestamp) => "o",
        (_, ExploreField::ReceivedAt) => "r",
        (ExploreDataset::Errors | ExploreDataset::Logs, ExploreField::Level) => "l",
        (ExploreDataset::Errors, ExploreField::Platform) => "a",
        (ExploreDataset::Errors, ExploreField::IssueId) => "u",
        (ExploreDataset::Logs, ExploreField::Message) => "m",
        (ExploreDataset::Logs | ExploreDataset::Spans, ExploreField::Environment) => "e",
        (ExploreDataset::Logs, ExploreField::Release) => "v",
        (ExploreDataset::Spans, ExploreField::Release) => "u",
        (ExploreDataset::Logs | ExploreDataset::Spans, ExploreField::Service) => "j",
        (ExploreDataset::Logs | ExploreDataset::Spans, ExploreField::TraceId) => "g",
        (ExploreDataset::Logs | ExploreDataset::Spans, ExploreField::SpanId) => "n",
        (ExploreDataset::Spans, ExploreField::DurationMs) => "d",
        (ExploreDataset::Spans, ExploreField::OperationClass) => "c",
        (ExploreDataset::Spans, ExploreField::Operation) => "w",
        (ExploreDataset::Spans, ExploreField::Status) => "v",
        (ExploreDataset::Spans, ExploreField::Name) => "m",
        (ExploreDataset::Spans, ExploreField::IsSegment) => "t",
        _ => return Err(ExploreStoreError::InvalidData),
    };
    Ok(value)
}

fn physical_value(
    dataset: ExploreDataset,
    field: ExploreField,
    value: &ExploreValue,
) -> Result<Bson, ExploreStoreError> {
    match (field, value) {
        (ExploreField::Timestamp, ExploreValue::Integer(value))
        | (ExploreField::ReceivedAt, ExploreValue::Integer(value)) => match (dataset, field) {
            (_, ExploreField::ReceivedAt) | (ExploreDataset::Errors, ExploreField::Timestamp) => {
                Ok(Bson::DateTime(DateTime::from_millis(*value)))
            }
            _ => value
                .checked_mul(1_000_000)
                .map(Bson::Int64)
                .ok_or(ExploreStoreError::InvalidData),
        },
        (ExploreField::DurationMs, ExploreValue::Integer(value)) => value
            .checked_mul(1_000_000)
            .map(Bson::Int64)
            .ok_or(ExploreStoreError::InvalidData),
        (ExploreField::DurationMs, ExploreValue::Number(value)) => {
            Ok(Bson::Int64((*value * 1_000_000.0).round() as i64))
        }
        (ExploreField::IsSegment, ExploreValue::Bool(value)) => Ok(Bson::Boolean(*value)),
        (ExploreField::Level, ExploreValue::String(value)) => Ok(Bson::Int32(
            level_code(dataset, value).ok_or(ExploreStoreError::InvalidData)?,
        )),
        (ExploreField::Platform, ExploreValue::String(value)) => Ok(Bson::Int32(
            platform_code(value).ok_or(ExploreStoreError::InvalidData)?,
        )),
        (ExploreField::OperationClass, ExploreValue::String(value)) => Ok(Bson::Int32(
            operation_class_code(value).ok_or(ExploreStoreError::InvalidData)?,
        )),
        (
            ExploreField::TraceId | ExploreField::SpanId | ExploreField::IssueId,
            ExploreValue::String(value),
        ) => {
            let bytes = hex::decode(value.as_ref()).map_err(|_| ExploreStoreError::InvalidData)?;
            Ok(Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes,
            }))
        }
        (_, ExploreValue::String(value)) => Ok(Bson::String(value.to_string())),
        _ => Err(ExploreStoreError::InvalidData),
    }
}

fn numeric_expression(
    dataset: ExploreDataset,
    field: ExploreField,
) -> Result<Bson, ExploreStoreError> {
    let physical = format!("${}", physical_field(dataset, field)?);
    Ok(match (dataset, field) {
        (_, ExploreField::ReceivedAt) | (ExploreDataset::Errors, ExploreField::Timestamp) => {
            Bson::Document(doc! { "$toLong": physical })
        }
        (
            ExploreDataset::Logs | ExploreDataset::Spans,
            ExploreField::Timestamp | ExploreField::DurationMs,
        ) => Bson::Document(doc! { "$divide": [physical, 1_000_000_f64] }),
        _ => return Err(ExploreStoreError::InvalidData),
    })
}

fn timestamp_expression(dataset: ExploreDataset) -> Bson {
    match dataset {
        ExploreDataset::Errors => Bson::String("$o".into()),
        ExploreDataset::Logs | ExploreDataset::Spans => {
            Bson::Document(doc! { "$toDate": { "$divide": ["$o", 1_000_000_i64] } })
        }
    }
}

fn bson_value(value: &Bson) -> Result<ExploreValue, ExploreStoreError> {
    match value {
        Bson::String(value) => Ok(ExploreValue::String(value.as_str().into())),
        Bson::Int32(value) => Ok(ExploreValue::Integer(i64::from(*value))),
        Bson::Int64(value) => Ok(ExploreValue::Integer(*value)),
        Bson::Double(value) if value.is_finite() => Ok(ExploreValue::Number(*value)),
        Bson::Boolean(value) => Ok(ExploreValue::Bool(*value)),
        Bson::DateTime(value) => Ok(ExploreValue::Integer(value.timestamp_millis())),
        Bson::Binary(value) if value.subtype == BinarySubtype::Generic => {
            Ok(ExploreValue::String(hex::encode(&value.bytes).into()))
        }
        Bson::Null => Ok(ExploreValue::Null),
        _ => Err(ExploreStoreError::InvalidData),
    }
}

fn display_bson_value(
    dataset: ExploreDataset,
    field: ExploreField,
    value: &Bson,
) -> Result<ExploreValue, ExploreStoreError> {
    if let Bson::Int32(code) = value {
        if field == ExploreField::Level {
            let label = match (dataset, *code) {
                (ExploreDataset::Errors, 1) | (ExploreDataset::Logs, 2) => "debug",
                (ExploreDataset::Errors, 2) | (ExploreDataset::Logs, 3) => "info",
                (ExploreDataset::Errors, 3) | (ExploreDataset::Logs, 4) => "warning",
                (ExploreDataset::Errors, 4) | (ExploreDataset::Logs, 6) => "fatal",
                (ExploreDataset::Logs, 1) => "trace",
                (ExploreDataset::Logs, 5) => "error",
                _ => return Err(ExploreStoreError::InvalidData),
            };
            return Ok(ExploreValue::String(label.into()));
        }
        if field == ExploreField::Platform {
            let label = match code {
                0 => "other",
                1 => "python",
                2 => "javascript",
                3 => "native",
                4 => "java",
                5 => "php",
                6 => "ruby",
                7 => "dotnet",
                8 => "go",
                9 => "rust",
                _ => return Err(ExploreStoreError::InvalidData),
            };
            return Ok(ExploreValue::String(label.into()));
        }
        if field == ExploreField::OperationClass {
            let label = match code {
                0 => "other",
                1 => "http.server",
                2 => "http.client",
                3 => "database",
                4 => "cache",
                5 => "queue",
                6 => "file",
                7 => "rpc",
                8 => "function",
                9 => "task",
                10 => "ui",
                11 => "resource",
                _ => return Err(ExploreStoreError::InvalidData),
            };
            return Ok(ExploreValue::String(label.into()));
        }
    }
    bson_value(value)
}

fn default_value(dataset: ExploreDataset, field: ExploreField) -> ExploreValue {
    match (dataset, field) {
        (ExploreDataset::Errors, ExploreField::Level) => ExploreValue::String("error".into()),
        (_, ExploreField::IsSegment) => ExploreValue::Bool(false),
        _ => ExploreValue::Null,
    }
}

fn level_code(dataset: ExploreDataset, value: &str) -> Option<i32> {
    match dataset {
        ExploreDataset::Errors => match value {
            "debug" => Some(1),
            "info" => Some(2),
            "warn" | "warning" => Some(3),
            "fatal" => Some(4),
            _ => None,
        },
        ExploreDataset::Logs => match value {
            "trace" => Some(1),
            "debug" => Some(2),
            "info" => Some(3),
            "warn" | "warning" => Some(4),
            "error" => Some(5),
            "fatal" => Some(6),
            _ => None,
        },
        ExploreDataset::Spans => None,
    }
}

fn platform_code(value: &str) -> Option<i32> {
    match value {
        "other" => Some(0),
        "python" => Some(1),
        "javascript" | "node" => Some(2),
        "native" | "cocoa" => Some(3),
        "java" => Some(4),
        "php" => Some(5),
        "ruby" => Some(6),
        "dotnet" => Some(7),
        "go" => Some(8),
        "rust" => Some(9),
        _ => None,
    }
}

fn operation_class_code(value: &str) -> Option<i32> {
    match value {
        "other" => Some(0),
        "http.server" => Some(1),
        "http.client" => Some(2),
        "database" => Some(3),
        "cache" => Some(4),
        "queue" => Some(5),
        "file" => Some(6),
        "rpc" => Some(7),
        "function" => Some(8),
        "task" => Some(9),
        "ui" => Some(10),
        "resource" => Some(11),
        _ => None,
    }
}

fn unavailable(_: mongodb::error::Error) -> ExploreStoreError {
    ExploreStoreError::Unavailable
}
