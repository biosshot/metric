//! Compact MongoDB Cron monitor definitions and TTL run history.

use std::{collections::HashMap, time::Duration};

use futures_util::TryStreamExt;
use metric_domain::{
    ProjectId, Timestamp,
    finalization::{EnvironmentId, ReleaseId},
    monitors::{
        MonitorAnchor, MonitorConfig, MonitorDefinition, MonitorId, MonitorPage, MonitorRun,
        MonitorRunAnchor, MonitorRunId, MonitorRunPage, MonitorRunSource, MonitorRunStatus,
        MonitorSchedule, MonitorUpdate, SealedUptimeHeaderValue, UptimeEndpoint, UptimeFailure,
        UptimeHeader, UptimeMethod, UptimeMonitorConfig,
    },
};
use metric_ports::{DurableOutcome, MonitorStore, PortFuture, SignalStoreError};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::{FindOptions, IndexOptions, UpdateOneModel},
};

const DAY_MILLIS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy)]
pub struct MonitorRetention {
    pub runs_days: u32,
}

impl Default for MonitorRetention {
    fn default() -> Self {
        Self { runs_days: 90 }
    }
}

#[derive(Clone)]
pub struct MongoMonitorStore {
    database: Database,
    retention: MonitorRetention,
}

impl MongoMonitorStore {
    #[must_use]
    pub const fn new(database: Database, retention: MonitorRetention) -> Self {
        Self {
            database,
            retention,
        }
    }

    async fn persist_inner(
        &self,
        updates: Vec<MonitorUpdate>,
    ) -> Result<Vec<DurableOutcome>, SignalStoreError> {
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        for update in &updates {
            update
                .validate()
                .map_err(|_| SignalStoreError::InvalidData)?;
        }
        let monitor_ids = updates
            .iter()
            .map(|update| Bson::Binary(binary(update.run.monitor_id.as_bytes())))
            .collect::<Vec<_>>();
        let mut monitors = HashMap::new();
        let mut monitor_cursor = self
            .database
            .collection::<Document>("monitors")
            .find(doc! { "_id": { "$in": monitor_ids } })
            .await
            .map_err(unavailable)?;
        while let Some(document) = monitor_cursor.try_next().await.map_err(unavailable)? {
            let monitor = decode_monitor(&document)?;
            monitors.insert(monitor.id, monitor);
        }
        for update in &updates {
            if let Some(definition) = &update.definition {
                match monitors.get(&definition.id) {
                    Some(existing) if existing.managed_by_web => {}
                    Some(existing) => {
                        let mut merged = definition.clone();
                        merged.created_at = existing.created_at;
                        merged.revision = existing.revision.saturating_add(1);
                        merged.last_run_id = existing.last_run_id;
                        merged.last_status = existing.last_status;
                        merged.last_check_in_at = existing.last_check_in_at;
                        monitors.insert(merged.id, merged);
                    }
                    None => {
                        monitors.insert(definition.id, definition.clone());
                    }
                }
            }
            if !monitors.contains_key(&update.run.monitor_id) {
                return Err(SignalStoreError::NotFound);
            }
        }

        let namespace = self.database.collection::<Document>("monitors").namespace();
        let mut monitor_models = Vec::new();
        for monitor in monitors.values() {
            monitor_models.push(
                UpdateOneModel::builder()
                    .namespace(namespace.clone())
                    .filter(doc! { "_id": binary(monitor.id.as_bytes()) })
                    .update(doc! { "$set": encode_monitor(monitor)? })
                    .upsert(true)
                    .build(),
            );
        }
        if !monitor_models.is_empty() {
            self.database
                .client()
                .bulk_write(monitor_models)
                .ordered(false)
                .await
                .map_err(unavailable)?;
        }

        let run_ids = updates
            .iter()
            .map(|update| Bson::Binary(binary(update.run.id.as_bytes())))
            .collect::<Vec<_>>();
        let mut existing_runs = HashMap::new();
        let mut run_cursor = self
            .database
            .collection::<Document>("monitor_runs")
            .find(doc! { "_id": { "$in": run_ids } })
            .await
            .map_err(unavailable)?;
        while let Some(document) = run_cursor.try_next().await.map_err(unavailable)? {
            let run = decode_run(&document)?;
            existing_runs.insert(run.id, run);
        }

        let run_namespace = self
            .database
            .collection::<Document>("monitor_runs")
            .namespace();
        let mut run_models = Vec::new();
        let mut outcomes = Vec::with_capacity(updates.len());
        let mut accepted_runs = Vec::new();
        for update in updates {
            let mut incoming = update.run;
            let monitor = monitors
                .get(&incoming.monitor_id)
                .ok_or(SignalStoreError::NotFound)?;
            if incoming.status == MonitorRunStatus::InProgress && incoming.timeout_at.is_none() {
                incoming.timeout_at = Some(
                    monitor
                        .config
                        .timeout_at(incoming.started_at)
                        .map_err(|_| SignalStoreError::InvalidData)?,
                );
            }
            match existing_runs.get(&incoming.id) {
                Some(existing) => {
                    validate_same_run(existing, &incoming)?;
                    if existing.status == MonitorRunStatus::InProgress
                        && incoming.status.is_terminal()
                    {
                        run_models.push(
                            UpdateOneModel::builder()
                                .namespace(run_namespace.clone())
                                .filter(doc! {
                                    "_id": binary(incoming.id.as_bytes()),
                                    "s": status_tag(MonitorRunStatus::InProgress),
                                })
                                .update(doc! { "$set": terminal_fields(&incoming)? })
                                .build(),
                        );
                        existing_runs.insert(incoming.id, incoming.clone());
                        accepted_runs.push(incoming);
                        outcomes.push(DurableOutcome::Accepted);
                    } else {
                        outcomes.push(DurableOutcome::Duplicate);
                    }
                }
                None => {
                    run_models.push(
                        UpdateOneModel::builder()
                            .namespace(run_namespace.clone())
                            .filter(doc! { "_id": binary(incoming.id.as_bytes()) })
                            .update(doc! { "$setOnInsert": encode_run(&incoming)? })
                            .upsert(true)
                            .build(),
                    );
                    existing_runs.insert(incoming.id, incoming.clone());
                    accepted_runs.push(incoming);
                    outcomes.push(DurableOutcome::Accepted);
                }
            }
        }
        if !run_models.is_empty() {
            self.database
                .client()
                .bulk_write(run_models)
                .ordered(true)
                .await
                .map_err(unavailable)?;
        }
        let mut projections = HashMap::new();
        for run in &accepted_runs {
            let replace = projections
                .get(&run.monitor_id)
                .is_none_or(|current: &&MonitorRun| current.received_at <= run.received_at);
            if replace {
                projections.insert(run.monitor_id, run);
            }
        }
        for run in projections.into_values() {
            let monitor = monitors
                .get(&run.monitor_id)
                .ok_or(SignalStoreError::NotFound)?;
            self.apply_projection(run, monitor).await?;
        }
        Ok(outcomes)
    }

