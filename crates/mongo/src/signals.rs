use futures_util::TryStreamExt;
use metric_domain::{
    EventKey, ProjectId, Timestamp,
    signals::{
        LogId, LogRecord, LogSeverity, PerformanceBucket, SignalBody, SignalCursor, SignalPage,
        SpanId, SpanOperationClass, SpanRecord, SpanRecordId, TraceId, TraceView,
    },
};
use metric_ports::{
    DurableOutcome, LogQuery, PerformanceQuery, PortFuture, SegmentQuery, SignalStore,
    SignalStoreError,
};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    error::ErrorKind,
    options::IndexOptions,
};

const DUPLICATE_KEY_CODE: i32 = 11000;
const MAX_QUERY_LIMIT: usize = 200;
const MAX_STATS_SAMPLES: i32 = 2_048;

#[derive(Debug, Clone, Copy)]
enum SignalWriteStatus {
    Inserted,
    Duplicate,
    Rejected,
}

fn classify_signal_insert_many(
    error: &ErrorKind,
    count: usize,
) -> Result<Vec<SignalWriteStatus>, SignalStoreError> {
    let ErrorKind::InsertMany(failure) = error else {
        return Err(SignalStoreError::Unavailable);
    };
    if failure.write_concern_error.is_some() {
        return Err(SignalStoreError::Unavailable);
    }
    let errors = failure
        .write_errors
        .as_ref()
        .ok_or(SignalStoreError::Unavailable)?;
    let mut statuses = vec![SignalWriteStatus::Inserted; count];
    for error in errors {
        let status = statuses
            .get_mut(error.index)
            .ok_or(SignalStoreError::Unavailable)?;
        *status = if error.code == DUPLICATE_KEY_CODE {
            SignalWriteStatus::Duplicate
        } else {
            SignalWriteStatus::Rejected
        };
    }
    Ok(statuses)
}

#[derive(Debug, Clone, Copy)]
pub struct SignalRetention {
    pub logs_days: u32,
    pub spans_days: u32,
    pub span_stats_hourly_days: u32,
    pub archive: bool,
}

impl Default for SignalRetention {
    fn default() -> Self {
        Self {
            logs_days: 30,
            spans_days: 30,
            span_stats_hourly_days: 90,
            archive: false,
        }
    }
}

#[derive(Clone)]
pub struct MongoSignalStore {
    database: Database,
    retention: SignalRetention,
}

impl MongoSignalStore {
    #[must_use]
    pub const fn from_database(database: Database) -> Self {
        Self {
            database,
            retention: SignalRetention {
                logs_days: 30,
                spans_days: 30,
                span_stats_hourly_days: 90,
                archive: false,
            },
        }
    }

    #[must_use]
    pub const fn with_retention(database: Database, retention: SignalRetention) -> Self {
        Self {
            database,
            retention,
        }
    }

    async fn verify_log_duplicate(&self, record: &LogRecord) -> Result<(), SignalStoreError> {
        let existing = self
            .database
            .collection::<Document>("logs")
            .find_one(doc! { "_id": binary(record.id.as_bytes()) })
            .projection(doc! { "p": 1, "o": 1, "m": 1 })
            .await
            .map_err(unavailable)?
            .ok_or(SignalStoreError::Conflict)?;
        if existing.get_i32("p") == Ok(record.project_id.get())
            && existing.get_i64("o") == Ok(record.occurred_at_ns)
            && existing.get_str("m") == Ok(record.message.as_ref())
        {
            Ok(())
        } else {
            Err(SignalStoreError::Conflict)
        }
    }

    async fn verify_span_duplicate(&self, record: &SpanRecord) -> Result<(), SignalStoreError> {
        let existing = self
            .database
            .collection::<Document>("spans")
            .find_one(doc! { "_id": binary(record.id.as_bytes()) })
            .projection(doc! { "p": 1, "g": 1, "n": 1 })
            .await
            .map_err(unavailable)?
            .ok_or(SignalStoreError::Conflict)?;
        if existing.get_i32("p") == Ok(record.project_id.get())
            && fixed_binary::<16>(&existing, "g") == Ok(record.trace_id.as_bytes())
            && fixed_binary::<8>(&existing, "n") == Ok(record.span_id.as_bytes())
        {
            Ok(())
        } else {
            Err(SignalStoreError::Conflict)
        }
    }

