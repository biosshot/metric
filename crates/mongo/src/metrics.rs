use std::collections::{BTreeMap, BTreeSet};

use metric_domain::{
    ProjectId, Timestamp,
    metrics::{METRIC_SKETCH_BINS, MetricAggregate, MetricDelta, MetricDeltaBatch},
};
use metric_ports::{DurableOutcome, MetricStore, PortFuture, SignalStoreError};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::IndexOptions,
};

#[derive(Debug, Clone, Copy)]
pub struct MetricRetention {
    pub days: u32,
    pub max_series_per_project: usize,
    pub archive: bool,
}

impl Default for MetricRetention {
    fn default() -> Self {
        Self {
            days: 90,
            max_series_per_project: 10_000,
            archive: false,
        }
    }
}

#[derive(Clone)]
pub struct MongoMetricStore {
    database: Database,
    retention: MetricRetention,
}

impl MongoMetricStore {
    #[must_use]
    pub const fn new(database: Database, retention: MetricRetention) -> Self {
        Self {
            database,
            retention,
        }
    }

    async fn persist_inner(
        &self,
        batch: MetricDeltaBatch,
    ) -> Result<DurableOutcome, SignalStoreError> {
        if batch.is_empty()
            || self.retention.days == 0
            || self.retention.max_series_per_project == 0
        {
            return Err(SignalStoreError::InvalidData);
        }
        self.check_cardinality(&batch).await?;
        for delta in batch.deltas.into_values() {
            self.apply_delta(delta).await?;
        }
        Ok(DurableOutcome::Accepted)
    }

    async fn check_cardinality(&self, batch: &MetricDeltaBatch) -> Result<(), SignalStoreError> {
        let mut requested = BTreeMap::<ProjectId, BTreeSet<[u8; 16]>>::new();
        for delta in batch.deltas.values() {
            requested
                .entry(delta.series.project_id)
                .or_default()
                .insert(delta.series.id());
        }
        let collection = self.database.collection::<Document>("metric_buckets");
        for (project_id, requested) in requested {
            let existing = collection
                .distinct("s", doc! { "p": project_id.get() })
                .await
                .map_err(|_| SignalStoreError::Unavailable)?;
            let existing = existing
                .into_iter()
                .filter_map(|value| fixed_binary(value).ok())
                .collect::<BTreeSet<_>>();
            let novel = requested
                .iter()
                .filter(|series| !existing.contains(*series))
                .count();
            if existing.len().saturating_add(novel) > self.retention.max_series_per_project {
                return Err(SignalStoreError::Capacity);
            }
        }
        Ok(())
    }

    async fn apply_delta(&self, delta: MetricDelta) -> Result<(), SignalStoreError> {
        let bucket_id = delta.bucket_id();
        let expires_at = delta
            .bucket_start
            .unix_millis()
            .checked_add(i64::from(self.retention.days) * 86_400_000)
            .and_then(|value| Timestamp::from_unix_millis(value).ok())
            .ok_or(SignalStoreError::InvalidData)?;
        let mut tags = Document::new();
        for (key, value) in &delta.series.tags {
            tags.insert(key.as_ref(), value.as_ref());
        }
        let mut set_on_insert = doc! {
            "p": delta.series.project_id.get(),
            "s": binary(delta.series.id()),
            "n": delta.series.name.as_ref(),
            "k": delta.series.kind.code(),
            "u": delta.series.unit.as_ref(),
            "a": tags,
            "t": date(delta.bucket_start),
            "w": i64::from(delta.bucket_width_seconds),
        };
        if self.retention.archive {
            set_on_insert.insert("h", date(expires_at));
        } else {
            set_on_insert.insert("z", date(expires_at));
        }
        if let Some(trace_id) = delta.trace_id {
            set_on_insert.insert("g", binary(trace_id.as_bytes()));
        }
        let mut update = doc! {
            "$setOnInsert": set_on_insert,
            "$set": { "r": date(delta.received_at) },
        };
        match delta.aggregate {
            MetricAggregate::Counter { sum, count } => {
                update.insert("$inc", doc! { "s0": sum, "c": as_i64(count)? });
            }
            MetricAggregate::Gauge {
                last,
                min,
                max,
                sum,
                count,
            } => {
                update.insert(
                    "$set",
                    doc! {
                        "r": date(delta.received_at),
                        "v": last,
                    },
                );
                update.insert("$min", doc! { "lo": min });
                update.insert("$max", doc! { "hi": max });
                update.insert("$inc", doc! { "s0": sum, "c": as_i64(count)? });
            }
            MetricAggregate::Distribution {
                min,
                max,
                sum,
                count,
                bins,
            } => {
                let mut increments = doc! { "s0": sum, "c": as_i64(count)? };
                for (index, quantity) in bins.iter().copied().enumerate() {
                    if quantity > 0 {
                        increments.insert(format!("q.{index}"), i64::from(quantity));
                    }
                }
                update.insert("$min", doc! { "lo": min });
                update.insert("$max", doc! { "hi": max });
                update.insert("$inc", increments);
            }
        }
        self.database
            .collection::<Document>("metric_buckets")
            .update_one(doc! { "_id": binary(bucket_id) }, update)
            .upsert(true)
            .await
            .map_err(|_| SignalStoreError::Unavailable)?;
        Ok(())
    }
}