    async fn apply_projection(
        &self,
        run: &MonitorRun,
        monitor: &MonitorDefinition,
    ) -> Result<(), SignalStoreError> {
        let mut set = doc! {
            "u": binary(run.id.as_bytes()),
            "s": status_tag(run.status),
            "h": date(run.received_at),
            "o": date(run.received_at),
        };
        if run.status.is_terminal() {
            let next = monitor
                .config
                .schedule
                .next_after(run.received_at)
                .map_err(|_| SignalStoreError::InvalidData)?;
            let due = if monitor.is_uptime() {
                next
            } else {
                monitor
                    .config
                    .missed_at(next)
                    .map_err(|_| SignalStoreError::InvalidData)?
            };
            set.insert("f", date(next));
            set.insert("d", date(due));
        }
        let update = if monitor.is_uptime() && run.status.is_terminal() {
            doc! { "$set": set, "$unset": { "y": "" } }
        } else {
            doc! { "$set": set }
        };
        self.database
            .collection::<Document>("monitors")
            .update_one(
                doc! {
                    "_id": binary(run.monitor_id.as_bytes()),
                    "$or": [
                        { "h": { "$exists": false } },
                        { "h": { "$lte": date(run.received_at) } },
                    ],
                },
                update,
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn upsert_inner(
        &self,
        monitor: MonitorDefinition,
    ) -> Result<MonitorDefinition, SignalStoreError> {
        monitor
            .validate()
            .map_err(|_| SignalStoreError::InvalidData)?;
        self.database
            .collection::<Document>("monitors")
            .update_one(
                doc! { "_id": binary(monitor.id.as_bytes()), "p": monitor.project_id.get() },
                doc! { "$set": encode_monitor(&monitor)? },
            )
            .upsert(true)
            .await
            .map_err(unavailable)?;
        Ok(monitor)
    }

    async fn load_inner(
        &self,
        project_id: ProjectId,
        monitor_id: MonitorId,
    ) -> Result<MonitorDefinition, SignalStoreError> {
        self.database
            .collection::<Document>("monitors")
            .find_one(doc! { "_id": binary(monitor_id.as_bytes()), "p": project_id.get() })
            .await
            .map_err(unavailable)?
            .ok_or(SignalStoreError::NotFound)
            .and_then(|document| decode_monitor(&document))
    }

    async fn list_inner(
        &self,
        project_id: ProjectId,
        before: Option<MonitorAnchor>,
        limit: usize,
    ) -> Result<MonitorPage, SignalStoreError> {
        if limit == 0 || limit > 100 {
            return Err(SignalStoreError::InvalidData);
        }
        let mut filter = doc! { "p": project_id.get() };
        if let Some(before) = before {
            filter.insert(
                "$or",
                vec![
                    doc! { "o": { "$lt": date(before.updated_at) } },
                    doc! {
                        "o": date(before.updated_at),
                        "_id": { "$lt": binary(before.monitor_id.as_bytes()) },
                    },
                ],
            );
        }
        let options = FindOptions::builder()
            .sort(doc! { "o": -1, "_id": -1 })
            .limit(i64::try_from(limit + 1).map_err(|_| SignalStoreError::InvalidData)?)
            .build();
        let mut cursor = self
            .database
            .collection::<Document>("monitors")
            .find(filter)
            .with_options(options)
            .await
            .map_err(unavailable)?;
        let mut items = Vec::with_capacity(limit + 1);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            items.push(decode_monitor(&document)?);
        }
        let next = if items.len() > limit {
            items.pop();
            items.last().map(|monitor| MonitorAnchor {
                updated_at: monitor.updated_at,
                monitor_id: monitor.id,
            })
        } else {
            None
        };
        Ok(MonitorPage { items, next })
    }

    async fn list_runs_inner(
        &self,
        project_id: ProjectId,
        monitor_id: MonitorId,
        before: Option<MonitorRunAnchor>,
        limit: usize,
    ) -> Result<MonitorRunPage, SignalStoreError> {
        if limit == 0 || limit > 100 {
            return Err(SignalStoreError::InvalidData);
        }
        let mut filter = doc! { "p": project_id.get(), "m": binary(monitor_id.as_bytes()) };
        if let Some(before) = before {
            filter.insert(
                "$or",
                vec![
                    doc! { "i": { "$lt": date(before.started_at) } },
                    doc! {
                        "i": date(before.started_at),
                        "_id": { "$lt": binary(before.run_id.as_bytes()) },
                    },
                ],
            );
        }
        let options = FindOptions::builder()
            .sort(doc! { "i": -1, "_id": -1 })
            .limit(i64::try_from(limit + 1).map_err(|_| SignalStoreError::InvalidData)?)
            .build();
        let mut cursor = self
            .database
            .collection::<Document>("monitor_runs")
            .find(filter)
            .with_options(options)
            .await
            .map_err(unavailable)?;
        let mut items = Vec::with_capacity(limit + 1);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            items.push(decode_run(&document)?);
        }
        let next = if items.len() > limit {
            items.pop();
            items.last().map(|run| MonitorRunAnchor {
                started_at: run.started_at,
                run_id: run.id,
            })
        } else {
            None
        };
        Ok(MonitorRunPage { items, next })
    }

