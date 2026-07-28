use futures_util::TryStreamExt;
use metric_domain::{
    ProjectId, Timestamp,
    auth::UserId,
    dashboards::{
        Dashboard, DashboardId, DashboardRefreshInterval, DashboardWidget, DashboardWidgetId,
        SavedQuery, SavedQueryId, WidgetShape,
    },
    explore::{
        ExploreAggregate, ExploreAggregateKind, ExploreDataset, ExploreField, ExploreInterval,
        ExplorePredicate, ExplorePredicateOp, ExploreQuery, ExploreValue,
    },
};
use metric_ports::{DashboardStore, DashboardStoreError, PortFuture};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    error::{Error as MongoError, ErrorKind, WriteFailure},
    options::IndexOptions,
};

#[derive(Clone)]
pub struct MongoDashboardStore {
    database: Database,
}

impl MongoDashboardStore {
    #[must_use]
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

impl DashboardStore for MongoDashboardStore {
    fn list_saved_queries(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<SavedQuery>, DashboardStoreError>> {
        Box::pin(async move {
            let cursor = self
                .database
                .collection::<Document>("saved_queries")
                .find(doc! { "project_id": project_id.get() })
                .sort(doc! { "updated_at": -1, "_id": -1 })
                .limit(i64::try_from(limit).map_err(|_| DashboardStoreError::InvalidData)?)
                .await
                .map_err(map_mongo)?;
            cursor
                .map_err(map_mongo)
                .and_then(|document| async move { decode_saved_query(document) })
                .try_collect()
                .await
        })
    }

    fn load_saved_query(
        &self,
        project_id: ProjectId,
        id: SavedQueryId,
    ) -> PortFuture<'_, Result<SavedQuery, DashboardStoreError>> {
        Box::pin(async move {
            self.database
                .collection::<Document>("saved_queries")
                .find_one(doc! { "_id": binary(id.as_bytes()), "project_id": project_id.get() })
                .await
                .map_err(map_mongo)?
                .ok_or(DashboardStoreError::NotFound)
                .and_then(decode_saved_query)
        })
    }

    fn insert_saved_query(
        &self,
        value: SavedQuery,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move {
            value
                .validate()
                .map_err(|_| DashboardStoreError::InvalidData)?;
            self.database
                .collection::<Document>("saved_queries")
                .insert_one(encode_saved_query(&value)?)
                .await
                .map_err(map_mongo)?;
            Ok(())
        })
    }

    fn replace_saved_query(
        &self,
        value: SavedQuery,
        expected_revision: u64,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move {
            value
                .validate()
                .map_err(|_| DashboardStoreError::InvalidData)?;
            let result = self
                .database
                .collection::<Document>("saved_queries")
                .replace_one(
                    doc! {
                        "_id": binary(value.id.as_bytes()),
                        "project_id": value.project_id.get(),
                        "revision": to_i64(expected_revision)?,
                    },
                    encode_saved_query(&value)?,
                )
                .await
                .map_err(map_mongo)?;
            if result.matched_count == 0 {
                return Err(DashboardStoreError::Conflict);
            }
            Ok(())
        })
    }

    fn delete_saved_query(
        &self,
        project_id: ProjectId,
        id: SavedQueryId,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move {
            let result = self
                .database
                .collection::<Document>("saved_queries")
                .delete_one(doc! { "_id": binary(id.as_bytes()), "project_id": project_id.get() })
                .await
                .map_err(map_mongo)?;
            if result.deleted_count == 0 {
                return Err(DashboardStoreError::NotFound);
            }
            Ok(())
        })
    }

    fn list_dashboards(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<Dashboard>, DashboardStoreError>> {
        Box::pin(async move {
            let cursor = self
                .database
                .collection::<Document>("dashboards")
                .find(doc! { "project_id": project_id.get() })
                .sort(doc! { "updated_at": -1, "_id": -1 })
                .limit(i64::try_from(limit).map_err(|_| DashboardStoreError::InvalidData)?)
                .await
                .map_err(map_mongo)?;
            cursor
                .map_err(map_mongo)
                .and_then(|document| async move { decode_dashboard(document) })
                .try_collect()
                .await
        })
    }

    fn load_dashboard(
        &self,
        project_id: ProjectId,
        id: DashboardId,
    ) -> PortFuture<'_, Result<Dashboard, DashboardStoreError>> {
        Box::pin(async move {
            self.database
                .collection::<Document>("dashboards")
                .find_one(doc! { "_id": binary(id.as_bytes()), "project_id": project_id.get() })
                .await
                .map_err(map_mongo)?
                .ok_or(DashboardStoreError::NotFound)
                .and_then(decode_dashboard)
        })
    }

    fn insert_dashboard(
        &self,
        value: Dashboard,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move {
            value
                .validate()
                .map_err(|_| DashboardStoreError::InvalidData)?;
            self.database
                .collection::<Document>("dashboards")
                .insert_one(encode_dashboard(&value)?)
                .await
                .map_err(map_mongo)?;
            Ok(())
        })
    }

    fn replace_dashboard(
        &self,
        value: Dashboard,
        expected_revision: u64,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move {
            value
                .validate()
                .map_err(|_| DashboardStoreError::InvalidData)?;
            let result = self
                .database
                .collection::<Document>("dashboards")
                .replace_one(
                    doc! {
                        "_id": binary(value.id.as_bytes()),
                        "project_id": value.project_id.get(),
                        "revision": to_i64(expected_revision)?,
                    },
                    encode_dashboard(&value)?,
                )
                .await
                .map_err(map_mongo)?;
            if result.matched_count == 0 {
                return Err(DashboardStoreError::Conflict);
            }
            Ok(())
        })
    }

    fn delete_dashboard(
        &self,
        project_id: ProjectId,
        id: DashboardId,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move {
            let result = self
                .database
                .collection::<Document>("dashboards")
                .delete_one(doc! { "_id": binary(id.as_bytes()), "project_id": project_id.get() })
                .await
                .map_err(map_mongo)?;
            if result.deleted_count == 0 {
                return Err(DashboardStoreError::NotFound);
            }
            Ok(())
        })
    }
}

