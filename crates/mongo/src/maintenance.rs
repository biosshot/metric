//! Bounded MongoDB operations owned by Phase 14 maintenance tasks.

use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use metric_domain::Timestamp;
use metric_ports::{
    MaintenanceCursor, MaintenanceDisposition, MaintenanceRequest, MaintenanceResult,
    MaintenanceStore, MaintenanceStoreError, MaintenanceTask, PortFuture,
};
use mongodb::{
    Database,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::Hint,
};

const EVENT_ID_BYTES: usize = 20;
const PROJECTION_ID_BYTES: usize = 16;
const PROJECT_CURSOR_BYTES: usize = 5;
const COUNTER_ISSUES_PHASE: u8 = 0;
const COUNTER_PROJECTS_PHASE: u8 = 1;
const MAX_RETENTION: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

#[derive(Clone)]
pub struct MongoMaintenanceStore {
    database: Database,
}

impl MongoMaintenanceStore {
    #[must_use]
    pub const fn from_database(database: Database) -> Self {
        Self { database }
    }

    async fn run_inner(
        &self,
        request: MaintenanceRequest,
    ) -> Result<MaintenanceResult, MaintenanceStoreError> {
        validate_request(&request)?;
        match request.task {
            MaintenanceTask::RetryBacklog => self.observe_due_retry(request).await,
            MaintenanceTask::EventRetention => self.retain_events(request).await,
            MaintenanceTask::HourlyRetention => self.retain_hourly(request).await,
            MaintenanceTask::CounterReconciliation => self.reconcile_counters(request).await,
            MaintenanceTask::UploadExpiry | MaintenanceTask::BlobOrphanRegistration => {
                Ok(MaintenanceResult {
                    scanned: 0,
                    changed: 0,
                    next_cursor: None,
                    disposition: MaintenanceDisposition::Disabled,
                })
            }
        }
    }

    async fn observe_due_retry(
        &self,
        request: MaintenanceRequest,
    ) -> Result<MaintenanceResult, MaintenanceStoreError> {
        if request.cursor.is_some() {
            return Err(MaintenanceStoreError::InvalidData);
        }
        let due = self
            .database
            .collection::<Document>("error_events")
            .find_one(doc! {
                "q.s": 0_i32,
                "q.n": { "$lte": date(request.now) },
            })
            .sort(doc! { "q.n": 1, "r": 1, "_id": 1 })
            .projection(doc! { "_id": 1, "q.n": 1 })
            .hint(Hint::Name("event_pending_due".to_owned()))
            .await
            .map_err(|_| MaintenanceStoreError::Unavailable)?;
        if let Some(document) = &due {
            let next_attempt = document
                .get_document("q")
                .and_then(|state| state.get_datetime("n"))
                .map_err(|_| MaintenanceStoreError::InvalidData)?;
            let lag = request
                .now
                .unix_millis()
                .saturating_sub(next_attempt.timestamp_millis())
                .max(0) as f64
                / 1_000.0;
            metrics::gauge!("metric_maintenance_due_retry_lag_seconds").set(lag);
        } else {
            metrics::gauge!("metric_maintenance_due_retry_lag_seconds").set(0.0);
        }
        Ok(MaintenanceResult {
            scanned: usize::from(due.is_some()),
            changed: 0,
            next_cursor: None,
            disposition: MaintenanceDisposition::Completed,
        })
    }