    async fn timeout_inner(
        &self,
        now: Timestamp,
        batch_size: usize,
    ) -> Result<u64, SignalStoreError> {
        if batch_size == 0 || batch_size > 10_000 {
            return Err(SignalStoreError::InvalidData);
        }
        let options = FindOptions::builder()
            .sort(doc! { "t": 1, "_id": 1 })
            .limit(i64::try_from(batch_size).map_err(|_| SignalStoreError::InvalidData)?)
            .build();
        let mut cursor = self
            .database
            .collection::<Document>("monitor_runs")
            .find(doc! {
                "s": status_tag(MonitorRunStatus::InProgress),
                "t": { "$lte": date(now) },
            })
            .with_options(options)
            .await
            .map_err(unavailable)?;
        let mut changed = 0_u64;
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let mut run = decode_run(&document)?;
            run.status = MonitorRunStatus::Timeout;
            run.finished_at = Some(now);
            run.received_at = now;
            run.duration_ms = Some(
                now.unix_millis()
                    .saturating_sub(run.started_at.unix_millis())
                    .max(0) as u64,
            );
            let result = self
                .database
                .collection::<Document>("monitor_runs")
                .update_one(
                    doc! {
                        "_id": binary(run.id.as_bytes()),
                        "s": status_tag(MonitorRunStatus::InProgress),
                    },
                    doc! { "$set": terminal_fields(&run)? },
                )
                .await
                .map_err(unavailable)?;
            if result.modified_count == 1 {
                changed = changed.saturating_add(1);
                let monitor = self.load_inner(run.project_id, run.monitor_id).await?;
                self.apply_projection(&run, &monitor).await?;
            }
        }
        Ok(changed)
    }

    async fn missed_inner(
        &self,
        now: Timestamp,
        batch_size: usize,
    ) -> Result<u64, SignalStoreError> {
        if batch_size == 0 || batch_size > 10_000 {
            return Err(SignalStoreError::InvalidData);
        }
        let options = FindOptions::builder()
            .sort(doc! { "d": 1, "_id": 1 })
            .limit(i64::try_from(batch_size).map_err(|_| SignalStoreError::InvalidData)?)
            .build();
        let mut cursor = self
            .database
            .collection::<Document>("monitors")
            .find(doc! { "k": 0, "a": true, "d": { "$lte": date(now) } })
            .with_options(options)
            .await
            .map_err(unavailable)?;
        let mut changed = 0_u64;
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let monitor = decode_monitor(&document)?;
            let run = MonitorRun {
                id: MonitorRunId::missed(monitor.id, monitor.next_expected_at),
                project_id: monitor.project_id,
                monitor_id: monitor.id,
                check_in_id: None,
                status: MonitorRunStatus::Missed,
                source: MonitorRunSource::Scheduler,
                scheduled_for: Some(monitor.next_expected_at),
                started_at: monitor.next_expected_at,
                finished_at: Some(now),
                duration_ms: None,
                received_at: now,
                release_id: None,
                timeout_at: None,
                delete_at: Some(add_days(now, self.retention.runs_days)?),
                http_status: None,
                uptime_failure: None,
            };
            self.database
                .collection::<Document>("monitor_runs")
                .update_one(
                    doc! { "_id": binary(run.id.as_bytes()) },
                    doc! { "$setOnInsert": encode_run(&run)? },
                )
                .upsert(true)
                .await
                .map_err(unavailable)?;
            let next = monitor
                .config
                .schedule
                .next_after(now)
                .map_err(|_| SignalStoreError::InvalidData)?;
            let due = monitor
                .config
                .missed_at(next)
                .map_err(|_| SignalStoreError::InvalidData)?;
            let result = self
                .database
                .collection::<Document>("monitors")
                .update_one(
                    doc! {
                        "_id": binary(monitor.id.as_bytes()),
                        "f": date(monitor.next_expected_at),
                    },
                    doc! { "$set": {
                        "f": date(next),
                        "d": date(due),
                        "u": binary(run.id.as_bytes()),
                        "s": status_tag(MonitorRunStatus::Missed),
                        "h": date(now),
                        "o": date(now),
                    }},
                )
                .await
                .map_err(unavailable)?;
            changed = changed.saturating_add(result.modified_count);
        }
        Ok(changed)
    }

    async fn claim_uptime_inner(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        limit: usize,
    ) -> Result<Vec<MonitorDefinition>, SignalStoreError> {
        if limit == 0 || limit > 1_000 || lease_until <= now {
            return Err(SignalStoreError::InvalidData);
        }
        let options = FindOptions::builder()
            .sort(doc! { "d": 1, "_id": 1 })
            .limit(i64::try_from(limit).map_err(|_| SignalStoreError::InvalidData)?)
            .build();
        let mut cursor = self
            .database
            .collection::<Document>("monitors")
            .find(doc! {
                "k": 1,
                "a": true,
                "d": { "$lte": date(now) },
                "$or": [{ "y": { "$exists": false } }, { "y": { "$lte": date(now) } }],
            })
            .with_options(options)
            .await
            .map_err(unavailable)?;
        let mut candidates = Vec::with_capacity(limit);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            candidates.push((
                MonitorId::from_bytes(id16(&document, "_id")?),
                timestamp(&document, "d")?,
            ));
        }
        let collection = self.database.collection::<Document>("monitors");
        let mut claimed = Vec::with_capacity(candidates.len());
        for (id, due) in candidates {
            if let Some(document) = collection
                .find_one_and_update(
                    doc! {
                        "_id": binary(id.as_bytes()),
                        "k": 1,
                        "a": true,
                        "d": date(due),
                        "$or": [{ "y": { "$exists": false } }, { "y": { "$lte": date(now) } }],
                    },
                    doc! { "$set": { "y": date(lease_until) } },
                )
                .return_document(mongodb::options::ReturnDocument::After)
                .await
                .map_err(unavailable)?
            {
                claimed.push(decode_monitor(&document)?);
            }
        }
        Ok(claimed)
    }

    async fn pending_alerts_inner(
        &self,
        limit: usize,
    ) -> Result<Vec<MonitorRun>, SignalStoreError> {
        if limit == 0 || limit > 1_000 {
            return Err(SignalStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("monitor_runs")
            .find(doc! {
                "$or": [
                    { "s": { "$in": [
                        status_tag(MonitorRunStatus::Error),
                        status_tag(MonitorRunStatus::Timeout),
                        status_tag(MonitorRunStatus::Missed),
                    ]}},
                    { "s": status_tag(MonitorRunStatus::Success), "a": { "$exists": true } },
                ],
                "z": { "$exists": false },
            })
            .sort(doc! { "r": 1, "_id": 1 })
            .limit(i64::try_from(limit).map_err(|_| SignalStoreError::InvalidData)?)
            .await
            .map_err(unavailable)?;
        let mut runs = Vec::new();
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            runs.push(decode_run(&document)?);
        }
        Ok(runs)
    }
}