    async fn apply_stat(&self, record: &SpanRecord) -> Result<(), SignalStoreError> {
        let hour_millis = (record.started_at_ns / 1_000_000).div_euclid(3_600_000) * 3_600_000;
        let hour =
            Timestamp::from_unix_millis(hour_millis).map_err(|_| SignalStoreError::InvalidData)?;
        let id = stat_id(
            record.project_id,
            hour,
            &record.name,
            record.service.as_deref(),
            record.environment.as_deref(),
            record.release.as_deref(),
            record.operation_class,
        );
        let failure = i64::from(!matches!(record.status.as_ref(), "" | "ok" | "cancelled"));
        let mut set_on_insert = doc! {
            "p": record.project_id.get(),
            "h": date(hour),
            "t": 1_i32,
            "k": record.name.as_ref(),
            "c": record.operation_class.code(),
            "g": binary(record.trace_id.as_bytes()),
            "x": date(Timestamp::from_unix_millis(
                hour_millis.saturating_add(
                    i64::from(self.retention.span_stats_hourly_days) * 86_400_000
                )
            ).map_err(|_| SignalStoreError::InvalidData)?),
        };
        if let Some(service) = &record.service {
            set_on_insert.insert("v", service.as_ref());
        }
        if let Some(environment) = &record.environment {
            set_on_insert.insert("e", environment.as_ref());
        }
        if let Some(release) = &record.release {
            set_on_insert.insert("u", release.as_ref());
        }
        self.database
            .collection::<Document>("span_stats_hourly")
            .update_one(
                doc! { "_id": binary(id) },
                doc! {
                    "$setOnInsert": set_on_insert,
                    "$inc": {
                        "n": 1_i64,
                        "f": failure,
                        "s": record.duration_ns,
                    },
                    "$push": {
                        "d": {
                            "$each": [record.duration_ns],
                            "$slice": -MAX_STATS_SAMPLES,
                        }
                    }
                },
            )
            .upsert(true)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn list_logs_inner(
        &self,
        project_id: ProjectId,
        query: LogQuery,
    ) -> Result<SignalPage<LogRecord>, SignalStoreError> {
        validate_query(query.from_ns, query.until_ns, query.limit)?;
        let mut filter = doc! {
            "p": project_id.get(),
            "o": { "$gte": query.from_ns, "$lt": query.until_ns },
        };
        if let Some(severity) = query.severity {
            filter.insert("l", severity.code());
        }
        if let Some(message) = query.message {
            filter.insert(
                "m",
                doc! { "$regex": regex_contains(&message), "$options": "i" },
            );
        }
        optional_exact(&mut filter, "e", query.environment);
        optional_exact(&mut filter, "v", query.release);
        optional_exact(&mut filter, "j", query.service);
        if let Some(trace_id) = query.trace_id {
            filter.insert("g", binary(trace_id.as_bytes()));
        }
        append_signal_anchor(&mut filter, query.before);
        let mut cursor = self
            .database
            .collection::<Document>("logs")
            .find(filter)
            .sort(doc! { "o": -1, "_id": -1 })
            .limit(i64::try_from(query.limit.saturating_add(1)).unwrap_or(201))
            .await
            .map_err(unavailable)?;
        let mut items = Vec::with_capacity(query.limit.saturating_add(1));
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            items.push(decode_log(&document)?);
        }
        let has_more = items.len() > query.limit;
        items.truncate(query.limit);
        let next = has_more.then(|| {
            let item = items.last().expect("a page with more data has an item");
            SignalCursor {
                time_ns: item.occurred_at_ns,
                id: item.id.as_bytes(),
            }
        });
        Ok(SignalPage { items, next })
    }