    async fn retain_events(
        &self,
        request: MaintenanceRequest,
    ) -> Result<MaintenanceResult, MaintenanceStoreError> {
        let after = decode_binary_cursor(request.cursor.as_ref(), EVENT_ID_BYTES)?;
        let mut filter = Document::new();
        if let Some(after) = after {
            filter.insert("_id", doc! { "$gt": Bson::Binary(binary(&after)) });
        }
        let events = self.database.collection::<Document>("error_events");
        let mut cursor = events
            .find(filter)
            .sort(doc! { "_id": 1 })
            .projection(doc! { "_id": 1, "r": 1, "q": 1, "x": 1, "h": 1, "z": 1 })
            .hint(Hint::Name("_id_".to_owned()))
            .limit(limit(request.batch_size)?)
            .await
            .map_err(|_| MaintenanceStoreError::Unavailable)?;
        let mut scanned = 0_usize;
        let mut changed = 0_usize;
        let mut last = None;
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| MaintenanceStoreError::Unavailable)?
        {
            scanned += 1;
            let id = fixed_binary::<EVENT_ID_BYTES>(&document, "_id")?;
            last = Some(id.to_vec());
            let received = timestamp(&document, "r")?;
            let desired = checked_add(received, request.event_retention)?;
            match document.get("q") {
                None => {
                    let awaiting_archive = request.archive_events && !document.contains_key("z");
                    let field = if awaiting_archive { "h" } else { "x" };
                    let current = optional_timestamp(&document, field)?;
                    let opposite_present = if awaiting_archive {
                        document.contains_key("x")
                    } else {
                        document.contains_key("h")
                    };
                    if current != Some(desired) || opposite_present {
                        let update = if awaiting_archive {
                            doc! {
                                "$set": { "h": date(desired) },
                                "$unset": { "x": "" },
                            }
                        } else {
                            doc! {
                                "$set": { "x": date(desired) },
                                "$unset": { "h": "" },
                            }
                        };
                        let result = events
                            .update_one(
                                doc! {
                                    "_id": Bson::Binary(binary(&id)),
                                    "q": { "$exists": false },
                                },
                                update,
                            )
                            .await
                            .map_err(|_| MaintenanceStoreError::Unavailable)?;
                        changed += usize::try_from(result.modified_count)
                            .map_err(|_| MaintenanceStoreError::InvalidData)?;
                    }
                }
                Some(Bson::Document(state)) if state.get_i32("s") == Ok(0) => {
                    if document.contains_key("x")
                        || document.contains_key("h")
                        || document.contains_key("z")
                    {
                        return Err(MaintenanceStoreError::InvalidData);
                    }
                }
                Some(Bson::Document(state)) if state.get_i32("s") == Ok(1) => {
                    if request.archive_events && !document.contains_key("z") {
                        if optional_timestamp(&document, "h")? != Some(desired)
                            || document.contains_key("x")
                        {
                            let result = events
                                .update_one(
                                    doc! {
                                        "_id": Bson::Binary(binary(&id)),
                                        "q.s": 1_i32,
                                        "z": { "$exists": false },
                                    },
                                    doc! {
                                        "$set": { "h": date(desired) },
                                        "$unset": { "x": "" },
                                    },
                                )
                                .await
                                .map_err(|_| MaintenanceStoreError::Unavailable)?;
                            changed += usize::try_from(result.modified_count)
                                .map_err(|_| MaintenanceStoreError::InvalidData)?;
                        }
                    } else if document.contains_key("z") {
                        if optional_timestamp(&document, "x")? != Some(desired) {
                            let result = events
                                .update_one(
                                    doc! {
                                        "_id": Bson::Binary(binary(&id)),
                                        "q.s": 1_i32,
                                        "z": { "$exists": true },
                                    },
                                    doc! {
                                        "$set": { "x": date(desired) },
                                        "$unset": { "h": "" },
                                    },
                                )
                                .await
                                .map_err(|_| MaintenanceStoreError::Unavailable)?;
                            changed += usize::try_from(result.modified_count)
                                .map_err(|_| MaintenanceStoreError::InvalidData)?;
                        }
                    } else if desired <= request.now {
                        let result = events
                            .delete_one(doc! {
                                "_id": Bson::Binary(binary(&id)),
                                "q.s": 1_i32,
                                "r": { "$lte": date(
                                    checked_sub(request.now, request.event_retention)?
                                ) },
                            })
                            .await
                            .map_err(|_| MaintenanceStoreError::Unavailable)?;
                        changed += usize::try_from(result.deleted_count)
                            .map_err(|_| MaintenanceStoreError::InvalidData)?;
                    }
                }
                _ => return Err(MaintenanceStoreError::InvalidData),
            }
        }
        batch_result(scanned, changed, request.batch_size, last)
    }

    async fn retain_hourly(
        &self,
        request: MaintenanceRequest,
    ) -> Result<MaintenanceResult, MaintenanceStoreError> {
        let after = decode_binary_cursor(request.cursor.as_ref(), PROJECTION_ID_BYTES)?;
        let mut filter = Document::new();
        if let Some(after) = after {
            filter.insert("_id", doc! { "$gt": Bson::Binary(binary(&after)) });
        }
        let buckets = self.database.collection::<Document>("issue_stats_hourly");
        let mut cursor = buckets
            .find(filter)
            .sort(doc! { "_id": 1 })
            .projection(doc! { "_id": 1, "bucket_start": 1, "expire_at": 1 })
            .hint(Hint::Name("_id_".to_owned()))
            .limit(limit(request.batch_size)?)
            .await
            .map_err(|_| MaintenanceStoreError::Unavailable)?;
        let mut scanned = 0_usize;
        let mut changed = 0_usize;
        let mut last = None;
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| MaintenanceStoreError::Unavailable)?
        {
            scanned += 1;
            let id = fixed_binary::<PROJECTION_ID_BYTES>(&document, "_id")?;
            last = Some(id.to_vec());
            let desired = checked_add(
                timestamp(&document, "bucket_start")?,
                request.hourly_retention,
            )?;
            if timestamp(&document, "expire_at")? != desired {
                let result = buckets
                    .update_one(
                        doc! { "_id": Bson::Binary(binary(&id)) },
                        doc! { "$set": { "expire_at": date(desired) } },
                    )
                    .await
                    .map_err(|_| MaintenanceStoreError::Unavailable)?;
                changed += usize::try_from(result.modified_count)
                    .map_err(|_| MaintenanceStoreError::InvalidData)?;
            }
        }
        batch_result(scanned, changed, request.batch_size, last)
    }

    async fn reconcile_counters(
        &self,
        request: MaintenanceRequest,
    ) -> Result<MaintenanceResult, MaintenanceStoreError> {
        let (phase, after) = decode_counter_cursor(request.cursor.as_ref())?;
        match phase {
            COUNTER_ISSUES_PHASE => self.reconcile_issue_counts(request, after).await,
            COUNTER_PROJECTS_PHASE => self.reconcile_environment_quota(request, after).await,
            _ => Err(MaintenanceStoreError::InvalidData),
        }
    }

    async fn reconcile_issue_counts(
        &self,
        request: MaintenanceRequest,
        after: Option<Vec<u8>>,
    ) -> Result<MaintenanceResult, MaintenanceStoreError> {
        let mut filter = Document::new();
        if let Some(after) = after {
            if after.len() != PROJECTION_ID_BYTES {
                return Err(MaintenanceStoreError::InvalidData);
            }
            filter.insert("_id", doc! { "$gt": Bson::Binary(binary(&after)) });
        }
        let issue_limit = request.batch_size.min(32);
        let issues = self.database.collection::<Document>("issues");
        let events = self.database.collection::<Document>("error_events");
        let mut cursor = issues
            .find(filter)
            .sort(doc! { "_id": 1 })
            .projection(doc! { "_id": 1, "p": 1, "c": 1 })
            .hint(Hint::Name("_id_".to_owned()))
            .limit(limit(issue_limit)?)
            .await
            .map_err(|_| MaintenanceStoreError::Unavailable)?;
        let mut scanned = 0_usize;
        let mut changed = 0_usize;
        let mut last = None;
        while let Some(issue) = cursor
            .try_next()
            .await
            .map_err(|_| MaintenanceStoreError::Unavailable)?
        {
            scanned += 1;
            let id = fixed_binary::<PROJECTION_ID_BYTES>(&issue, "_id")?;
            last = Some(id.to_vec());
            let project = issue
                .get_i32("p")
                .map_err(|_| MaintenanceStoreError::InvalidData)?;
            let current = issue
                .get_i64("c")
                .map_err(|_| MaintenanceStoreError::InvalidData)?;
            let mut retained = events
                .find(doc! {
                    "p": project,
                    "u": Bson::Binary(binary(&id)),
                })
                .projection(doc! { "_id": 1 })
                .hint(Hint::Name("event_issue_timeline".to_owned()))
                .limit(limit(request.batch_size)?)
                .await
                .map_err(|_| MaintenanceStoreError::Unavailable)?;
            let mut lower_bound = 0_i64;
            while retained
                .try_next()
                .await
                .map_err(|_| MaintenanceStoreError::Unavailable)?
                .is_some()
            {
                lower_bound = lower_bound.saturating_add(1);
            }
            if lower_bound > current {
                let result = issues
                    .update_one(
                        doc! { "_id": Bson::Binary(binary(&id)), "p": project },
                        doc! { "$max": { "c": lower_bound } },
                    )
                    .await
                    .map_err(|_| MaintenanceStoreError::Unavailable)?;
                changed += usize::try_from(result.modified_count)
                    .map_err(|_| MaintenanceStoreError::InvalidData)?;
            }
        }
        let next_cursor = if scanned == issue_limit {
            last.map(|id| counter_cursor(COUNTER_ISSUES_PHASE, &id))
                .transpose()?
        } else {
            MaintenanceCursor::new([COUNTER_PROJECTS_PHASE].to_vec())
        };
        Ok(MaintenanceResult {
            scanned,
            changed,
            next_cursor,
            disposition: MaintenanceDisposition::Completed,
        })
    }

    async fn reconcile_environment_quota(
        &self,
        request: MaintenanceRequest,
        after: Option<Vec<u8>>,
    ) -> Result<MaintenanceResult, MaintenanceStoreError> {
        let mut filter = Document::new();
        if let Some(after) = after {
            let bytes: [u8; 4] = after
                .try_into()
                .map_err(|_| MaintenanceStoreError::InvalidData)?;
            let project = i32::from_be_bytes(bytes);
            filter.insert("_id", doc! { "$gt": project });
        }
        let projects = self.database.collection::<Document>("projects");
        let environments = self.database.collection::<Document>("environments");
        let mut cursor = projects
            .find(filter)
            .sort(doc! { "_id": 1 })
            .projection(doc! { "_id": 1, "catalog_usage.ec": 1 })
            .hint(Hint::Name("_id_".to_owned()))
            .limit(limit(request.batch_size)?)
            .await
            .map_err(|_| MaintenanceStoreError::Unavailable)?;
        let mut scanned = 0_usize;
        let mut changed = 0_usize;
        let mut last = None;
        while let Some(project) = cursor
            .try_next()
            .await
            .map_err(|_| MaintenanceStoreError::Unavailable)?
        {
            scanned += 1;
            let project_id = project
                .get_i32("_id")
                .map_err(|_| MaintenanceStoreError::InvalidData)?;
            last = Some(project_id);
            let current = project
                .get_document("catalog_usage")
                .ok()
                .and_then(|usage| usage.get_i32("ec").ok())
                .unwrap_or(0);
            let mut stored = environments
                .find(doc! { "project_id": project_id })
                .projection(doc! { "_id": 1 })
                .hint(Hint::Name("environment_project_timeline".to_owned()))
                .limit(limit(request.batch_size)?)
                .await
                .map_err(|_| MaintenanceStoreError::Unavailable)?;
            let mut actual = 0_i32;
            while stored
                .try_next()
                .await
                .map_err(|_| MaintenanceStoreError::Unavailable)?
                .is_some()
            {
                actual = actual.saturating_add(1);
            }
            if actual != current {
                let update = if actual == 0 {
                    doc! { "$unset": { "catalog_usage.ec": "" } }
                } else {
                    doc! { "$set": { "catalog_usage.ec": actual } }
                };
                let result = projects
                    .update_one(doc! { "_id": project_id }, update)
                    .await
                    .map_err(|_| MaintenanceStoreError::Unavailable)?;
                changed += usize::try_from(result.modified_count)
                    .map_err(|_| MaintenanceStoreError::InvalidData)?;
            }
        }
        let next_cursor = if scanned == request.batch_size {
            last.map(|id| {
                let mut bytes = Vec::with_capacity(PROJECT_CURSOR_BYTES);
                bytes.push(COUNTER_PROJECTS_PHASE);
                bytes.extend_from_slice(&id.to_be_bytes());
                MaintenanceCursor::new(bytes).ok_or(MaintenanceStoreError::InvalidData)
            })
            .transpose()?
        } else {
            None
        };
        Ok(MaintenanceResult {
            scanned,
            changed,
            next_cursor,
            disposition: MaintenanceDisposition::Completed,
        })
    }
}