pub(crate) async fn create_dashboard_indexes(database: &Database) -> Result<(), MongoError> {
    for collection in ["saved_queries", "dashboards"] {
        database
            .collection::<Document>(collection)
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "project_id": 1, "name": 1 })
                    .options(
                        IndexOptions::builder()
                            .name(format!("{collection}_project_name_unique"))
                            .unique(true)
                            .build(),
                    )
                    .build(),
            )
            .await?;
        database
            .collection::<Document>(collection)
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "project_id": 1, "updated_at": -1, "_id": -1 })
                    .options(
                        IndexOptions::builder()
                            .name(format!("{collection}_project_updated"))
                            .build(),
                    )
                    .build(),
            )
            .await?;
    }
    Ok(())
}

pub(crate) fn saved_query_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "project_id", "name", "query", "revision", "created_by", "updated_by", "created_at", "updated_at"],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "project_id": { "bsonType": "int", "minimum": 1 },
                "name": { "bsonType": "string", "minLength": 1, "maxLength": 120 },
                "query": { "bsonType": "object" },
                "revision": { "bsonType": "long", "minimum": 1 },
                "created_by": { "bsonType": "long", "minimum": 1 },
                "updated_by": { "bsonType": "long", "minimum": 1 },
                "created_at": { "bsonType": "date" },
                "updated_at": { "bsonType": "date" },
            }
        }
    }
}

pub(crate) fn dashboard_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "project_id", "name", "widgets", "refresh_interval", "revision", "created_by", "updated_by", "created_at", "updated_at"],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "project_id": { "bsonType": "int", "minimum": 1 },
                "name": { "bsonType": "string", "minLength": 1, "maxLength": 120 },
                "widgets": {
                    "bsonType": "array", "minItems": 1, "maxItems": 8,
                    "items": {
                        "bsonType": "object",
                        "required": ["id", "title", "saved_query_id", "shape"],
                        "additionalProperties": false,
                        "properties": {
                            "id": { "bsonType": "binData" },
                            "title": { "bsonType": "string", "minLength": 1, "maxLength": 120 },
                            "saved_query_id": { "bsonType": "binData" },
                            "shape": { "enum": ["number", "table", "timeseries"] },
                        }
                    }
                },
                "refresh_interval": { "enum": ["manual", "30s", "1m", "5m"] },
                "revision": { "bsonType": "long", "minimum": 1 },
                "created_by": { "bsonType": "long", "minimum": 1 },
                "updated_by": { "bsonType": "long", "minimum": 1 },
                "created_at": { "bsonType": "date" },
                "updated_at": { "bsonType": "date" },
            }
        }
    }
}

fn encode_saved_query(value: &SavedQuery) -> Result<Document, DashboardStoreError> {
    Ok(doc! {
        "_id": binary(value.id.as_bytes()),
        "project_id": value.project_id.get(),
        "name": value.name.as_ref(),
        "query": encode_query(&value.query)?,
        "revision": to_i64(value.revision)?,
        "created_by": to_i64(value.created_by.get())?,
        "updated_by": to_i64(value.updated_by.get())?,
        "created_at": DateTime::from_millis(value.created_at.unix_millis()),
        "updated_at": DateTime::from_millis(value.updated_at.unix_millis()),
    })
}