    async fn list_segments_inner(
        &self,
        project_id: ProjectId,
        query: SegmentQuery,
    ) -> Result<SignalPage<SpanRecord>, SignalStoreError> {
        validate_query(query.from_ns, query.until_ns, query.limit)?;
        let mut filter = doc! {
            "p": project_id.get(),
            "t": true,
            "o": { "$gte": query.from_ns, "$lt": query.until_ns },
        };
        optional_exact(&mut filter, "e", query.environment);
        optional_exact(&mut filter, "v", query.release);
        optional_exact(&mut filter, "j", query.service);
        append_signal_anchor(&mut filter, query.before);
        let mut cursor = self
            .database
            .collection::<Document>("spans")
            .find(filter)
            .sort(doc! { "o": -1, "_id": -1 })
            .limit(i64::try_from(query.limit.saturating_add(1)).unwrap_or(201))
            .await
            .map_err(unavailable)?;
        let mut items = Vec::with_capacity(query.limit.saturating_add(1));
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            items.push(decode_span(&document)?);
        }
        let has_more = items.len() > query.limit;
        items.truncate(query.limit);
        let next = has_more.then(|| {
            let item = items.last().expect("a page with more data has an item");
            SignalCursor {
                time_ns: item.started_at_ns,
                id: item.id.as_bytes(),
            }
        });
        Ok(SignalPage { items, next })
    }

    async fn trace_inner(
        &self,
        project_ids: Vec<ProjectId>,
        trace_id: TraceId,
        maximum_spans: usize,
        maximum_logs: usize,
    ) -> Result<TraceView, SignalStoreError> {
        if project_ids.is_empty()
            || project_ids.len() > 32
            || maximum_spans == 0
            || maximum_spans > 2_000
            || maximum_logs > 1_000
        {
            return Err(SignalStoreError::InvalidData);
        }
        let projects = project_ids
            .iter()
            .map(|project| Bson::Int32(project.get()))
            .collect::<Vec<_>>();
        let mut cursor = self
            .database
            .collection::<Document>("spans")
            .find(doc! {
                "p": { "$in": &projects },
                "g": binary(trace_id.as_bytes()),
            })
            .sort(doc! { "o": 1, "n": 1 })
            .limit(i64::try_from(maximum_spans.saturating_add(1)).unwrap_or(2_001))
            .await
            .map_err(unavailable)?;
        let mut spans = Vec::with_capacity(maximum_spans.saturating_add(1));
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            spans.push(decode_span(&document)?);
        }
        let partial = spans.len() > maximum_spans;
        let omitted_spans =
            u32::try_from(spans.len().saturating_sub(maximum_spans)).unwrap_or(u32::MAX);
        spans.truncate(maximum_spans);

        let mut logs = Vec::new();
        let mut logs_partial = false;
        if maximum_logs > 0 {
            let mut cursor = self
                .database
                .collection::<Document>("logs")
                .find(doc! {
                    "p": { "$in": projects },
                    "g": binary(trace_id.as_bytes()),
                })
                .sort(doc! { "o": 1, "_id": 1 })
                .limit(i64::try_from(maximum_logs.saturating_add(1)).unwrap_or(1_001))
                .await
                .map_err(unavailable)?;
            while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
                logs.push(decode_log(&document)?);
            }
            logs_partial = logs.len() > maximum_logs;
            logs.truncate(maximum_logs);
        }
        let mut errors = Vec::new();
        let mut cursor = self
            .database
            .collection::<Document>("error_events")
            .find(doc! {
                "p": { "$in": project_ids.iter().map(|project| project.get()).collect::<Vec<_>>() },
                "g": binary(trace_id.as_bytes()),
                "q": { "$exists": false },
            })
            .sort(doc! { "o": 1, "_id": 1 })
            .limit(100)
            .await
            .map_err(unavailable)?;
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let key = EventKey::from_bytes(fixed_binary(&document, "_id")?)
                .map_err(|_| SignalStoreError::InvalidData)?;
            errors.push(key.event_id());
        }
        if spans.is_empty() && logs.is_empty() && errors.is_empty() {
            return Err(SignalStoreError::NotFound);
        }
        Ok(TraceView {
            trace_id,
            spans,
            logs,
            errors,
            partial: partial || logs_partial,
            omitted_spans,
        })
    }

    async fn performance_inner(
        &self,
        project_id: ProjectId,
        query: PerformanceQuery,
    ) -> Result<Vec<PerformanceBucket>, SignalStoreError> {
        if query.from >= query.until || query.limit == 0 || query.limit > MAX_QUERY_LIMIT {
            return Err(SignalStoreError::InvalidData);
        }
        let mut filter = doc! {
            "p": project_id.get(),
            "h": { "$gte": date(query.from), "$lt": date(query.until) },
        };
        optional_exact(&mut filter, "v", query.service);
        optional_exact(&mut filter, "e", query.environment);
        optional_exact(&mut filter, "u", query.release);
        let mut cursor = self
            .database
            .collection::<Document>("span_stats_hourly")
            .find(filter)
            .sort(doc! { "h": -1, "n": -1 })
            .limit(i64::try_from(query.limit).unwrap_or(200))
            .await
            .map_err(unavailable)?;
        let mut result = Vec::with_capacity(query.limit);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            result.push(decode_stat(&document)?);
        }
        Ok(result)
    }

    async fn rebuild_inner(
        &self,
        project_id: ProjectId,
        from: Timestamp,
        until: Timestamp,
    ) -> Result<u64, SignalStoreError> {
        if from >= until {
            return Err(SignalStoreError::InvalidData);
        }
        self.database
            .collection::<Document>("span_stats_hourly")
            .delete_many(doc! {
                "p": project_id.get(),
                "h": { "$gte": date(from), "$lt": date(until) },
            })
            .await
            .map_err(unavailable)?;
        let from_ns = from
            .unix_millis()
            .checked_mul(1_000_000)
            .ok_or(SignalStoreError::InvalidData)?;
        let until_ns = until
            .unix_millis()
            .checked_mul(1_000_000)
            .ok_or(SignalStoreError::InvalidData)?;
        let mut cursor = self
            .database
            .collection::<Document>("spans")
            .find(doc! {
                "p": project_id.get(),
                "t": true,
                "o": { "$gte": from_ns, "$lt": until_ns },
            })
            .sort(doc! { "o": 1, "_id": 1 })
            .await
            .map_err(unavailable)?;
        let mut count = 0_u64;
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            self.apply_stat(&decode_span(&document)?).await?;
            count = count.saturating_add(1);
        }
        Ok(count)
    }
}

impl SignalStore for MongoSignalStore {
    fn persist_logs(
        &self,
        records: Vec<LogRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(async move {
            if records.is_empty() {
                return Ok(Vec::new());
            }
            let documents = records
                .iter()
                .map(|record| encode_log(record, self.retention.logs_days, self.retention.archive))
                .collect::<Result<Vec<_>, _>>()?;
            let started = std::time::Instant::now();
            let result = self
                .database
                .collection::<Document>("logs")
                .insert_many(documents.iter())
                .ordered(false)
                .await;
            let statuses = match result {
                Ok(_) => vec![SignalWriteStatus::Inserted; records.len()],
                Err(error) => classify_signal_insert_many(error.kind.as_ref(), records.len())?,
            };
            let mut outcomes = Vec::with_capacity(records.len());
            for (record, status) in records.iter().zip(statuses) {
                match status {
                    SignalWriteStatus::Inserted => outcomes.push(DurableOutcome::Accepted),
                    SignalWriteStatus::Duplicate => {
                        self.verify_log_duplicate(record).await?;
                        outcomes.push(DurableOutcome::Duplicate);
                    }
                    SignalWriteStatus::Rejected => return Err(SignalStoreError::Unavailable),
                }
            }
            metrics::histogram!(
                "metric_mongodb_operation_duration_seconds",
                "operation" => "log_insert_batch",
                "outcome" => "durable"
            )
            .record(started.elapsed().as_secs_f64());
            Ok(outcomes)
        })
    }