impl MaintenanceStore for MongoMaintenanceStore {
    fn run(
        &self,
        request: MaintenanceRequest,
    ) -> PortFuture<'_, Result<MaintenanceResult, MaintenanceStoreError>> {
        Box::pin(async move {
            let task = request.task;
            let started = Instant::now();
            let result = self.run_inner(request).await;
            let outcome = match result {
                Ok(_) => "ok",
                Err(MaintenanceStoreError::InvalidData) => "invalid_data",
                Err(MaintenanceStoreError::Unavailable) => "unavailable",
            };
            metrics::histogram!(
                "metric_mongodb_operation_duration_seconds",
                "operation" => task.name(),
                "outcome" => outcome
            )
            .record(started.elapsed().as_secs_f64());
            result
        })
    }
}

fn validate_request(request: &MaintenanceRequest) -> Result<(), MaintenanceStoreError> {
    let valid = (1..=10_000).contains(&request.batch_size)
        && !request.event_retention.is_zero()
        && request.event_retention <= MAX_RETENTION
        && !request.hourly_retention.is_zero()
        && request.hourly_retention <= MAX_RETENTION;
    valid
        .then_some(())
        .ok_or(MaintenanceStoreError::InvalidData)
}

fn batch_result(
    scanned: usize,
    changed: usize,
    batch_size: usize,
    last: Option<Vec<u8>>,
) -> Result<MaintenanceResult, MaintenanceStoreError> {
    let next_cursor = if scanned == batch_size {
        last.map(|bytes| MaintenanceCursor::new(bytes).ok_or(MaintenanceStoreError::InvalidData))
            .transpose()?
    } else {
        None
    };
    Ok(MaintenanceResult {
        scanned,
        changed,
        next_cursor,
        disposition: MaintenanceDisposition::Completed,
    })
}