impl MetricStore for MongoMetricStore {
    fn persist_metrics(
        &self,
        batch: MetricDeltaBatch,
    ) -> PortFuture<'_, Result<DurableOutcome, SignalStoreError>> {
        Box::pin(self.persist_inner(batch))
    }
}

pub(crate) fn metric_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "p", "s", "n", "k", "u", "a", "t", "w", "r", "c"],
            "anyOf": [
                { "required": ["z"] },
                { "required": ["h"] }
            ],
            "properties": {
                "_id": { "bsonType": "binData" },
                "p": { "bsonType": "int" },
                "s": { "bsonType": "binData" },
                "n": { "bsonType": "string" },
                "k": { "bsonType": "int", "minimum": 1, "maximum": 3 },
                "u": { "bsonType": "string" },
                "a": { "bsonType": "object" },
                "t": { "bsonType": "date" },
                "w": { "bsonType": "long" },
                "z": { "bsonType": "date" },
                "h": { "bsonType": "date" },
                "r": { "bsonType": "date" },
                "g": { "bsonType": "binData" },
                "v": { "bsonType": "double" },
                "s0": { "bsonType": "double" },
                "c": { "bsonType": "long" },
                "lo": { "bsonType": "double" },
                "hi": { "bsonType": "double" },
                "q": { "bsonType": "object" },
            }
        }
    }
}

pub(crate) async fn create_metric_indexes(database: &Database) -> mongodb::error::Result<()> {
    let collection = database.collection::<Document>("metric_buckets");
    for model in metric_indexes() {
        collection.create_index(model).await?;
    }
    Ok(())
}

pub(crate) fn metric_index_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "_id_",
        "metric_project_time_name",
        "metric_project_series",
        "metric_retention",
        "metric_archive_due",
    ])
}

fn metric_indexes() -> [IndexModel; 4] {
    [
        named_index(doc! { "p": 1, "t": -1, "n": 1 }, "metric_project_time_name"),
        named_index(doc! { "p": 1, "s": 1 }, "metric_project_series"),
        IndexModel::builder()
            .keys(doc! { "z": 1 })
            .options(
                IndexOptions::builder()
                    .name("metric_retention".to_owned())
                    .expire_after(std::time::Duration::ZERO)
                    .build(),
            )
            .build(),
        IndexModel::builder()
            .keys(doc! { "h": 1, "p": 1, "t": 1, "_id": 1 })
            .options(
                IndexOptions::builder()
                    .name("metric_archive_due".to_owned())
                    .partial_filter_expression(doc! { "h": { "$exists": true } })
                    .build(),
            )
            .build(),
    ]
}

fn named_index(keys: Document, name: &str) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(IndexOptions::builder().name(name.to_owned()).build())
        .build()
}

fn binary(bytes: [u8; 16]) -> Bson {
    Bson::Binary(Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    })
}

fn fixed_binary(value: Bson) -> Result<[u8; 16], SignalStoreError> {
    let Bson::Binary(value) = value else {
        return Err(SignalStoreError::InvalidData);
    };
    value
        .bytes
        .try_into()
        .map_err(|_| SignalStoreError::InvalidData)
}

fn date(value: Timestamp) -> Bson {
    Bson::DateTime(DateTime::from_millis(value.unix_millis()))
}

fn as_i64(value: u64) -> Result<i64, SignalStoreError> {
    i64::try_from(value).map_err(|_| SignalStoreError::InvalidData)
}

const _: usize = METRIC_SKETCH_BINS;