    fn persist_spans(
        &self,
        records: Vec<SpanRecord>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(async move {
            if records.is_empty() {
                return Ok(Vec::new());
            }
            let documents = records
                .iter()
                .map(|record| {
                    encode_span(record, self.retention.spans_days, self.retention.archive)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let started = std::time::Instant::now();
            let result = self
                .database
                .collection::<Document>("spans")
                .insert_many(documents.iter())
                .ordered(false)
                .await;
            let statuses = match result {
                Ok(_) => vec![SignalWriteStatus::Inserted; records.len()],
                Err(error) => classify_signal_insert_many(error.kind.as_ref(), records.len())?,
            };
            let mut outcomes = Vec::with_capacity(records.len());
            for (record, status) in records.iter().zip(statuses) {
                match status {
                    SignalWriteStatus::Inserted => {
                        outcomes.push(DurableOutcome::Accepted);
                        if record.is_segment {
                            // Aggregates are rebuildable derived state. A durable Span remains
                            // accepted even if the best-effort rollup is temporarily unavailable.
                            let _ = self.apply_stat(record).await;
                        }
                    }
                    SignalWriteStatus::Duplicate => {
                        self.verify_span_duplicate(record).await?;
                        outcomes.push(DurableOutcome::Duplicate);
                    }
                    SignalWriteStatus::Rejected => return Err(SignalStoreError::Unavailable),
                }
            }
            metrics::histogram!(
                "metric_mongodb_operation_duration_seconds",
                "operation" => "span_insert_batch",
                "outcome" => "durable"
            )
            .record(started.elapsed().as_secs_f64());
            Ok(outcomes)
        })
    }

    fn list_logs(
        &self,
        project_id: ProjectId,
        query: LogQuery,
    ) -> PortFuture<'_, Result<SignalPage<LogRecord>, SignalStoreError>> {
        Box::pin(self.list_logs_inner(project_id, query))
    }

    fn load_log(
        &self,
        project_id: ProjectId,
        log_id: LogId,
    ) -> PortFuture<'_, Result<LogRecord, SignalStoreError>> {
        Box::pin(async move {
            let document = self
                .database
                .collection::<Document>("logs")
                .find_one(doc! {
                    "_id": binary(log_id.as_bytes()),
                    "p": project_id.get(),
                })
                .await
                .map_err(unavailable)?
                .ok_or(SignalStoreError::NotFound)?;
            decode_log(&document)
        })
    }

    fn list_segments(
        &self,
        project_id: ProjectId,
        query: SegmentQuery,
    ) -> PortFuture<'_, Result<SignalPage<SpanRecord>, SignalStoreError>> {
        Box::pin(self.list_segments_inner(project_id, query))
    }

    fn trace(
        &self,
        project_ids: Vec<ProjectId>,
        trace_id: TraceId,
        maximum_spans: usize,
        maximum_logs: usize,
    ) -> PortFuture<'_, Result<TraceView, SignalStoreError>> {
        Box::pin(self.trace_inner(project_ids, trace_id, maximum_spans, maximum_logs))
    }

    fn performance(
        &self,
        project_id: ProjectId,
        query: PerformanceQuery,
    ) -> PortFuture<'_, Result<Vec<PerformanceBucket>, SignalStoreError>> {
        Box::pin(self.performance_inner(project_id, query))
    }

    fn rebuild_span_stats(
        &self,
        project_id: ProjectId,
        from: Timestamp,
        until: Timestamp,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
        Box::pin(self.rebuild_inner(project_id, from, until))
    }
}

fn encode_log(
    record: &LogRecord,
    retention_days: u32,
    archive: bool,
) -> Result<Document, SignalStoreError> {
    validate_body(record.body.as_bytes())?;
    let mut document = doc! {
        "_id": binary(record.id.as_bytes()),
        "p": record.project_id.get(),
        "r": date(record.received_at),
        "o": record.occurred_at_ns,
        "l": record.severity.code(),
        "m": record.message.as_ref(),
        "b": body_binary(record.body.as_bytes()),
    };
    insert_retention(&mut document, record.received_at, retention_days, archive)?;
    insert_binary(&mut document, "g", record.trace_id.map(TraceId::as_bytes));
    insert_binary(&mut document, "n", record.span_id.map(SpanId::as_bytes));
    insert_optional(&mut document, "e", record.environment.as_deref());
    insert_optional(&mut document, "v", record.release.as_deref());
    insert_optional(&mut document, "j", record.service.as_deref());
    Ok(document)
}

fn decode_log(document: &Document) -> Result<LogRecord, SignalStoreError> {
    Ok(LogRecord {
        id: LogId::from_bytes(fixed_binary(document, "_id")?),
        project_id: ProjectId::new(document.get_i32("p").map_err(invalid)?)
            .map_err(|_| SignalStoreError::InvalidData)?,
        received_at: timestamp(document, "r")?,
        occurred_at_ns: document.get_i64("o").map_err(invalid)?,
        severity: LogSeverity::from_code(document.get_i32("l").map_err(invalid)?)
            .map_err(|_| SignalStoreError::InvalidData)?,
        message: document.get_str("m").map_err(invalid)?.into(),
        trace_id: optional_fixed_binary(document, "g")?.map(TraceId::from_bytes),
        span_id: optional_fixed_binary(document, "n")?.map(SpanId::from_bytes),
        environment: optional_string(document, "e")?,
        release: optional_string(document, "v")?,
        service: optional_string(document, "j")?,
        body: SignalBody::new(decode_body(document)?),
    })
}