impl MonitorStore for MongoMonitorStore {
    fn persist_monitors(
        &self,
        updates: Vec<MonitorUpdate>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(self.persist_inner(updates))
    }

    fn upsert_monitor(
        &self,
        monitor: MonitorDefinition,
    ) -> PortFuture<'_, Result<MonitorDefinition, SignalStoreError>> {
        Box::pin(self.upsert_inner(monitor))
    }

    fn load_monitor(
        &self,
        project_id: ProjectId,
        monitor_id: MonitorId,
    ) -> PortFuture<'_, Result<MonitorDefinition, SignalStoreError>> {
        Box::pin(self.load_inner(project_id, monitor_id))
    }

    fn list_monitors(
        &self,
        project_id: ProjectId,
        before: Option<MonitorAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<MonitorPage, SignalStoreError>> {
        Box::pin(self.list_inner(project_id, before, limit))
    }

    fn list_monitor_runs(
        &self,
        project_id: ProjectId,
        monitor_id: MonitorId,
        before: Option<MonitorRunAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<MonitorRunPage, SignalStoreError>> {
        Box::pin(self.list_runs_inner(project_id, monitor_id, before, limit))
    }

    fn terminalize_due_timeouts(
        &self,
        now: Timestamp,
        batch_size: usize,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
        Box::pin(self.timeout_inner(now, batch_size))
    }

    fn materialize_due_missed(
        &self,
        now: Timestamp,
        batch_size: usize,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
        Box::pin(self.missed_inner(now, batch_size))
    }

    fn claim_due_uptime(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<MonitorDefinition>, SignalStoreError>> {
        Box::pin(self.claim_uptime_inner(now, lease_until, limit))
    }

    fn pending_monitor_alerts(
        &self,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<MonitorRun>, SignalStoreError>> {
        Box::pin(self.pending_alerts_inner(limit))
    }

    fn mark_monitor_alert_evaluated(
        &self,
        run_id: MonitorRunId,
        evaluated_at: Timestamp,
    ) -> PortFuture<'_, Result<(), SignalStoreError>> {
        Box::pin(async move {
            self.database
                .collection::<Document>("monitor_runs")
                .update_one(
                    doc! { "_id": binary(run_id.as_bytes()) },
                    doc! { "$set": { "z": date(evaluated_at) } },
                )
                .await
                .map_err(unavailable)?;
            Ok(())
        })
    }
}

fn validate_same_run(existing: &MonitorRun, incoming: &MonitorRun) -> Result<(), SignalStoreError> {
    if existing.project_id != incoming.project_id
        || existing.monitor_id != incoming.monitor_id
        || existing.check_in_id != incoming.check_in_id
        || existing.source != incoming.source
    {
        return Err(SignalStoreError::Conflict);
    }
    Ok(())
}

fn encode_monitor(monitor: &MonitorDefinition) -> Result<Document, SignalStoreError> {
    monitor
        .validate()
        .map_err(|_| SignalStoreError::InvalidData)?;
    let schedule_tag = match monitor.config.schedule {
        MonitorSchedule::Interval { .. } => 0,
        MonitorSchedule::Crontab { .. } => 1,
    };
    let due = if monitor.is_uptime() {
        monitor.next_expected_at
    } else {
        monitor
            .config
            .missed_at(monitor.next_expected_at)
            .map_err(|_| SignalStoreError::InvalidData)?
    };
    let mut document = doc! {
        "p": monitor.project_id.get(),
        "k": if monitor.is_uptime() { 1 } else { 0 },
        "l": monitor.slug.as_ref(),
        "n": monitor.name.as_ref(),
        "e": binary(monitor.environment_id.as_bytes()),
        "v": monitor.environment.as_ref(),
        "a": monitor.enabled,
        "w": monitor.managed_by_web,
        "r": i64::try_from(monitor.revision).map_err(|_| SignalStoreError::InvalidData)?,
        "c": {
            "t": schedule_tag,
            "q": monitor.config.schedule.value().as_ref(),
            "m": i64::from(monitor.config.checkin_margin_seconds),
            "x": i64::from(monitor.config.max_runtime_seconds),
        },
        "f": date(monitor.next_expected_at),
        "d": date(due),
        "i": date(monitor.created_at),
        "o": date(monitor.updated_at),
    };
    if let Some(uptime) = &monitor.uptime {
        let headers = uptime
            .headers
            .iter()
            .map(|header| {
                doc! {
                    "n": header.name.as_ref(),
                    "v": Binary {
                        subtype: BinarySubtype::Generic,
                        bytes: header.value.expose_ciphertext().to_vec(),
                    },
                    "s": header.sensitive,
                }
            })
            .collect::<Vec<_>>();
        document.insert(
            "b",
            doc! {
                "u": uptime.endpoint.as_str(),
                "m": uptime.method.as_str(),
                "l": i32::from(uptime.expected_status_min),
                "h": i32::from(uptime.expected_status_max),
                "t": i64::from(uptime.timeout_seconds),
                "r": i32::from(uptime.max_redirects),
                "c": headers,
            },
        );
    }
    if let Some(value) = monitor.last_run_id {
        document.insert("u", binary(value.as_bytes()));
    }
    if let Some(value) = monitor.last_status {
        document.insert("s", status_tag(value));
    }
    if let Some(value) = monitor.last_check_in_at {
        document.insert("h", date(value));
    }
    Ok(document)
}

fn decode_monitor(document: &Document) -> Result<MonitorDefinition, SignalStoreError> {
    let config = document
        .get_document("c")
        .map_err(|_| SignalStoreError::InvalidData)?;
    let schedule = match config
        .get_i32("t")
        .map_err(|_| SignalStoreError::InvalidData)?
    {
        0 => MonitorSchedule::interval(
            config
                .get_str("q")
                .map_err(|_| SignalStoreError::InvalidData)?
                .parse()
                .map_err(|_| SignalStoreError::InvalidData)?,
        ),
        1 => MonitorSchedule::crontab(
            config
                .get_str("q")
                .map_err(|_| SignalStoreError::InvalidData)?,
        ),
        _ => return Err(SignalStoreError::InvalidData),
    }
    .map_err(|_| SignalStoreError::InvalidData)?;
    let uptime = document
        .get_document("b")
        .ok()
        .map(decode_uptime)
        .transpose()?;
    let monitor = MonitorDefinition {
        id: MonitorId::from_bytes(id16(document, "_id")?),
        project_id: project_id(document)?,
        slug: text(document, "l")?,
        name: text(document, "n")?,
        environment_id: EnvironmentId::from_bytes(id16(document, "e")?),
        environment: text(document, "v")?,
        enabled: document
            .get_bool("a")
            .map_err(|_| SignalStoreError::InvalidData)?,
        managed_by_web: document
            .get_bool("w")
            .map_err(|_| SignalStoreError::InvalidData)?,
        revision: u64::try_from(
            document
                .get_i64("r")
                .map_err(|_| SignalStoreError::InvalidData)?,
        )
        .map_err(|_| SignalStoreError::InvalidData)?,
        config: MonitorConfig {
            schedule,
            checkin_margin_seconds: u32::try_from(
                config
                    .get_i64("m")
                    .map_err(|_| SignalStoreError::InvalidData)?,
            )
            .map_err(|_| SignalStoreError::InvalidData)?,
            max_runtime_seconds: u32::try_from(
                config
                    .get_i64("x")
                    .map_err(|_| SignalStoreError::InvalidData)?,
            )
            .map_err(|_| SignalStoreError::InvalidData)?,
        },
        uptime,
        next_expected_at: timestamp(document, "f")?,
        last_run_id: optional_id16(document, "u")?.map(MonitorRunId::from_bytes),
        last_status: document.get_i32("s").ok().map(parse_status).transpose()?,
        last_check_in_at: optional_timestamp(document, "h")?,
        created_at: timestamp(document, "i")?,
        updated_at: timestamp(document, "o")?,
    };
    monitor
        .validate()
        .map_err(|_| SignalStoreError::InvalidData)?;
    Ok(monitor)
}

fn decode_uptime(document: &Document) -> Result<UptimeMonitorConfig, SignalStoreError> {
    let headers = document
        .get_array("c")
        .map_err(|_| SignalStoreError::InvalidData)?
        .iter()
        .map(|value| {
            let header = value.as_document().ok_or(SignalStoreError::InvalidData)?;
            Ok(UptimeHeader {
                name: text(header, "n")?,
                value: SealedUptimeHeaderValue::new(
                    header
                        .get_binary_generic("v")
                        .map_err(|_| SignalStoreError::InvalidData)?
                        .to_vec(),
                )
                .map_err(|_| SignalStoreError::InvalidData)?,
                sensitive: header
                    .get_bool("s")
                    .map_err(|_| SignalStoreError::InvalidData)?,
            })
        })
        .collect::<Result<Vec<_>, SignalStoreError>>()?;
    Ok(UptimeMonitorConfig {
        endpoint: UptimeEndpoint::new(text(document, "u")?)
            .map_err(|_| SignalStoreError::InvalidData)?,
        method: UptimeMethod::parse(
            document
                .get_str("m")
                .map_err(|_| SignalStoreError::InvalidData)?,
        )
        .map_err(|_| SignalStoreError::InvalidData)?,
        expected_status_min: u16::try_from(
            document
                .get_i32("l")
                .map_err(|_| SignalStoreError::InvalidData)?,
        )
        .map_err(|_| SignalStoreError::InvalidData)?,
        expected_status_max: u16::try_from(
            document
                .get_i32("h")
                .map_err(|_| SignalStoreError::InvalidData)?,
        )
        .map_err(|_| SignalStoreError::InvalidData)?,
        timeout_seconds: u32::try_from(
            document
                .get_i64("t")
                .map_err(|_| SignalStoreError::InvalidData)?,
        )
        .map_err(|_| SignalStoreError::InvalidData)?,
        max_redirects: u8::try_from(
            document
                .get_i32("r")
                .map_err(|_| SignalStoreError::InvalidData)?,
        )
        .map_err(|_| SignalStoreError::InvalidData)?,
        headers: headers.into_boxed_slice(),
    })
}

fn encode_run(run: &MonitorRun) -> Result<Document, SignalStoreError> {
    run.validate().map_err(|_| SignalStoreError::InvalidData)?;
    let mut document = doc! {
        "_id": binary(run.id.as_bytes()),
        "p": run.project_id.get(),
        "m": binary(run.monitor_id.as_bytes()),
        "s": status_tag(run.status),
        "g": source_tag(run.source),
        "i": date(run.started_at),
        "r": date(run.received_at),
    };
    if let Some(value) = run.check_in_id {
        document.insert("c", binary(value));
    }
    if let Some(value) = run.scheduled_for {
        document.insert("q", date(value));
    }
    if let Some(value) = run.finished_at {
        document.insert("f", date(value));
    }
    if let Some(value) = run.duration_ms {
        document.insert(
            "d",
            i64::try_from(value).map_err(|_| SignalStoreError::InvalidData)?,
        );
    }
    if let Some(value) = run.release_id {
        document.insert("l", binary(value.as_bytes()));
    }
    if let Some(value) = run.timeout_at {
        document.insert("t", date(value));
    }
    if let Some(value) = run.delete_at {
        document.insert("x", date(value));
    }
    if let Some(value) = run.http_status {
        document.insert("a", i32::from(value));
    }
    if let Some(value) = run.uptime_failure {
        document.insert("b", value.as_str());
    }
    Ok(document)
}

fn terminal_fields(run: &MonitorRun) -> Result<Document, SignalStoreError> {
    let finished = run.finished_at.ok_or(SignalStoreError::InvalidData)?;
    let mut fields = doc! {
        "s": status_tag(run.status),
        "f": date(finished),
        "r": date(run.received_at),
    };
    if let Some(value) = run.duration_ms {
        fields.insert(
            "d",
            i64::try_from(value).map_err(|_| SignalStoreError::InvalidData)?,
        );
    }
    if let Some(value) = run.delete_at {
        fields.insert("x", date(value));
    }
    Ok(fields)
}

fn decode_run(document: &Document) -> Result<MonitorRun, SignalStoreError> {
    let run = MonitorRun {
        id: MonitorRunId::from_bytes(id16(document, "_id")?),
        project_id: project_id(document)?,
        monitor_id: MonitorId::from_bytes(id16(document, "m")?),
        check_in_id: optional_id16(document, "c")?,
        status: parse_status(
            document
                .get_i32("s")
                .map_err(|_| SignalStoreError::InvalidData)?,
        )?,
        source: match document
            .get_i32("g")
            .map_err(|_| SignalStoreError::InvalidData)?
        {
            0 => MonitorRunSource::Sdk,
            1 => MonitorRunSource::Scheduler,
            _ => return Err(SignalStoreError::InvalidData),
        },
        scheduled_for: optional_timestamp(document, "q")?,
        started_at: timestamp(document, "i")?,
        finished_at: optional_timestamp(document, "f")?,
        duration_ms: document
            .get_i64("d")
            .ok()
            .map(|value| u64::try_from(value).map_err(|_| SignalStoreError::InvalidData))
            .transpose()?,
        received_at: timestamp(document, "r")?,
        release_id: optional_id16(document, "l")?.map(ReleaseId::from_bytes),
        timeout_at: optional_timestamp(document, "t")?,
        delete_at: optional_timestamp(document, "x")?,
        http_status: document
            .get_i32("a")
            .ok()
            .map(|value| u16::try_from(value).map_err(|_| SignalStoreError::InvalidData))
            .transpose()?,
        uptime_failure: document
            .get_str("b")
            .ok()
            .map(UptimeFailure::parse)
            .transpose()
            .map_err(|_| SignalStoreError::InvalidData)?,
    };
    run.validate().map_err(|_| SignalStoreError::InvalidData)?;
    Ok(run)
}

pub fn monitor_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "k", "l", "n", "e", "v", "a", "w", "r", "c", "f", "d", "i", "o"],
        "additionalProperties": false,
        "properties": {
            "_id": bin16_schema(), "p": { "bsonType": "int", "minimum": 1 },
            "k": { "bsonType": "int", "enum": [0, 1] },
            "l": { "bsonType": "string", "maxLength": 64 },
            "n": { "bsonType": "string", "maxLength": 128 },
            "e": bin16_schema(), "v": { "bsonType": "string", "maxLength": 64 },
            "a": { "bsonType": "bool" }, "w": { "bsonType": "bool" },
            "r": { "bsonType": "long", "minimum": 1 },
            "c": { "bsonType": "object" }, "b": { "bsonType": "object" },
            "f": { "bsonType": "date" }, "d": { "bsonType": "date" },
            "u": bin16_schema(), "s": { "bsonType": "int", "minimum": 0, "maximum": 4 },
            "h": { "bsonType": "date" }, "i": { "bsonType": "date" }, "o": { "bsonType": "date" },
            "y": { "bsonType": "date" },
        }
    }}
}