fn decode_saved_query(document: Document) -> Result<SavedQuery, DashboardStoreError> {
    let value = SavedQuery {
        id: SavedQueryId::from_bytes(required_binary(&document, "_id")?),
        project_id: ProjectId::new(required_i32(&document, "project_id")?)
            .map_err(|_| DashboardStoreError::InvalidData)?,
        name: required_str(&document, "name")?.into(),
        query: decode_query(document.get_document("query").map_err(invalid)?)?,
        revision: required_u64(&document, "revision")?,
        created_by: UserId::new(required_u64(&document, "created_by")?)
            .map_err(|_| DashboardStoreError::InvalidData)?,
        updated_by: UserId::new(required_u64(&document, "updated_by")?)
            .map_err(|_| DashboardStoreError::InvalidData)?,
        created_at: required_time(&document, "created_at")?,
        updated_at: required_time(&document, "updated_at")?,
    };
    value
        .validate()
        .map_err(|_| DashboardStoreError::InvalidData)?;
    Ok(value)
}

fn encode_dashboard(value: &Dashboard) -> Result<Document, DashboardStoreError> {
    Ok(doc! {
        "_id": binary(value.id.as_bytes()),
        "project_id": value.project_id.get(),
        "name": value.name.as_ref(),
        "widgets": value.widgets.iter().map(|widget| Bson::Document(doc! {
            "id": binary(widget.id.as_bytes()),
            "title": widget.title.as_ref(),
            "saved_query_id": binary(widget.saved_query_id.as_bytes()),
            "shape": widget.shape.as_str(),
        })).collect::<Vec<_>>(),
        "refresh_interval": value.refresh_interval.as_str(),
        "revision": to_i64(value.revision)?,
        "created_by": to_i64(value.created_by.get())?,
        "updated_by": to_i64(value.updated_by.get())?,
        "created_at": DateTime::from_millis(value.created_at.unix_millis()),
        "updated_at": DateTime::from_millis(value.updated_at.unix_millis()),
    })
}

fn decode_dashboard(document: Document) -> Result<Dashboard, DashboardStoreError> {
    let widgets = document
        .get_array("widgets")
        .map_err(invalid)?
        .iter()
        .map(|value| {
            let value = value
                .as_document()
                .ok_or(DashboardStoreError::InvalidData)?;
            Ok(DashboardWidget {
                id: DashboardWidgetId::from_bytes(required_binary(value, "id")?),
                title: required_str(value, "title")?.into(),
                saved_query_id: SavedQueryId::from_bytes(required_binary(value, "saved_query_id")?),
                shape: WidgetShape::parse(required_str(value, "shape")?)
                    .map_err(|_| DashboardStoreError::InvalidData)?,
            })
        })
        .collect::<Result<Vec<_>, DashboardStoreError>>()?;
    let value = Dashboard {
        id: DashboardId::from_bytes(required_binary(&document, "_id")?),
        project_id: ProjectId::new(required_i32(&document, "project_id")?)
            .map_err(|_| DashboardStoreError::InvalidData)?,
        name: required_str(&document, "name")?.into(),
        widgets,
        refresh_interval: DashboardRefreshInterval::parse(required_str(
            &document,
            "refresh_interval",
        )?)
        .map_err(|_| DashboardStoreError::InvalidData)?,
        revision: required_u64(&document, "revision")?,
        created_by: UserId::new(required_u64(&document, "created_by")?)
            .map_err(|_| DashboardStoreError::InvalidData)?,
        updated_by: UserId::new(required_u64(&document, "updated_by")?)
            .map_err(|_| DashboardStoreError::InvalidData)?,
        created_at: required_time(&document, "created_at")?,
        updated_at: required_time(&document, "updated_at")?,
    };
    value
        .validate()
        .map_err(|_| DashboardStoreError::InvalidData)?;
    Ok(value)
}