fn encode_span(
    record: &SpanRecord,
    retention_days: u32,
    archive: bool,
) -> Result<Document, SignalStoreError> {
    if record.duration_ns < 0 {
        return Err(SignalStoreError::InvalidData);
    }
    validate_body(record.body.as_bytes())?;
    let mut document = doc! {
        "_id": binary(record.id.as_bytes()),
        "p": record.project_id.get(),
        "r": date(record.received_at),
        "o": record.started_at_ns,
        "d": record.duration_ns,
        "g": binary(record.trace_id.as_bytes()),
        "n": binary(record.span_id.as_bytes()),
        "c": record.operation_class.code(),
        "w": record.operation.as_ref(),
        "v": record.status.as_ref(),
        "m": record.name.as_ref(),
        "i": i64::from(record.insight_flags),
        "b": body_binary(record.body.as_bytes()),
    };
    insert_retention(&mut document, record.received_at, retention_days, archive)?;
    if let Some(parent) = record.parent_span_id {
        document.insert("a", binary(parent.as_bytes()));
    }
    if record.is_segment {
        document.insert("t", true);
    }
    insert_optional(&mut document, "e", record.environment.as_deref());
    insert_optional(&mut document, "u", record.release.as_deref());
    insert_optional(&mut document, "j", record.service.as_deref());
    Ok(document)
}

fn decode_span(document: &Document) -> Result<SpanRecord, SignalStoreError> {
    let project_id = ProjectId::new(document.get_i32("p").map_err(invalid)?)
        .map_err(|_| SignalStoreError::InvalidData)?;
    let trace_id = TraceId::from_bytes(fixed_binary(document, "g")?);
    let span_id = SpanId::from_bytes(fixed_binary(document, "n")?);
    Ok(SpanRecord {
        id: SpanRecordId::from_bytes(fixed_binary(document, "_id")?),
        project_id,
        received_at: timestamp(document, "r")?,
        started_at_ns: document.get_i64("o").map_err(invalid)?,
        duration_ns: document.get_i64("d").map_err(invalid)?,
        trace_id,
        span_id,
        parent_span_id: optional_fixed_binary(document, "a")?.map(SpanId::from_bytes),
        is_segment: document.get_bool("t").unwrap_or(false),
        operation_class: SpanOperationClass::from_code(document.get_i32("c").map_err(invalid)?)
            .map_err(|_| SignalStoreError::InvalidData)?,
        operation: document.get_str("w").map_err(invalid)?.into(),
        status: document.get_str("v").map_err(invalid)?.into(),
        name: document.get_str("m").map_err(invalid)?.into(),
        environment: optional_string(document, "e")?,
        release: optional_string(document, "u")?,
        service: optional_string(document, "j")?,
        insight_flags: u32::try_from(document.get_i64("i").unwrap_or(0))
            .map_err(|_| SignalStoreError::InvalidData)?,
        body: SignalBody::new(decode_body(document)?),
    })
}

fn decode_stat(document: &Document) -> Result<PerformanceBucket, SignalStoreError> {
    let count = u64::try_from(document.get_i64("n").map_err(invalid)?)
        .map_err(|_| SignalStoreError::InvalidData)?;
    let failure_count = u64::try_from(document.get_i64("f").map_err(invalid)?)
        .map_err(|_| SignalStoreError::InvalidData)?;
    let sum = document.get_i64("s").map_err(invalid)?;
    let mut samples = document
        .get_array("d")
        .map_err(invalid)?
        .iter()
        .map(|value| value.as_i64().ok_or(SignalStoreError::InvalidData))
        .collect::<Result<Vec<_>, _>>()?;
    samples.sort_unstable();
    Ok(PerformanceBucket {
        hour: timestamp(document, "h")?,
        name: document.get_str("k").map_err(invalid)?.into(),
        service: optional_string(document, "v")?,
        environment: optional_string(document, "e")?,
        release: optional_string(document, "u")?,
        representative_trace_id: TraceId::from_bytes(fixed_binary(document, "g")?),
        operation: SpanOperationClass::from_code(document.get_i32("c").map_err(invalid)?)
            .map_err(|_| SignalStoreError::InvalidData)?,
        count,
        failure_count,
        average_duration_ms: if count == 0 {
            0.0
        } else {
            sum as f64 / count as f64 / 1_000_000.0
        },
        p50_ms: percentile(&samples, 50),
        p75_ms: percentile(&samples, 75),
        p90_ms: percentile(&samples, 90),
        p95_ms: percentile(&samples, 95),
        p99_ms: percentile(&samples, 99),
    })
}

fn percentile(sorted: &[i64], percentile: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = (sorted.len().saturating_sub(1) * percentile).div_ceil(100);
    sorted[index] as f64 / 1_000_000.0
}

fn validate_query(from: i64, until: i64, limit: usize) -> Result<(), SignalStoreError> {
    if from >= until || limit == 0 || limit > MAX_QUERY_LIMIT {
        Err(SignalStoreError::InvalidData)
    } else {
        Ok(())
    }
}

fn validate_body(body: &[u8]) -> Result<(), SignalStoreError> {
    if body.is_empty() || body.len() > 1024 * 1024 || serde_json::from_slice::<Bson>(body).is_err()
    {
        Err(SignalStoreError::InvalidData)
    } else {
        Ok(())
    }
}

