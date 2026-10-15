use crate::{PostgresDatabase, PostgresError, signal_storage};
use metric_domain::{
    Timestamp,
    metrics::{METRIC_SKETCH_BINS, MetricAggregate, MetricDelta, MetricDeltaBatch, MetricKind},
};
use metric_ports::{DurableOutcome, MetricStore, PortFuture, SignalStoreError};
use sqlx::{Postgres, QueryBuilder, Row};
use std::collections::{BTreeMap, BTreeSet};

const DAY: i64 = 86_400_000;
const MAX_BATCH: usize = 10_000;
const MAX_METADATA: usize = 512 * 1024;
const MAX_BATCH_BYTES: usize = 16 * 1024 * 1024;
const CHUNK: usize = 1000;

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
pub struct PostgresMetricStore {
    database: PostgresDatabase,
    retention: MetricRetention,
}
impl PostgresMetricStore {
    pub fn new(
        database: PostgresDatabase,
        retention: MetricRetention,
    ) -> Result<Self, PostgresError> {
        if !(1..=3650).contains(&retention.days)
            || !(1..=1_000_000).contains(&retention.max_series_per_project)
        {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self {
            database,
            retention,
        })
    }
    async fn persist(&self, batch: MetricDeltaBatch) -> Result<DurableOutcome, SignalStoreError> {
        if batch.is_empty() || batch.len() > MAX_BATCH {
            return Err(SignalStoreError::InvalidData);
        }
        let mut prepared = Vec::<Prepared>::with_capacity(batch.len());
        let mut identities = BTreeMap::<(i32, [u8; 16]), usize>::new();
        let mut requested = BTreeMap::<i32, BTreeSet<[u8; 16]>>::new();
        let mut bytes = 0_usize;
        for (key, delta) in batch.deltas {
            let value = Prepared::new(delta, self.retention.days)?;
            if key
                != (
                    value.delta.series.id(),
                    value.delta.bucket_start.unix_millis(),
                )
            {
                return Err(SignalStoreError::InvalidData);
            }
            bytes = bytes
                .saturating_add(value.tags.len())
                .saturating_add(value.delta.series.name.len())
                .saturating_add(value.delta.series.unit.len())
                .saturating_add(METRIC_SKETCH_BINS * 8 + 200);
            if bytes > MAX_BATCH_BYTES {
                return Err(SignalStoreError::InvalidData);
            }
            let project = value.delta.series.project_id.get();
            let series = value.delta.series.id();
            if let Some(index) = identities.get(&(project, series)) {
                if prepared[*index].delta.series != value.delta.series {
                    return Err(SignalStoreError::Conflict);
                }
            } else {
                identities.insert((project, series), prepared.len());
            }
            requested.entry(project).or_default().insert(series);
            prepared.push(value);
        }
        let projects: Vec<_> = requested.keys().copied().collect();
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        signal_storage::lock_projects(&mut transaction, &projects).await?;
        // Cardinality decisions and updates share project-local coordination,
        // independent of Session/Event/Issue locks, including multi-process writers.
        sqlx::query("INSERT INTO metric.metric_project_locks(project_id) SELECT unnest($1::integer[]) ORDER BY 1 ON CONFLICT(project_id) DO NOTHING").bind(&projects).execute(&mut *transaction).await.map_err(unavailable)?;
        sqlx::query("SELECT project_id FROM metric.metric_project_locks WHERE project_id=ANY($1) ORDER BY project_id FOR UPDATE").bind(&projects).fetch_all(&mut *transaction).await.map_err(unavailable)?;
        for (project, series) in requested {
            let ids: Vec<_> = series.iter().map(|id| id.to_vec()).collect();
            // A capped native DISTINCT/UNION computes the admission count without
            // transferring an unbounded existing-series list into the process.
            let count:i64=sqlx::query_scalar("SELECT count(*) FROM ((SELECT DISTINCT series_id FROM metric.metric_buckets WHERE project_id=$1 LIMIT $3) UNION SELECT unnest($2::bytea[])) admitted")
                .bind(project).bind(&ids).bind((self.retention.max_series_per_project+1) as i64).fetch_one(&mut *transaction).await.map_err(unavailable)?;
            if count > self.retention.max_series_per_project as i64 {
                return Err(SignalStoreError::Capacity);
            }
            let existing=sqlx::query("SELECT requested.series_id,current.name,current.kind,current.unit,current.tags FROM unnest($2::bytea[]) AS requested(series_id) JOIN LATERAL(SELECT name,kind,unit,tags FROM metric.metric_buckets WHERE project_id=$1 AND series_id=requested.series_id LIMIT 1) current ON true")
                .bind(project).bind(&ids).fetch_all(&mut *transaction).await.map_err(unavailable)?;
            for row in existing {
                let id: [u8; 16] = row
                    .try_get::<Vec<u8>, _>("series_id")
                    .map_err(invalid)?
                    .try_into()
                    .map_err(invalid)?;
                let value = &prepared[*identities
                    .get(&(project, id))
                    .ok_or(SignalStoreError::InvalidData)?];
                if row.try_get::<Vec<u8>, _>("name").map_err(invalid)?
                    != value.delta.series.name.as_bytes()
                    || row.try_get::<i16, _>("kind").map_err(invalid)?
                        != value.delta.series.kind.code() as i16
                    || row.try_get::<Vec<u8>, _>("unit").map_err(invalid)?
                        != value.delta.series.unit.as_bytes()
                    || row.try_get::<Vec<u8>, _>("tags").map_err(invalid)? != value.tags
                {
                    return Err(SignalStoreError::Conflict);
                }
            }
        }
        prepared
            .sort_by_key(|value| (value.delta.series.project_id.get(), value.delta.bucket_id()));
        for chunk in prepared.chunks(CHUNK) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO metric.metric_buckets AS current(project_id,bucket_id,series_id,name,kind,unit,tags,bucket_start,bucket_width_seconds,received_at,trace_id,aggregate_sum,measurement_count,last_value,minimum,maximum,bins,expire_at,archive_due) ",
            );
            query.push_values(chunk, |mut row, value| {
                let delta = &value.delta;
                row.push_bind(delta.series.project_id.get())
                    .push_bind(delta.bucket_id().to_vec())
                    .push_bind(delta.series.id().to_vec())
                    .push_bind(delta.series.name.as_bytes())
                    .push_bind(delta.series.kind.code() as i16)
                    .push_bind(delta.series.unit.as_bytes())
                    .push_bind(&value.tags)
                    .push_bind(delta.bucket_start.unix_millis())
                    .push_bind(i64::from(delta.bucket_width_seconds))
                    .push_bind(delta.received_at.unix_millis())
                    .push_bind(delta.trace_id.map(|id| id.as_bytes().to_vec()))
                    .push_bind(value.sum)
                    .push_bind(value.count)
                    .push_bind(value.last)
                    .push_bind(value.minimum)
                    .push_bind(value.maximum)
                    .push_bind(&value.bins)
                    .push_bind(if self.retention.archive {
                        None
                    } else {
                        Some(value.expire)
                    })
                    .push_bind(if self.retention.archive {
                        Some(value.expire)
                    } else {
                        None
                    });
            });
            query.push(" ON CONFLICT(project_id,bucket_id) DO UPDATE SET received_at=EXCLUDED.received_at,aggregate_sum=current.aggregate_sum+EXCLUDED.aggregate_sum,measurement_count=current.measurement_count+EXCLUDED.measurement_count,last_value=EXCLUDED.last_value,minimum=LEAST(current.minimum,EXCLUDED.minimum),maximum=GREATEST(current.maximum,EXCLUDED.maximum),bins=metric.add_metric_bins(current.bins,EXCLUDED.bins) WHERE current.series_id=EXCLUDED.series_id AND current.name=EXCLUDED.name AND current.kind=EXCLUDED.kind AND current.unit=EXCLUDED.unit AND current.tags=EXCLUDED.tags AND current.bucket_start=EXCLUDED.bucket_start AND current.bucket_width_seconds=EXCLUDED.bucket_width_seconds");
            if query
                .build()
                .execute(&mut *transaction)
                .await
                .map_err(write_error)?
                .rows_affected()
                != chunk.len() as u64
            {
                return Err(SignalStoreError::Conflict);
            }
        }
        // Metric deltas retain their documented at-least-once contract. Never
        // retry internally after an unknown COMMIT; the port has no batch ID.
        transaction.commit().await.map_err(unavailable)?;
        Ok(DurableOutcome::Accepted)
    }
}
impl MetricStore for PostgresMetricStore {
    fn persist_metrics(
        &self,
        batch: MetricDeltaBatch,
    ) -> PortFuture<'_, Result<DurableOutcome, SignalStoreError>> {
        Box::pin(self.persist(batch))
    }
}
struct Prepared {
    delta: MetricDelta,
    tags: Vec<u8>,
    sum: f64,
    count: i64,
    last: Option<f64>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    bins: Vec<i64>,
    expire: i64,
}
impl Prepared {
    fn new(delta: MetricDelta, days: u32) -> Result<Self, SignalStoreError> {
        let raw = delta.series.tags.iter().fold(
            delta
                .series
                .name
                .len()
                .saturating_add(delta.series.unit.len())
                .saturating_add(2),
            |bytes, (key, value)| {
                bytes
                    .saturating_add(key.len())
                    .saturating_add(value.len())
                    .saturating_add(6)
            },
        );
        if delta.series.name.is_empty() || raw > MAX_METADATA || delta.bucket_width_seconds == 0 {
            return Err(SignalStoreError::InvalidData);
        }
        let tags = serde_json::to_vec(&delta.series.tags).map_err(invalid)?;
        if tags
            .len()
            .saturating_add(delta.series.name.len())
            .saturating_add(delta.series.unit.len())
            > MAX_METADATA
        {
            return Err(SignalStoreError::InvalidData);
        }
        let mut bins = vec![0_i64; METRIC_SKETCH_BINS];
        let (sum, count, last, minimum, maximum) = match &delta.aggregate {
            MetricAggregate::Counter { sum, count } if delta.series.kind == MetricKind::Counter => {
                (*sum, *count, None, None, None)
            }
            MetricAggregate::Gauge {
                last,
                min,
                max,
                sum,
                count,
            } if delta.series.kind == MetricKind::Gauge => {
                (*sum, *count, Some(*last), Some(*min), Some(*max))
            }
            MetricAggregate::Distribution {
                min,
                max,
                sum,
                count,
                bins: values,
            } if delta.series.kind == MetricKind::Distribution => {
                for (bin, value) in bins.iter_mut().zip(values.iter()) {
                    *bin = i64::from(*value);
                }
                (*sum, *count, None, Some(*min), Some(*max))
            }
            _ => return Err(SignalStoreError::InvalidData),
        };
        if !sum.is_finite()
            || last
                .into_iter()
                .chain(minimum)
                .chain(maximum)
                .any(|value| !value.is_finite())
            || minimum.zip(maximum).is_some_and(|(min, max)| min > max)
            || last
                .zip(minimum.zip(maximum))
                .is_some_and(|(last, (min, max))| last < min || last > max)
        {
            return Err(SignalStoreError::InvalidData);
        }
        let expire = delta
            .bucket_start
            .unix_millis()
            .checked_add(i64::from(days) * DAY)
            .and_then(|value| Timestamp::from_unix_millis(value).ok())
            .ok_or(SignalStoreError::InvalidData)?
            .unix_millis();
        Ok(Self {
            delta,
            tags,
            sum,
            count: i64::try_from(count).map_err(invalid)?,
            last,
            minimum,
            maximum,
            bins,
            expire,
        })
    }
}
fn invalid<T>(_: T) -> SignalStoreError {
    SignalStoreError::InvalidData
}
fn unavailable(_: sqlx::Error) -> SignalStoreError {
    SignalStoreError::Unavailable
}
fn write_error(error: sqlx::Error) -> SignalStoreError {
    match error {
        sqlx::Error::Database(ref db)
            if db
                .code()
                .as_deref()
                .is_some_and(|code| code.starts_with("23") || code == "22003") =>
        {
            SignalStoreError::InvalidData
        }
        _ => SignalStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