pub fn monitor_run_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "m", "s", "g", "i", "r"],
        "additionalProperties": false,
        "properties": {
            "_id": bin16_schema(), "p": { "bsonType": "int", "minimum": 1 },
            "m": bin16_schema(), "c": bin16_schema(),
            "s": { "bsonType": "int", "minimum": 0, "maximum": 4 },
            "g": { "bsonType": "int", "minimum": 0, "maximum": 1 },
            "q": { "bsonType": "date" }, "i": { "bsonType": "date" },
            "f": { "bsonType": "date" }, "d": { "bsonType": "long", "minimum": 0 },
            "r": { "bsonType": "date" }, "l": bin16_schema(),
            "t": { "bsonType": "date" }, "x": { "bsonType": "date" },
            "z": { "bsonType": "date" },
            "a": { "bsonType": "int", "minimum": 100, "maximum": 599 },
            "b": { "bsonType": "string" },
        }
    }}
}

pub fn monitor_indexes() -> Vec<IndexModel> {
    vec![
        named_index(
            doc! { "p": 1, "k": 1, "l": 1, "e": 1 },
            "monitor_identity",
            true,
            None,
        ),
        named_index(
            doc! { "p": 1, "o": -1, "_id": -1 },
            "monitor_project_list",
            false,
            None,
        ),
        named_index(
            doc! { "d": 1, "_id": 1 },
            "monitor_due",
            false,
            Some(doc! { "a": true }),
        ),
    ]
}