fn body_binary(body: &[u8]) -> Binary {
    let mut bytes = Vec::with_capacity(body.len() + 2);
    bytes.extend_from_slice(&[1, 0]);
    bytes.extend_from_slice(body);
    Binary {
        subtype: BinarySubtype::Generic,
        bytes,
    }
}

pub(crate) fn decode_body(document: &Document) -> Result<Box<[u8]>, SignalStoreError> {
    let bytes = document.get_binary_generic("b").map_err(invalid)?;
    if bytes.len() < 3 || bytes[..2] != [1, 0] {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(bytes[2..].into())
}

fn append_signal_anchor(filter: &mut Document, before: Option<SignalCursor>) {
    if let Some(before) = before {
        filter.insert(
            "$or",
            vec![
                doc! { "o": { "$lt": before.time_ns } },
                doc! {
                    "o": before.time_ns,
                    "_id": { "$lt": binary(before.id) },
                },
            ],
        );
    }
}

fn optional_exact(filter: &mut Document, key: &str, value: Option<Box<str>>) {
    if let Some(value) = value {
        filter.insert(key, value.as_ref());
    }
}

fn insert_optional(document: &mut Document, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        document.insert(key, value);
    }
}

fn insert_binary<const N: usize>(document: &mut Document, key: &str, value: Option<[u8; N]>) {
    if let Some(value) = value {
        document.insert(key, binary(value));
    }
}

fn binary(bytes: impl AsRef<[u8]>) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.as_ref().to_vec(),
    }
}

fn fixed_binary<const N: usize>(
    document: &Document,
    key: &str,
) -> Result<[u8; N], SignalStoreError> {
    document
        .get_binary_generic(key)
        .map_err(invalid)?
        .as_slice()
        .try_into()
        .map_err(|_| SignalStoreError::InvalidData)
}

fn optional_fixed_binary<const N: usize>(
    document: &Document,
    key: &str,
) -> Result<Option<[u8; N]>, SignalStoreError> {
    match document.get(key) {
        None => Ok(None),
        Some(Bson::Binary(value)) if value.subtype == BinarySubtype::Generic => value
            .bytes
            .as_slice()
            .try_into()
            .map(Some)
            .map_err(|_| SignalStoreError::InvalidData),
        Some(_) => Err(SignalStoreError::InvalidData),
    }
}

fn optional_string(document: &Document, key: &str) -> Result<Option<Box<str>>, SignalStoreError> {
    document
        .get(key)
        .map(|_| document.get_str(key).map(Box::<str>::from).map_err(invalid))
        .transpose()
}

fn timestamp(document: &Document, key: &str) -> Result<Timestamp, SignalStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(key)
            .map_err(invalid)?
            .timestamp_millis(),
    )
    .map_err(|_| SignalStoreError::InvalidData)
}

fn date(value: Timestamp) -> DateTime {
    DateTime::from_millis(value.unix_millis())
}

fn retention_date(value: Timestamp, days: i64) -> Result<DateTime, SignalStoreError> {
    Timestamp::from_unix_millis(
        value
            .unix_millis()
            .checked_add(days * 86_400_000)
            .ok_or(SignalStoreError::InvalidData)?,
    )
    .map(date)
    .map_err(|_| SignalStoreError::InvalidData)
}

fn insert_retention(
    document: &mut Document,
    received_at: Timestamp,
    days: u32,
    archive: bool,
) -> Result<(), SignalStoreError> {
    let due = retention_date(received_at, i64::from(days))?;
    document.insert(if archive { "h" } else { "x" }, due);
    Ok(())
}

fn unavailable(_: mongodb::error::Error) -> SignalStoreError {
    SignalStoreError::Unavailable
}

fn invalid<T>(_: T) -> SignalStoreError {
    SignalStoreError::InvalidData
}

fn regex_contains(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '.' | '^' | '$' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn stat_id(
    project_id: ProjectId,
    hour: Timestamp,
    name: &str,
    service: Option<&str>,
    environment: Option<&str>,
    release: Option<&str>,
    operation: SpanOperationClass,
) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"span-stats-hourly/v1");
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(&hour.unix_millis().to_be_bytes());
    hasher.update(name.as_bytes());
    hasher.update(&[0]);
    hasher.update(service.unwrap_or_default().as_bytes());
    hasher.update(&[0]);
    hasher.update(environment.unwrap_or_default().as_bytes());
    hasher.update(&[0]);
    hasher.update(release.unwrap_or_default().as_bytes());
    hasher.update(&operation.code().to_be_bytes());
    hasher.finalize().as_bytes()[..16]
        .try_into()
        .expect("BLAKE3 digest prefix")
}

pub fn log_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "p", "r", "o", "l", "m", "b"],
            "properties": {
                "_id": { "bsonType": "binData" },
                "p": { "bsonType": "int", "minimum": 1 },
                "r": { "bsonType": "date" },
                "o": { "bsonType": "long" },
                "l": { "bsonType": "int", "minimum": 1, "maximum": 6 },
                "m": { "bsonType": "string", "maxLength": 8192 },
                "x": { "bsonType": "date" },
                "h": { "bsonType": "date" },
                "z": { "bsonType": "binData" },
                "g": { "bsonType": "binData" },
                "n": { "bsonType": "binData" },
                "e": { "bsonType": "string", "maxLength": 128 },
                "v": { "bsonType": "string", "maxLength": 256 },
                "j": { "bsonType": "string", "maxLength": 256 },
                "b": { "bsonType": "binData" },
            },
        },
    }
}