fn decode_binary_cursor(
    cursor: Option<&MaintenanceCursor>,
    expected: usize,
) -> Result<Option<Vec<u8>>, MaintenanceStoreError> {
    cursor
        .map(|cursor| {
            (cursor.as_bytes().len() == expected)
                .then(|| cursor.as_bytes().to_vec())
                .ok_or(MaintenanceStoreError::InvalidData)
        })
        .transpose()
}

fn decode_counter_cursor(
    cursor: Option<&MaintenanceCursor>,
) -> Result<(u8, Option<Vec<u8>>), MaintenanceStoreError> {
    let Some(cursor) = cursor else {
        return Ok((COUNTER_ISSUES_PHASE, None));
    };
    let (phase, after) = cursor
        .as_bytes()
        .split_first()
        .ok_or(MaintenanceStoreError::InvalidData)?;
    match (*phase, after.len()) {
        (COUNTER_ISSUES_PHASE, PROJECTION_ID_BYTES) => Ok((*phase, Some(after.to_vec()))),
        (COUNTER_PROJECTS_PHASE, 0) => Ok((*phase, None)),
        (COUNTER_PROJECTS_PHASE, 4) => Ok((*phase, Some(after.to_vec()))),
        _ => Err(MaintenanceStoreError::InvalidData),
    }
}