fn encode_query(query: &ExploreQuery) -> Result<Document, DashboardStoreError> {
    if query.cursor.is_some() {
        return Err(DashboardStoreError::InvalidData);
    }
    Ok(doc! {
        "dataset": query.dataset.as_str(),
        "from": query.from.unix_millis(),
        "until": query.until.unix_millis(),
        "predicates": query.predicates.iter().map(|predicate| {
            let mut document = doc! {
                "field": predicate.field.as_str(),
                "op": match predicate.op {
                    ExplorePredicateOp::Exact => "exact",
                    ExplorePredicateOp::Present => "present",
                    ExplorePredicateOp::Range => "range",
                },
            };
            if let Some(value) = &predicate.value {
                document.insert("value", encode_value(value));
            }
            if let Some(value) = &predicate.upper {
                document.insert("upper", encode_value(value));
            }
            Bson::Document(document)
        }).collect::<Vec<_>>(),
        "aggregates": query.aggregates.iter().map(|aggregate| {
            let mut document = doc! {
                "function": aggregate.kind.as_str(),
                "alias": aggregate.alias.as_ref(),
            };
            if let Some(field) = aggregate.field {
                document.insert("field", field.as_str());
            }
            Bson::Document(document)
        }).collect::<Vec<_>>(),
        "group_by": query.group_by.iter().map(|field| field.as_str()).collect::<Vec<_>>(),
        "interval": query.interval.map(ExploreInterval::as_str),
        "limit": i64::try_from(query.limit).map_err(|_| DashboardStoreError::InvalidData)?,
    })
}