pub fn span_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "p", "r", "o", "d", "g", "n", "c", "w", "v", "m", "i", "b"],
            "properties": {
                "_id": { "bsonType": "binData" },
                "p": { "bsonType": "int", "minimum": 1 },
                "r": { "bsonType": "date" },
                "o": { "bsonType": "long" },
                "d": { "bsonType": "long", "minimum": 0 },
                "g": { "bsonType": "binData" },
                "n": { "bsonType": "binData" },
                "a": { "bsonType": "binData" },
                "t": { "bsonType": "bool" },
                "c": { "bsonType": "int", "minimum": 0, "maximum": 11 },
                "w": { "bsonType": "string", "maxLength": 128 },
                "v": { "bsonType": "string", "maxLength": 64 },
                "m": { "bsonType": "string", "maxLength": 1024 },
                "e": { "bsonType": "string", "maxLength": 128 },
                "u": { "bsonType": "string", "maxLength": 256 },
                "j": { "bsonType": "string", "maxLength": 256 },
                "i": { "bsonType": "long", "minimum": 0 },
                "x": { "bsonType": "date" },
                "h": { "bsonType": "date" },
                "z": { "bsonType": "binData" },
                "b": { "bsonType": "binData" },
            },
        },
    }
}

pub fn span_stats_validator() -> Document {
    doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "p", "h", "t", "k", "c", "g", "n", "f", "s", "d", "x"],
            "properties": {
                "_id": { "bsonType": "binData" },
                "p": { "bsonType": "int", "minimum": 1 },
                "h": { "bsonType": "date" },
                "t": { "bsonType": "int" },
                "k": { "bsonType": "string", "maxLength": 1024 },
                "v": { "bsonType": "string", "maxLength": 256 },
                "e": { "bsonType": "string", "maxLength": 128 },
                "u": { "bsonType": "string", "maxLength": 256 },
                "c": { "bsonType": "int", "minimum": 0, "maximum": 11 },
                "g": { "bsonType": "binData" },
                "n": { "bsonType": "long", "minimum": 0 },
                "f": { "bsonType": "long", "minimum": 0 },
                "s": { "bsonType": "long", "minimum": 0 },
                "d": { "bsonType": "array", "maxItems": 2048 },
                "x": { "bsonType": "date" },
            },
        },
    }
}

pub async fn create_signal_indexes(database: &Database) -> mongodb::error::Result<()> {
    for model in log_indexes() {
        database
            .collection::<Document>("logs")
            .create_index(model)
            .await?;
    }
    for model in span_indexes() {
        database
            .collection::<Document>("spans")
            .create_index(model)
            .await?;
    }
    for model in stat_indexes() {
        database
            .collection::<Document>("span_stats_hourly")
            .create_index(model)
            .await?;
    }
    Ok(())
}

pub fn signal_index_names(collection: &str) -> std::collections::BTreeSet<&'static str> {
    match collection {
        "logs" => std::collections::BTreeSet::from([
            "_id_",
            "log_project_time",
            "log_project_trace",
            "log_archive_due",
            "log_expiry",
        ]),
        "spans" => std::collections::BTreeSet::from([
            "_id_",
            "span_project_trace",
            "span_segment_feed",
            "span_archive_due",
            "span_expiry",
        ]),
        "span_stats_hourly" => std::collections::BTreeSet::from([
            "_id_",
            "span_stats_project_hour",
            "span_stats_expiry",
        ]),
        _ => std::collections::BTreeSet::new(),
    }
}

fn log_indexes() -> Vec<IndexModel> {
    vec![
        index(doc! { "p": 1, "o": -1, "_id": -1 }, "log_project_time"),
        partial_index(
            doc! { "p": 1, "g": 1, "o": 1 },
            "log_project_trace",
            doc! { "g": { "$exists": true } },
        ),
        partial_index(
            doc! { "h": 1, "_id": 1 },
            "log_archive_due",
            doc! { "h": { "$exists": true } },
        ),
        ttl_index("x", "log_expiry"),
    ]
}

fn span_indexes() -> Vec<IndexModel> {
    vec![
        index(
            doc! { "p": 1, "g": 1, "o": 1, "n": 1 },
            "span_project_trace",
        ),
        partial_index(
            doc! { "p": 1, "t": 1, "o": -1, "_id": -1 },
            "span_segment_feed",
            doc! { "t": true },
        ),
        partial_index(
            doc! { "h": 1, "_id": 1 },
            "span_archive_due",
            doc! { "h": { "$exists": true } },
        ),
        ttl_index("x", "span_expiry"),
    ]
}

fn stat_indexes() -> Vec<IndexModel> {
    vec![
        index(doc! { "p": 1, "h": -1, "n": -1 }, "span_stats_project_hour"),
        IndexModel::builder()
            .keys(doc! { "x": 1 })
            .options(
                IndexOptions::builder()
                    .name("span_stats_expiry".to_owned())
                    .expire_after(std::time::Duration::ZERO)
                    .build(),
            )
            .build(),
    ]
}

fn index(keys: Document, name: &str) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(IndexOptions::builder().name(name.to_owned()).build())
        .build()
}

fn partial_index(keys: Document, name: &str, filter: Document) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .partial_filter_expression(filter)
                .build(),
        )
        .build()
}