pub fn monitor_run_indexes() -> Vec<IndexModel> {
    vec![
        named_index(
            doc! { "p": 1, "m": 1, "i": -1, "_id": -1 },
            "monitor_run_history",
            false,
            None,
        ),
        named_index(
            doc! { "r": 1, "_id": 1 },
            "monitor_run_alert",
            false,
            Some(doc! {
                "$or": [
                    { "s": { "$in": [
                        status_tag(MonitorRunStatus::Error),
                        status_tag(MonitorRunStatus::Timeout),
                        status_tag(MonitorRunStatus::Missed),
                    ]}},
                    { "s": status_tag(MonitorRunStatus::Success), "a": { "$exists": true } },
                ],
                "z": Bson::Null,
            }),
        ),
        named_index(
            doc! { "t": 1, "_id": 1 },
            "monitor_run_timeout",
            false,
            Some(doc! { "s": status_tag(MonitorRunStatus::InProgress) }),
        ),
        IndexModel::builder()
            .keys(doc! { "x": 1 })
            .options(
                IndexOptions::builder()
                    .name("monitor_run_ttl".to_owned())
                    .expire_after(Duration::ZERO)
                    .build(),
            )
            .build(),
    ]
}

pub fn monitor_index_names() -> std::collections::BTreeSet<&'static str> {
    std::collections::BTreeSet::from([
        "_id_",
        "monitor_identity",
        "monitor_project_list",
        "monitor_due",
    ])
}