fn decode_query(document: &Document) -> Result<ExploreQuery, DashboardStoreError> {
    Ok(ExploreQuery {
        dataset: parse_dataset(required_str(document, "dataset")?)?,
        from: Timestamp::from_unix_millis(required_i64(document, "from")?)
            .map_err(|_| DashboardStoreError::InvalidData)?,
        until: Timestamp::from_unix_millis(required_i64(document, "until")?)
            .map_err(|_| DashboardStoreError::InvalidData)?,
        predicates: document
            .get_array("predicates")
            .map_err(invalid)?
            .iter()
            .map(|value| {
                let value = value
                    .as_document()
                    .ok_or(DashboardStoreError::InvalidData)?;
                Ok(ExplorePredicate {
                    field: parse_field(required_str(value, "field")?)?,
                    op: match required_str(value, "op")? {
                        "exact" => ExplorePredicateOp::Exact,
                        "present" => ExplorePredicateOp::Present,
                        "range" => ExplorePredicateOp::Range,
                        _ => return Err(DashboardStoreError::InvalidData),
                    },
                    value: value.get("value").map(decode_value).transpose()?,
                    upper: value.get("upper").map(decode_value).transpose()?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        aggregates: document
            .get_array("aggregates")
            .map_err(invalid)?
            .iter()
            .map(|value| {
                let value = value
                    .as_document()
                    .ok_or(DashboardStoreError::InvalidData)?;
                Ok(ExploreAggregate {
                    kind: parse_aggregate(required_str(value, "function")?)?,
                    field: value.get_str("field").ok().map(parse_field).transpose()?,
                    alias: required_str(value, "alias")?.into(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        group_by: document
            .get_array("group_by")
            .map_err(invalid)?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or(DashboardStoreError::InvalidData)
                    .and_then(parse_field)
            })
            .collect::<Result<Vec<_>, _>>()?,
        interval: match document.get("interval") {
            Some(Bson::String(value)) => Some(parse_interval(value)?),
            Some(Bson::Null) | None => None,
            _ => return Err(DashboardStoreError::InvalidData),
        },
        cursor: None,
        limit: usize::try_from(required_i64(document, "limit")?)
            .map_err(|_| DashboardStoreError::InvalidData)?,
    })
}

fn encode_value(value: &ExploreValue) -> Bson {
    match value {
        ExploreValue::String(value) => Bson::String(value.to_string()),
        ExploreValue::Number(value) => Bson::Double(*value),
        ExploreValue::Integer(value) => Bson::Int64(*value),
        ExploreValue::Bool(value) => Bson::Boolean(*value),
        ExploreValue::Null => Bson::Null,
    }
}

fn decode_value(value: &Bson) -> Result<ExploreValue, DashboardStoreError> {
    match value {
        Bson::String(value) => Ok(ExploreValue::String(value.clone().into())),
        Bson::Double(value) => Ok(ExploreValue::Number(*value)),
        Bson::Int32(value) => Ok(ExploreValue::Integer(i64::from(*value))),
        Bson::Int64(value) => Ok(ExploreValue::Integer(*value)),
        Bson::Boolean(value) => Ok(ExploreValue::Bool(*value)),
        Bson::Null => Ok(ExploreValue::Null),
        _ => Err(DashboardStoreError::InvalidData),
    }
}

fn parse_dataset(value: &str) -> Result<ExploreDataset, DashboardStoreError> {
    match value {
        "errors" => Ok(ExploreDataset::Errors),
        "logs" => Ok(ExploreDataset::Logs),
        "spans" => Ok(ExploreDataset::Spans),
        "metrics" => Ok(ExploreDataset::Metrics),
        _ => Err(DashboardStoreError::InvalidData),
    }
}

fn parse_field(value: &str) -> Result<ExploreField, DashboardStoreError> {
    match value {
        "timestamp" => Ok(ExploreField::Timestamp),
        "received_at" => Ok(ExploreField::ReceivedAt),
        "level" => Ok(ExploreField::Level),
        "platform" => Ok(ExploreField::Platform),
        "issue_id" => Ok(ExploreField::IssueId),
        "message" => Ok(ExploreField::Message),
        "environment" => Ok(ExploreField::Environment),
        "release" => Ok(ExploreField::Release),
        "service" => Ok(ExploreField::Service),
        "trace_id" => Ok(ExploreField::TraceId),
        "span_id" => Ok(ExploreField::SpanId),
        "duration_ms" => Ok(ExploreField::DurationMs),
        "operation_class" => Ok(ExploreField::OperationClass),
        "operation" => Ok(ExploreField::Operation),
        "status" => Ok(ExploreField::Status),
        "name" => Ok(ExploreField::Name),
        "is_segment" => Ok(ExploreField::IsSegment),
        _ => Err(DashboardStoreError::InvalidData),
    }
}

fn parse_aggregate(value: &str) -> Result<ExploreAggregateKind, DashboardStoreError> {
    match value {
        "count" => Ok(ExploreAggregateKind::Count),
        "sum" => Ok(ExploreAggregateKind::Sum),
        "min" => Ok(ExploreAggregateKind::Min),
        "max" => Ok(ExploreAggregateKind::Max),
        "avg" => Ok(ExploreAggregateKind::Avg),
        "p50" => Ok(ExploreAggregateKind::P50),
        "p75" => Ok(ExploreAggregateKind::P75),
        "p90" => Ok(ExploreAggregateKind::P90),
        "p95" => Ok(ExploreAggregateKind::P95),
        "p99" => Ok(ExploreAggregateKind::P99),
        _ => Err(DashboardStoreError::InvalidData),
    }
}

fn parse_interval(value: &str) -> Result<ExploreInterval, DashboardStoreError> {
    match value {
        "1m" => Ok(ExploreInterval::Minute),
        "5m" => Ok(ExploreInterval::FiveMinutes),
        "1h" => Ok(ExploreInterval::Hour),
        "1d" => Ok(ExploreInterval::Day),
        _ => Err(DashboardStoreError::InvalidData),
    }
}

fn required_str<'a>(document: &'a Document, field: &str) -> Result<&'a str, DashboardStoreError> {
    document.get_str(field).map_err(invalid)
}

fn required_i32(document: &Document, field: &str) -> Result<i32, DashboardStoreError> {
    document.get_i32(field).map_err(invalid)
}

fn required_i64(document: &Document, field: &str) -> Result<i64, DashboardStoreError> {
    document.get_i64(field).map_err(invalid)
}

fn required_u64(document: &Document, field: &str) -> Result<u64, DashboardStoreError> {
    u64::try_from(required_i64(document, field)?).map_err(|_| DashboardStoreError::InvalidData)
}

fn required_time(document: &Document, field: &str) -> Result<Timestamp, DashboardStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(field)
            .map_err(invalid)?
            .timestamp_millis(),
    )
    .map_err(|_| DashboardStoreError::InvalidData)
}

fn required_binary(document: &Document, field: &str) -> Result<[u8; 16], DashboardStoreError> {
    document
        .get_binary_generic(field)
        .map_err(invalid)?
        .as_slice()
        .try_into()
        .map_err(|_| DashboardStoreError::InvalidData)
}

fn to_i64(value: u64) -> Result<i64, DashboardStoreError> {
    i64::try_from(value).map_err(|_| DashboardStoreError::InvalidData)
}

fn binary(value: [u8; 16]) -> Bson {
    Bson::Binary(Binary {
        subtype: BinarySubtype::Generic,
        bytes: value.to_vec(),
    })
}

fn invalid<T>(_: T) -> DashboardStoreError {
    DashboardStoreError::InvalidData
}

fn map_mongo(error: MongoError) -> DashboardStoreError {
    if matches!(
        error.kind.as_ref(),
        ErrorKind::Write(WriteFailure::WriteError(error)) if error.code == 11000
    ) {
        DashboardStoreError::Conflict
    } else {
        DashboardStoreError::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_encoded_dashboard_dataset_can_be_decoded() {
        for dataset in [
            ExploreDataset::Errors,
            ExploreDataset::Logs,
            ExploreDataset::Spans,
            ExploreDataset::Metrics,
        ] {
            assert_eq!(parse_dataset(dataset.as_str()), Ok(dataset));
        }
    }
}