fn ttl_index(key: &str, name: &str) -> IndexModel {
    let mut keys = Document::new();
    keys.insert(key, 1);
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .expire_after(std::time::Duration::ZERO)
                .build(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace_id() -> TraceId {
        TraceId::parse("0123456789abcdef0123456789abcdef").unwrap()
    }

    fn span_id() -> SpanId {
        SpanId::parse("0123456789abcdef").unwrap()
    }

    #[test]
    fn compact_signal_codecs_round_trip_with_independent_retention() {
        let project_id = ProjectId::new(7).unwrap();
        let received_at = Timestamp::from_unix_millis(1_700_000_000_000).unwrap();
        let log = LogRecord {
            id: LogId::deterministic(project_id, received_at, 1_700_000_000_123_000_000, b"log"),
            project_id,
            received_at,
            occurred_at_ns: 1_700_000_000_123_000_000,
            severity: LogSeverity::Warn,
            message: "database retry".into(),
            trace_id: Some(trace_id()),
            span_id: Some(span_id()),
            environment: Some("production".into()),
            release: Some("api@1".into()),
            service: Some("payments".into()),
            body: SignalBody::new(br#"{"body":"database retry","attempt":2}"#.as_slice()),
        };
        let log_document = encode_log(&log, 7, false).unwrap();
        assert_eq!(decode_log(&log_document).unwrap(), log);
        assert_eq!(
            log_document.get_datetime("x").unwrap().timestamp_millis()
                - log_document.get_datetime("r").unwrap().timestamp_millis(),
            7 * 86_400_000
        );
        assert!(mongodb::bson::to_vec(&log_document).unwrap().len() < 512);

        let span = SpanRecord {
            id: SpanRecordId::deterministic(project_id, trace_id(), span_id()),
            project_id,
            received_at,
            started_at_ns: 1_700_000_000_000_000_000,
            duration_ns: 25_000_000,
            trace_id: trace_id(),
            span_id: span_id(),
            parent_span_id: None,
            is_segment: true,
            operation_class: SpanOperationClass::HttpServer,
            operation: "http.server".into(),
            status: "ok".into(),
            name: "GET /checkout".into(),
            environment: Some("production".into()),
            release: Some("api@1".into()),
            service: Some("payments".into()),
            insight_flags: 1,
            body: SignalBody::new(br#"{"request":{"method":"GET"}}"#.as_slice()),
        };
        let span_document = encode_span(&span, 14, false).unwrap();
        assert_eq!(decode_span(&span_document).unwrap(), span);
        assert_eq!(
            span_document.get_datetime("x").unwrap().timestamp_millis()
                - span_document.get_datetime("r").unwrap().timestamp_millis(),
            14 * 86_400_000
        );
        assert!(mongodb::bson::to_vec(&span_document).unwrap().len() < 512);

        let archived_log = encode_log(&log, 7, true).unwrap();
        let archived_span = encode_span(&span, 14, true).unwrap();
        for document in [&archived_log, &archived_span] {
            assert!(document.contains_key("h"));
            assert!(!document.contains_key("x"));
            assert!(!document.contains_key("z"));
        }
    }

    #[test]
    fn percentile_and_regex_contracts_are_deterministic() {
        assert_eq!(percentile(&[1_000_000, 2_000_000, 3_000_000], 50), 2.0);
        assert_eq!(percentile(&[1_000_000, 2_000_000, 3_000_000], 99), 3.0);
        assert_eq!(regex_contains("db.*"), r"db\.\*");
    }

    #[test]
    fn performance_bucket_decode_pins_percentile_and_approximation_inputs() {
        let document = doc! {
            "_id": binary([9_u8; 16]),
            "p": 7_i32,
            "h": DateTime::from_millis(1_700_000_000_000),
            "t": 1_i32,
            "k": "GET /checkout",
            "v": "payments",
            "e": "production",
            "u": "api@1",
            "c": SpanOperationClass::HttpServer.code(),
            "g": binary(trace_id().as_bytes()),
            "n": 4_i64,
            "f": 1_i64,
            "s": 10_000_000_i64,
            "d": [1_000_000_i64, 2_000_000_i64, 3_000_000_i64, 4_000_000_i64],
            "x": DateTime::from_millis(1_700_086_400_000),
        };
        let bucket = decode_stat(&document).unwrap();
        assert_eq!(bucket.count, 4);
        assert_eq!(bucket.failure_count, 1);
        assert_eq!(bucket.average_duration_ms, 2.5);
        assert_eq!(bucket.p50_ms, 3.0);
        assert_eq!(bucket.p95_ms, 4.0);
        assert_eq!(bucket.environment.as_deref(), Some("production"));
        assert_eq!(bucket.release.as_deref(), Some("api@1"));
        assert_eq!(bucket.representative_trace_id, trace_id());
    }

    #[test]
    fn stat_identity_changes_with_dimension() {
        let project = ProjectId::new(7).unwrap();
        let hour = Timestamp::from_unix_millis(0).unwrap();
        assert_ne!(
            stat_id(
                project,
                hour,
                "GET /",
                Some("api"),
                Some("production"),
                Some("api@1"),
                SpanOperationClass::HttpServer,
            ),
            stat_id(
                project,
                hour,
                "GET /",
                Some("worker"),
                Some("production"),
                Some("api@1"),
                SpanOperationClass::HttpServer,
            )
        );
    }
}