pub fn monitor_run_index_names() -> std::collections::BTreeSet<&'static str> {
    std::collections::BTreeSet::from([
        "_id_",
        "monitor_run_history",
        "monitor_run_timeout",
        "monitor_run_alert",
        "monitor_run_ttl",
    ])
}

fn named_index(keys: Document, name: &str, unique: bool, partial: Option<Document>) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .unique(unique.then_some(true))
                .partial_filter_expression(partial)
                .build(),
        )
        .build()
}

fn status_tag(value: MonitorRunStatus) -> i32 {
    match value {
        MonitorRunStatus::InProgress => 0,
        MonitorRunStatus::Success => 1,
        MonitorRunStatus::Error => 2,
        MonitorRunStatus::Timeout => 3,
        MonitorRunStatus::Missed => 4,
    }
}

fn parse_status(value: i32) -> Result<MonitorRunStatus, SignalStoreError> {
    match value {
        0 => Ok(MonitorRunStatus::InProgress),
        1 => Ok(MonitorRunStatus::Success),
        2 => Ok(MonitorRunStatus::Error),
        3 => Ok(MonitorRunStatus::Timeout),
        4 => Ok(MonitorRunStatus::Missed),
        _ => Err(SignalStoreError::InvalidData),
    }
}

fn source_tag(value: MonitorRunSource) -> i32 {
    match value {
        MonitorRunSource::Sdk => 0,
        MonitorRunSource::Scheduler => 1,
    }
}