fn counter_cursor(phase: u8, after: &[u8]) -> Result<MaintenanceCursor, MaintenanceStoreError> {
    let mut bytes = Vec::with_capacity(after.len() + 1);
    bytes.push(phase);
    bytes.extend_from_slice(after);
    MaintenanceCursor::new(bytes).ok_or(MaintenanceStoreError::InvalidData)
}

fn fixed_binary<const N: usize>(
    document: &Document,
    field: &str,
) -> Result<[u8; N], MaintenanceStoreError> {
    document
        .get_binary_generic(field)
        .map_err(|_| MaintenanceStoreError::InvalidData)?
        .as_slice()
        .try_into()
        .map_err(|_| MaintenanceStoreError::InvalidData)
}

fn timestamp(document: &Document, field: &str) -> Result<Timestamp, MaintenanceStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(field)
            .map_err(|_| MaintenanceStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| MaintenanceStoreError::InvalidData)
}

fn optional_timestamp(
    document: &Document,
    field: &str,
) -> Result<Option<Timestamp>, MaintenanceStoreError> {
    match document.get(field) {
        None => Ok(None),
        Some(Bson::DateTime(value)) => Timestamp::from_unix_millis(value.timestamp_millis())
            .map(Some)
            .map_err(|_| MaintenanceStoreError::InvalidData),
        Some(_) => Err(MaintenanceStoreError::InvalidData),
    }
}

fn checked_add(
    timestamp: Timestamp,
    duration: Duration,
) -> Result<Timestamp, MaintenanceStoreError> {
    let millis =
        i64::try_from(duration.as_millis()).map_err(|_| MaintenanceStoreError::InvalidData)?;
    Timestamp::from_unix_millis(
        timestamp
            .unix_millis()
            .checked_add(millis)
            .ok_or(MaintenanceStoreError::InvalidData)?,
    )
    .map_err(|_| MaintenanceStoreError::InvalidData)
}

fn checked_sub(
    timestamp: Timestamp,
    duration: Duration,
) -> Result<Timestamp, MaintenanceStoreError> {
    let millis =
        i64::try_from(duration.as_millis()).map_err(|_| MaintenanceStoreError::InvalidData)?;
    Timestamp::from_unix_millis(
        timestamp
            .unix_millis()
            .checked_sub(millis)
            .ok_or(MaintenanceStoreError::InvalidData)?,
    )
    .map_err(|_| MaintenanceStoreError::InvalidData)
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn binary(bytes: &[u8]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn limit(value: usize) -> Result<i64, MaintenanceStoreError> {
    i64::try_from(value).map_err(|_| MaintenanceStoreError::InvalidData)
}