fn add_days(value: Timestamp, days: u32) -> Result<Timestamp, SignalStoreError> {
    Timestamp::from_unix_millis(
        value
            .unix_millis()
            .checked_add(i64::from(days) * DAY_MILLIS)
            .ok_or(SignalStoreError::InvalidData)?,
    )
    .map_err(|_| SignalStoreError::InvalidData)
}

fn date(value: Timestamp) -> DateTime {
    DateTime::from_millis(value.unix_millis())
}

fn timestamp(document: &Document, key: &str) -> Result<Timestamp, SignalStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(key)
            .map_err(|_| SignalStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| SignalStoreError::InvalidData)
}

fn optional_timestamp(
    document: &Document,
    key: &str,
) -> Result<Option<Timestamp>, SignalStoreError> {
    document
        .get_datetime(key)
        .ok()
        .map(|value| {
            Timestamp::from_unix_millis(value.timestamp_millis())
                .map_err(|_| SignalStoreError::InvalidData)
        })
        .transpose()
}

fn binary(bytes: [u8; 16]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn id16(document: &Document, key: &str) -> Result<[u8; 16], SignalStoreError> {
    let bytes = document
        .get_binary_generic(key)
        .map_err(|_| SignalStoreError::InvalidData)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| SignalStoreError::InvalidData)
}

fn optional_id16(document: &Document, key: &str) -> Result<Option<[u8; 16]>, SignalStoreError> {
    document
        .get_binary_generic(key)
        .ok()
        .map(|bytes| {
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| SignalStoreError::InvalidData)
        })
        .transpose()
}

fn project_id(document: &Document) -> Result<ProjectId, SignalStoreError> {
    ProjectId::new(
        document
            .get_i32("p")
            .map_err(|_| SignalStoreError::InvalidData)?,
    )
    .map_err(|_| SignalStoreError::InvalidData)
}

fn text(document: &Document, key: &str) -> Result<Box<str>, SignalStoreError> {
    document
        .get_str(key)
        .map(Box::<str>::from)
        .map_err(|_| SignalStoreError::InvalidData)
}

fn bin16_schema() -> Document {
    doc! { "bsonType": "binData" }
}

fn unavailable<T>(_: T) -> SignalStoreError {
    SignalStoreError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant(value: i64) -> Timestamp {
        Timestamp::from_unix_millis(value).unwrap()
    }

    fn fixture() -> (MonitorDefinition, MonitorRun) {
        let project_id = ProjectId::new(42).unwrap();
        let monitor_id = MonitorId::derive(project_id, "nightly-backup", "production");
        let monitor = MonitorDefinition {
            id: monitor_id,
            project_id,
            slug: "nightly-backup".into(),
            name: "Nightly backup".into(),
            environment_id: EnvironmentId::from_bytes([2; 16]),
            environment: "production".into(),
            enabled: true,
            managed_by_web: false,
            revision: 1,
            config: MonitorConfig {
                schedule: MonitorSchedule::crontab("0 2 * * *").unwrap(),
                checkin_margin_seconds: 60,
                max_runtime_seconds: 900,
            },
            uptime: None,
            next_expected_at: instant(1_700_000_100_000),
            last_run_id: None,
            last_status: None,
            last_check_in_at: None,
            created_at: instant(1_700_000_000_000),
            updated_at: instant(1_700_000_000_000),
        };
        let run = MonitorRun {
            id: MonitorRunId::sdk(monitor_id, [3; 16]),
            project_id,
            monitor_id,
            check_in_id: Some([3; 16]),
            status: MonitorRunStatus::Success,
            source: MonitorRunSource::Sdk,
            scheduled_for: None,
            started_at: instant(1_700_000_000_000),
            finished_at: Some(instant(1_700_000_001_000)),
            duration_ms: Some(1_000),
            received_at: instant(1_700_000_001_000),
            release_id: None,
            timeout_at: None,
            delete_at: Some(instant(1_707_776_001_000)),
            http_status: None,
            uptime_failure: None,
        };
        (monitor, run)
    }

    #[test]
    fn compact_documents_round_trip_with_bounded_size() {
        let (monitor, run) = fixture();
        let mut monitor_document = encode_monitor(&monitor).unwrap();
        monitor_document.insert("_id", binary(monitor.id.as_bytes()));
        let run_document = encode_run(&run).unwrap();

        assert_eq!(decode_monitor(&monitor_document).unwrap(), monitor);
        assert_eq!(decode_run(&run_document).unwrap(), run);
        assert!(mongodb::bson::to_vec(&monitor_document).unwrap().len() < 400);
        assert!(mongodb::bson::to_vec(&run_document).unwrap().len() < 256);
        assert!(!run_document.contains_key("raw"));
        assert!(!run_document.contains_key("payload"));
    }
}
