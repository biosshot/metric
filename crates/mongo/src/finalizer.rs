use std::{collections::BTreeMap, time::Instant};

use metric_domain::{
    EventKey, OrganizationId, ProjectId, Timestamp,
    archive::ArchiveSegmentId,
    event::{EventLevel, EventPlatform},
    finalization::{
        FinalizationPolicy, FinalizeBatch, FinalizeEvent, FinalizeResult, ProcessedEventPayload,
        SearchToken, derive_environment_id, derive_hour_bucket_id, derive_release_id, hour_start,
    },
    grouping::IssueId,
};
use metric_ports::{FinalizationStore, FinalizationStoreError, IssueStoreError, PortFuture};
use futures_util::TryStreamExt;
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    error::{Error as MongoError, ErrorKind, WriteFailure},
    options::{IndexOptions, ReturnDocument, UpdateOneModel},
};

use crate::{
    event::{self, EventCodecConfig},
    issue::{IssueCodecConfig, MongoIssueStore},
};

const DUPLICATE_KEY_CODE: i32 = 11000;
const DAY_MILLIS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Clone)]
pub struct MongoFinalizationStore {
    database: Database,
    event_codec: EventCodecConfig,
    issues: MongoIssueStore,
}

impl MongoFinalizationStore {
    #[must_use]
    pub fn from_database(
        database: Database,
        event_codec: EventCodecConfig,
        issue_codec: IssueCodecConfig,
    ) -> Self {
        Self {
            issues: MongoIssueStore::from_database(database.clone(), issue_codec),
            database,
            event_codec,
        }
    }

    async fn finalize_inner(
        &self,
        batch: FinalizeBatch,
        policy: FinalizationPolicy,
    ) -> Result<FinalizeResult, FinalizationStoreError> {
        validate_policy(policy)?;
        let requested = batch.events.len();
        if requested == 0 {
            return Err(FinalizationStoreError::InvalidData);
        }
        let pending = self.pending_events(batch.events).await?;
        if pending.is_empty() {
            return Ok(FinalizeResult {
                requested,
                pending: 0,
                finalized: 0,
                skipped_completed: requested,
            });
        }

        self.update_issues(&pending).await?;
        self.update_hourly(&pending, policy).await?;
        self.update_catalogs(&pending, policy).await?;
        let finalized = self.finalize_events(&pending, policy).await?;
        Ok(FinalizeResult {
            requested,
            pending: pending.len(),
            finalized,
            skipped_completed: requested.saturating_sub(pending.len())
                + pending.len().saturating_sub(finalized),
        })
    }

    async fn pending_events(
        &self,
        events: Vec<FinalizeEvent>,
    ) -> Result<Vec<FinalizeEvent>, FinalizationStoreError> {
        let mut by_key = BTreeMap::new();
        for event in events {
            validate_event(&event, self.event_codec)?;
            if by_key.insert(event.key(), event).is_some() {
                return Err(FinalizationStoreError::InvalidData);
            }
        }
        let ids = by_key
            .keys()
            .map(|key| Bson::Binary(binary(key.as_bytes())))
            .collect::<Vec<_>>();
        let mut cursor = self
            .database
            .collection::<Document>("error_events")
            .find(doc! { "_id": { "$in": ids }, "q.s": 0_i32 })
            .projection(doc! { "_id": 1, "p": 1 })
            .await
            .map_err(|_| FinalizationStoreError::Unavailable)?;
        let mut pending = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| FinalizationStoreError::Unavailable)?
        {
            let key = EventKey::from_bytes(fixed_binary::<20>(&document, "_id")?)
                .map_err(|_| FinalizationStoreError::InvalidData)?;
            let event = by_key
                .remove(&key)
                .ok_or(FinalizationStoreError::InvalidData)?;
            if document.get_i32("p") != Ok(event.project_id.get()) {
                return Err(FinalizationStoreError::InvalidData);
            }
            pending.push(event);
        }
        pending.sort_by_key(FinalizeEvent::key);
        Ok(pending)
    }

    async fn update_issues(&self, events: &[FinalizeEvent]) -> Result<(), FinalizationStoreError> {
        let mut groups = BTreeMap::<(i32, [u8; 16]), Vec<_>>::new();
        for event in events {
            groups
                .entry((event.project_id.get(), event.issue.issue_id.as_bytes()))
                .or_default()
                .push(event.issue.clone());
        }
        for occurrences in groups.values() {
            self.issues
                .apply_occurrence_batch(occurrences)
                .await
                .map_err(map_issue_error)?;
        }
        Ok(())
    }

    async fn update_hourly(
        &self,
        events: &[FinalizeEvent],
        policy: FinalizationPolicy,
    ) -> Result<(), FinalizationStoreError> {
        let retention = duration_millis(policy.hourly_retention)?;
        let mut groups = BTreeMap::<(i32, [u8; 16], i64), i64>::new();
        for event in events {
            let bucket = hour_start(event.occurred_at).unix_millis();
            *groups
                .entry((
                    event.project_id.get(),
                    event.issue.issue_id.as_bytes(),
                    bucket,
                ))
                .or_default() += 1;
        }
        let collection = self.database.collection::<Document>("issue_stats_hourly");
        for ((project, issue_bytes, bucket_millis), count) in groups {
            let project_id =
                ProjectId::new(project).map_err(|_| FinalizationStoreError::InvalidData)?;
            let issue_id = IssueId::from_bytes(issue_bytes);
            let bucket = Timestamp::from_unix_millis(bucket_millis)
                .map_err(|_| FinalizationStoreError::InvalidData)?;
            let expire = checked_add_millis(bucket, retention)?;
            let id = derive_hour_bucket_id(project_id, issue_id, bucket);
            let result = collection
                .update_one(
                    doc! {
                        "_id": binary(id.as_bytes()),
                        "project_id": project,
                        "issue_id": binary(issue_bytes),
                        "bucket_start": date(bucket),
                    },
                    doc! {
                        "$setOnInsert": { "expire_at": date(expire) },
                        "$inc": { "occurrence_count": count },
                    },
                )
                .upsert(true)
                .await;
            match result {
                Ok(_) => {}
                Err(error) if duplicate_write(&error) => {
                    return Err(FinalizationStoreError::IdentityCollision);
                }
                Err(_) => return Err(FinalizationStoreError::Unavailable),
            }
        }
        Ok(())
    }

    async fn update_catalogs(
        &self,
        events: &[FinalizeEvent],
        policy: FinalizationPolicy,
    ) -> Result<(), FinalizationStoreError> {
        let projects = self.load_projects(events).await?;
        let mut releases = BTreeMap::<(i32, Box<str>), Vec<&FinalizeEvent>>::new();
        let mut environments = BTreeMap::<(i32, Box<str>), Vec<&FinalizeEvent>>::new();
        for event in events {
            if let Some(release) = &event.issue.release {
                releases
                    .entry((event.project_id.get(), release.as_str().into()))
                    .or_default()
                    .push(event);
            }
            if let Some(environment) = &event.environment {
                environments
                    .entry((event.project_id.get(), environment.clone()))
                    .or_default()
                    .push(event);
            }
        }
        for ((project, version), values) in releases {
            let organization = *projects
                .get(&project)
                .ok_or(FinalizationStoreError::InvalidData)?;
            self.materialize_release(
                ProjectId::new(project).map_err(|_| FinalizationStoreError::InvalidData)?,
                organization,
                &version,
                &values,
                policy.max_implicit_releases_per_project_day,
            )
            .await?;
        }
        for ((project, name), values) in environments {
            self.materialize_environment(
                ProjectId::new(project).map_err(|_| FinalizationStoreError::InvalidData)?,
                &name,
                &values,
                policy.max_implicit_environments_per_project,
            )
            .await?;
        }
        Ok(())
    }

    async fn load_projects(
        &self,
        events: &[FinalizeEvent],
    ) -> Result<BTreeMap<i32, OrganizationId>, FinalizationStoreError> {
        let ids = events
            .iter()
            .map(|event| event.project_id.get())
            .collect::<std::collections::BTreeSet<_>>();
        let mut cursor = self
            .database
            .collection::<Document>("projects")
            .find(doc! { "_id": { "$in": ids.iter().copied().collect::<Vec<_>>() } })
            .projection(doc! { "_id": 1, "organization_id": 1 })
            .await
            .map_err(|_| FinalizationStoreError::Unavailable)?;
        let mut projects = BTreeMap::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| FinalizationStoreError::Unavailable)?
        {
            let project = document
                .get_i32("_id")
                .map_err(|_| FinalizationStoreError::InvalidData)?;
            let organization = OrganizationId::new(
                u64::try_from(
                    document
                        .get_i64("organization_id")
                        .map_err(|_| FinalizationStoreError::InvalidData)?,
                )
                .map_err(|_| FinalizationStoreError::InvalidData)?,
            )
            .map_err(|_| FinalizationStoreError::InvalidData)?;
            projects.insert(project, organization);
        }
        if projects.len() != ids.len() {
            return Err(FinalizationStoreError::InvalidData);
        }
        Ok(projects)
    }

    async fn materialize_release(
        &self,
        project_id: ProjectId,
        organization_id: OrganizationId,
        version: &str,
        events: &[&FinalizeEvent],
        limit: u32,
    ) -> Result<(), FinalizationStoreError> {
        let id = derive_release_id(organization_id, version);
        let collection = self.database.collection::<Document>("releases");
        let exists = collection
            .find_one(doc! { "_id": binary(id.as_bytes()) })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|_| FinalizationStoreError::Unavailable)?
            .is_some();
        let day = events
            .iter()
            .map(|event| receipt_day(event.received_at))
            .min()
            .ok_or(FinalizationStoreError::InvalidData)?;
        if !exists && !self.reserve_release(project_id, day, limit).await? {
            metrics::counter!("metric_catalog_admission_total", "catalog" => "release", "outcome" => "limited").increment(1);
            return Ok(());
        }
        let first = events
            .iter()
            .min_by_key(|event| (event.occurred_at, event.key().as_bytes()))
            .ok_or(FinalizationStoreError::InvalidData)?;
        let latest = events
            .iter()
            .max_by_key(|event| (event.occurred_at, event.key().as_bytes()))
            .ok_or(FinalizationStoreError::InvalidData)?;
        let update = release_pipeline(project_id, organization_id, version, first, latest);
        let result = collection
            .update_one(
                doc! {
                    "_id": binary(id.as_bytes()),
                    "organization_id": i64::try_from(organization_id.get()).map_err(|_| FinalizationStoreError::InvalidData)?,
                    "version": version,
                },
                update,
            )
            .upsert(true)
            .await;
        map_catalog_write(result)
    }

    async fn materialize_environment(
        &self,
        project_id: ProjectId,
        name: &str,
        events: &[&FinalizeEvent],
        limit: u32,
    ) -> Result<(), FinalizationStoreError> {
        let id = derive_environment_id(project_id, name);
        let collection = self.database.collection::<Document>("environments");
        let exists = collection
            .find_one(doc! { "_id": binary(id.as_bytes()) })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|_| FinalizationStoreError::Unavailable)?
            .is_some();
        if !exists && !self.reserve_environment(project_id, limit).await? {
            metrics::counter!("metric_catalog_admission_total", "catalog" => "environment", "outcome" => "limited").increment(1);
            return Ok(());
        }
        let first = events
            .iter()
            .map(|event| event.occurred_at)
            .min()
            .ok_or(FinalizationStoreError::InvalidData)?;
        let latest = events
            .iter()
            .map(|event| event.occurred_at)
            .max()
            .ok_or(FinalizationStoreError::InvalidData)?;
        let result = collection
            .update_one(
                doc! {
                    "_id": binary(id.as_bytes()),
                    "project_id": project_id.get(),
                    "name": name,
                },
                doc! {
                    "$setOnInsert": {
                        "hidden": false,
                        "source": "event",
                    },
                    "$min": { "first_seen": date(first) },
                    "$max": { "last_seen": date(latest) },
                },
            )
            .upsert(true)
            .await;
        map_catalog_write(result)
    }

    async fn reserve_release(
        &self,
        project_id: ProjectId,
        day: Timestamp,
        limit: u32,
    ) -> Result<bool, FinalizationStoreError> {
        let updated = self
            .database
            .collection::<Document>("projects")
            .find_one_and_update(
                doc! {
                    "_id": project_id.get(),
                    "$expr": { "$or": [
                        { "$ne": ["$catalog_usage.rd", date(day)] },
                        { "$lt": [{ "$ifNull": ["$catalog_usage.rc", 0_i32] }, i64::from(limit)] },
                    ] },
                },
                vec![doc! { "$set": {
                    "catalog_usage.rd": date(day),
                    "catalog_usage.rc": { "$cond": [
                        { "$eq": ["$catalog_usage.rd", date(day)] },
                        { "$add": [{ "$ifNull": ["$catalog_usage.rc", 0_i32] }, 1_i32] },
                        1_i32,
                    ] },
                } }],
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(|_| FinalizationStoreError::Unavailable)?;
        Ok(updated.is_some())
    }

    async fn reserve_environment(
        &self,
        project_id: ProjectId,
        limit: u32,
    ) -> Result<bool, FinalizationStoreError> {
        let updated = self
            .database
            .collection::<Document>("projects")
            .find_one_and_update(
                doc! {
                    "_id": project_id.get(),
                    "$expr": { "$lt": [
                        { "$ifNull": ["$catalog_usage.ec", 0_i32] },
                        i64::from(limit),
                    ] },
                },
                doc! { "$inc": { "catalog_usage.ec": 1_i32 } },
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(|_| FinalizationStoreError::Unavailable)?;
        Ok(updated.is_some())
    }

    async fn finalize_events(
        &self,
        events: &[FinalizeEvent],
        policy: FinalizationPolicy,
    ) -> Result<usize, FinalizationStoreError> {
        let retention = duration_millis(policy.event_retention)?;
        let collection = self.database.collection::<Document>("error_events");
        let namespace = collection.namespace();
        let mut models = Vec::with_capacity(events.len());
        for event in events {
            let expire = checked_add_millis(event.received_at, retention)?;
            let body = event::encode_body(event.payload.as_bytes(), self.event_codec)
                .map_err(|_| FinalizationStoreError::InvalidData)?;
            let mut set = doc! {
                "u": binary(event.issue.issue_id.as_bytes()),
                "o": date(event.occurred_at),
                "a": platform_code(&event.platform),
                "b": Binary { subtype: BinarySubtype::Generic, bytes: body },
            };
            let correlation = event_correlation(event.payload.as_bytes());
            if let Some((trace_id, span_id)) = correlation {
                set.insert("g", binary(trace_id));
                set.insert("n", binary(span_id));
            }
            if policy.archive_events {
                set.insert("h", date(expire));
            } else {
                set.insert("x", date(expire));
            }
            if let Some(level) = level_code(event.level) {
                set.insert("l", level);
            }
            if !event.search_tokens.is_empty() {
                set.insert(
                    "k",
                    event
                        .search_tokens
                        .iter()
                        .map(|token| Bson::Int64(token.stored()))
                        .collect::<Vec<_>>(),
                );
            }
            let mut unset = doc! { "q": "" };
            if policy.archive_events {
                unset.insert("x", "");
            } else {
                unset.insert("h", "");
            }
            unset.insert("z", "");
            if level_code(event.level).is_none() {
                unset.insert("l", "");
            }
            if event.search_tokens.is_empty() {
                unset.insert("k", "");
            }
            if correlation.is_none() {
                unset.insert("g", "");
                unset.insert("n", "");
            }
            models.push(
                UpdateOneModel::builder()
                    .namespace(namespace.clone())
                    .filter(doc! {
                        "_id": binary(event.key().as_bytes()),
                        "p": event.project_id.get(),
                        "q.s": 0_i32,
                    })
                    .update(doc! { "$set": set, "$unset": unset })
                    .build(),
            );
        }
        let result = self
            .database
            .client()
            .bulk_write(models)
            .ordered(false)
            .await
            .map_err(|_| FinalizationStoreError::Unavailable)?;
        usize::try_from(result.modified_count).map_err(|_| FinalizationStoreError::InvalidData)
    }
}

fn event_correlation(payload: &[u8]) -> Option<([u8; 16], [u8; 8])> {
    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let trace = value.pointer("/contexts/trace")?;
    let trace_id = trace.get("trace_id")?.as_str()?;
    let span_id = trace.get("span_id")?.as_str()?;
    let mut trace_bytes = [0_u8; 16];
    let mut span_bytes = [0_u8; 8];
    if trace_id.len() != 32
        || span_id.len() != 16
        || hex::decode_to_slice(trace_id, &mut trace_bytes).is_err()
        || hex::decode_to_slice(span_id, &mut span_bytes).is_err()
        || trace_bytes == [0; 16]
        || span_bytes == [0; 8]
    {
        return None;
    }
    Some((trace_bytes, span_bytes))
}

#[cfg(test)]
mod correlation_tests {
    use super::*;

    #[test]
    fn valid_error_trace_context_gets_a_compact_projection() {
        let payload = br#"{"contexts":{"trace":{"trace_id":"0123456789abcdef0123456789abcdef","span_id":"0123456789abcdef"}}}"#;
        let (trace_id, span_id) = event_correlation(payload).unwrap();
        assert_eq!(hex::encode(trace_id), "0123456789abcdef0123456789abcdef");
        assert_eq!(hex::encode(span_id), "0123456789abcdef");
        assert!(event_correlation(br#"{"contexts":{"trace":{"trace_id":"bad"}}}"#).is_none());
    }
}

impl FinalizationStore for MongoFinalizationStore {
    fn finalize(
        &self,
        batch: FinalizeBatch,
        policy: FinalizationPolicy,
    ) -> PortFuture<'_, Result<FinalizeResult, FinalizationStoreError>> {
        Box::pin(async move {
            let started = Instant::now();
            let result = self.finalize_inner(batch, policy).await;
            let outcome = match result {
                Ok(_) => "ok",
                Err(FinalizationStoreError::InvalidData) => "invalid_data",
                Err(FinalizationStoreError::IdentityCollision) => "collision",
                Err(FinalizationStoreError::Unavailable) => "unavailable",
            };
            metrics::histogram!(
                "metric_mongodb_operation_duration_seconds",
                "operation" => "finalize_batch",
                "outcome" => outcome
            )
            .record(started.elapsed().as_secs_f64());
            result
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFinalizedEvent {
    pub key: EventKey,
    pub issue_id: IssueId,
    pub expire_at: Option<Timestamp>,
    pub archive_due: Option<Timestamp>,
    pub archive_segment: Option<ArchiveSegmentId>,
    pub search_tokens: Vec<SearchToken>,
    pub payload: ProcessedEventPayload,
}

pub fn decode_finalized_event(
    document: &Document,
    config: EventCodecConfig,
) -> Result<DecodedFinalizedEvent, FinalizationStoreError> {
    if document.contains_key("q") {
        return Err(FinalizationStoreError::InvalidData);
    }
    let key = EventKey::from_bytes(fixed_binary::<20>(document, "_id")?)
        .map_err(|_| FinalizationStoreError::InvalidData)?;
    let issue_id = IssueId::from_bytes(fixed_binary::<16>(document, "u")?);
    let expire_at = optional_timestamp(document, "x")?;
    let archive_due = optional_timestamp(document, "h")?;
    let archive_segment = match document.get("z") {
        None => None,
        Some(Bson::Binary(value)) if value.subtype == BinarySubtype::Generic => {
            Some(ArchiveSegmentId::from_bytes(
                value
                    .bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| FinalizationStoreError::InvalidData)?,
            ))
        }
        Some(_) => return Err(FinalizationStoreError::InvalidData),
    };
    let search_tokens = match document.get("k") {
        None => Vec::new(),
        Some(Bson::Array(values)) if values.len() <= 16 => values
            .iter()
            .map(|value| match value {
                Bson::Int64(value) => Ok(SearchToken::from_stored(*value)),
                _ => Err(FinalizationStoreError::InvalidData),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(FinalizationStoreError::InvalidData),
    };
    let body = document
        .get_binary_generic("b")
        .map_err(|_| FinalizationStoreError::InvalidData)?;
    let payload = event::decode_body(body, config.max_decoded_body_bytes)
        .map_err(|_| FinalizationStoreError::InvalidData)?;
    Ok(DecodedFinalizedEvent {
        key,
        issue_id,
        expire_at,
        archive_due,
        archive_segment,
        search_tokens,
        payload: ProcessedEventPayload::new(payload),
    })
}

fn release_pipeline(
    project_id: ProjectId,
    organization_id: OrganizationId,
    version: &str,
    first: &FinalizeEvent,
    latest: &FinalizeEvent,
) -> Vec<Document> {
    let first_key = Bson::Binary(binary(first.key().as_bytes()));
    let latest_key = Bson::Binary(binary(latest.key().as_bytes()));
    let first_at = Bson::DateTime(date(first.occurred_at));
    let latest_at = Bson::DateTime(date(latest.occurred_at));
    vec![
        doc! { "$set": {
            "_fk_first": { "$or": [
                { "$eq": [{ "$type": "$first_seen" }, "missing"] },
                { "$lt": [first_at.clone(), "$first_seen"] },
                { "$and": [
                    { "$eq": [first_at.clone(), "$first_seen"] },
                    { "$lt": [first_key.clone(), "$first_event_id"] },
                ] },
            ] },
            "_fk_latest": { "$or": [
                { "$eq": [{ "$type": "$last_seen" }, "missing"] },
                { "$gt": [latest_at.clone(), "$last_seen"] },
                { "$and": [
                    { "$eq": [latest_at.clone(), "$last_seen"] },
                    { "$gt": [latest_key.clone(), "$latest_event_id"] },
                ] },
            ] },
        } },
        doc! { "$set": {
            "organization_id": { "$ifNull": ["$organization_id", i64::try_from(organization_id.get()).expect("organization ID fits i64")] },
            "version": { "$ifNull": ["$version", version] },
            "status": { "$ifNull": ["$status", "open"] },
            "project_ids": { "$setUnion": [{ "$ifNull": ["$project_ids", []] }, [project_id.get()]] },
            "first_seen": { "$cond": ["$_fk_first", first_at, "$first_seen"] },
            "first_event_id": { "$cond": ["$_fk_first", first_key, "$first_event_id"] },
            "last_seen": { "$cond": ["$_fk_latest", latest_at, "$last_seen"] },
            "latest_event_id": { "$cond": ["$_fk_latest", latest_key, "$latest_event_id"] },
            "created_at": { "$ifNull": ["$created_at", date(first.received_at)] },
            "source": { "$ifNull": ["$source", "event"] },
        } },
        doc! { "$unset": ["_fk_first", "_fk_latest"] },
    ]
}

fn validate_event(
    event: &FinalizeEvent,
    config: EventCodecConfig,
) -> Result<(), FinalizationStoreError> {
    let consistent = event.issue.project_id == event.project_id
        && event.issue.event_id == event.event_id
        && event.issue.received_at == event.received_at
        && event.issue.occurred_at == event.occurred_at
        && event.search_tokens.len() <= 16;
    if !consistent {
        return Err(FinalizationStoreError::InvalidData);
    }
    let value: serde_json::Value = serde_json::from_slice(event.payload.as_bytes())
        .map_err(|_| FinalizationStoreError::InvalidData)?;
    let canonical = serde_json::to_vec(&value).map_err(|_| FinalizationStoreError::InvalidData)?;
    if canonical != event.payload.as_bytes() || canonical.len() > config.max_decoded_body_bytes {
        return Err(FinalizationStoreError::InvalidData);
    }
    let mut tokens = std::collections::BTreeSet::new();
    if event
        .search_tokens
        .iter()
        .any(|token| !tokens.insert(*token))
    {
        return Err(FinalizationStoreError::InvalidData);
    }
    Ok(())
}

fn validate_policy(policy: FinalizationPolicy) -> Result<(), FinalizationStoreError> {
    if policy.event_retention.is_zero()
        || policy.hourly_retention.is_zero()
        || policy.max_implicit_releases_per_project_day == 0
        || policy.max_implicit_environments_per_project == 0
    {
        return Err(FinalizationStoreError::InvalidData);
    }
    duration_millis(policy.event_retention)?;
    duration_millis(policy.hourly_retention)?;
    Ok(())
}

fn duration_millis(duration: std::time::Duration) -> Result<i64, FinalizationStoreError> {
    i64::try_from(duration.as_millis()).map_err(|_| FinalizationStoreError::InvalidData)
}

fn checked_add_millis(
    timestamp: Timestamp,
    millis: i64,
) -> Result<Timestamp, FinalizationStoreError> {
    Timestamp::from_unix_millis(
        timestamp
            .unix_millis()
            .checked_add(millis)
            .ok_or(FinalizationStoreError::InvalidData)?,
    )
    .map_err(|_| FinalizationStoreError::InvalidData)
}

fn optional_timestamp(
    document: &Document,
    field: &str,
) -> Result<Option<Timestamp>, FinalizationStoreError> {
    match document.get(field) {
        None => Ok(None),
        Some(Bson::DateTime(value)) => Timestamp::from_unix_millis(value.timestamp_millis())
            .map(Some)
            .map_err(|_| FinalizationStoreError::InvalidData),
        Some(_) => Err(FinalizationStoreError::InvalidData),
    }
}

fn receipt_day(timestamp: Timestamp) -> Timestamp {
    Timestamp::from_unix_millis(timestamp.unix_millis().div_euclid(DAY_MILLIS) * DAY_MILLIS)
        .expect("receipt-day floor stays in timestamp range")
}

fn platform_code(platform: &EventPlatform) -> i32 {
    match platform {
        EventPlatform::Python => 1,
        EventPlatform::JavaScript | EventPlatform::Node => 2,
        EventPlatform::Native | EventPlatform::Cocoa => 3,
        EventPlatform::Java => 4,
        EventPlatform::Php => 5,
        EventPlatform::Ruby => 6,
        EventPlatform::DotNet => 7,
        EventPlatform::Go => 8,
        EventPlatform::Rust => 9,
        EventPlatform::Other | EventPlatform::Dart | EventPlatform::Custom(_) => 0,
    }
}

fn level_code(level: EventLevel) -> Option<i32> {
    match level {
        EventLevel::Error => None,
        EventLevel::Debug => Some(1),
        EventLevel::Info => Some(2),
        EventLevel::Warning => Some(3),
        EventLevel::Fatal => Some(4),
    }
}

fn map_issue_error(error: IssueStoreError) -> FinalizationStoreError {
    match error {
        IssueStoreError::IdentityCollision => FinalizationStoreError::IdentityCollision,
        IssueStoreError::NotFound | IssueStoreError::InvalidData => {
            FinalizationStoreError::InvalidData
        }
        IssueStoreError::Unavailable => FinalizationStoreError::Unavailable,
    }
}

fn map_catalog_write(
    result: Result<mongodb::results::UpdateResult, MongoError>,
) -> Result<(), FinalizationStoreError> {
    match result {
        Ok(_) => Ok(()),
        Err(error) if duplicate_write(&error) => Err(FinalizationStoreError::IdentityCollision),
        Err(_) => Err(FinalizationStoreError::Unavailable),
    }
}

fn duplicate_write(error: &MongoError) -> bool {
    matches!(
        error.kind.as_ref(),
        ErrorKind::Write(WriteFailure::WriteError(write)) if write.code == DUPLICATE_KEY_CODE
    ) || matches!(
        error.kind.as_ref(),
        ErrorKind::Command(command) if command.code == DUPLICATE_KEY_CODE
    )
}

fn fixed_binary<const N: usize>(
    document: &Document,
    name: &str,
) -> Result<[u8; N], FinalizationStoreError> {
    document
        .get_binary_generic(name)
        .map_err(|_| FinalizationStoreError::InvalidData)?
        .as_slice()
        .try_into()
        .map_err(|_| FinalizationStoreError::InvalidData)
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn binary<const N: usize>(bytes: [u8; N]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

pub(crate) fn hourly_validator() -> Document {
    doc! { "$and": [
        { "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "project_id", "issue_id", "bucket_start", "occurrence_count", "expire_at"],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "project_id": { "bsonType": "int", "minimum": 1 },
                "issue_id": { "bsonType": "binData" },
                "bucket_start": { "bsonType": "date" },
                "occurrence_count": { "bsonType": "long", "minimum": 1 },
                "expire_at": { "bsonType": "date" },
            },
        } },
        { "$expr": { "$and": [
            { "$eq": [{ "$binarySize": "$_id" }, 16] },
            { "$eq": [{ "$binarySize": "$issue_id" }, 16] },
        ] } },
    ] }
}

pub(crate) fn release_validator() -> Document {
    doc! { "$and": [
        { "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "organization_id", "version", "status", "project_ids", "first_seen", "last_seen", "first_event_id", "latest_event_id", "created_at", "source"],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "organization_id": { "bsonType": "long", "minimum": 1 },
                "version": { "bsonType": "string", "minLength": 1 },
                "status": { "enum": ["open", "archived"] },
                "project_ids": { "bsonType": "array", "minItems": 1, "items": { "bsonType": "int", "minimum": 1 } },
                "first_seen": { "bsonType": "date" },
                "last_seen": { "bsonType": "date" },
                "first_event_id": { "bsonType": "binData" },
                "latest_event_id": { "bsonType": "binData" },
                "created_at": { "bsonType": "date" },
                "released_at": { "bsonType": "date" },
                "ref": { "bsonType": "string", "minLength": 1 },
                "url": { "bsonType": "string", "minLength": 1 },
                "source": { "enum": ["event", "api"] },
            },
        } },
        { "$expr": { "$and": [
            { "$eq": [{ "$binarySize": "$_id" }, 16] },
            { "$eq": [{ "$binarySize": "$first_event_id" }, 20] },
            { "$eq": [{ "$binarySize": "$latest_event_id" }, 20] },
            { "$lte": [{ "$strLenBytes": "$version" }, 200] },
        ] } },
    ] }
}

pub(crate) fn environment_validator() -> Document {
    doc! { "$and": [
        { "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "project_id", "name", "first_seen", "last_seen", "hidden", "source"],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "project_id": { "bsonType": "int", "minimum": 1 },
                "name": { "bsonType": "string", "minLength": 1 },
                "first_seen": { "bsonType": "date" },
                "last_seen": { "bsonType": "date" },
                "hidden": { "bsonType": "bool" },
                "source": { "enum": ["event", "api"] },
            },
        } },
        { "$expr": { "$and": [
            { "$eq": [{ "$binarySize": "$_id" }, 16] },
            { "$lte": [{ "$strLenBytes": "$name" }, 64] },
        ] } },
    ] }
}

pub(crate) fn finalization_index_names(
    collection: &str,
) -> std::collections::BTreeSet<&'static str> {
    match collection {
        "issue_stats_hourly" => std::collections::BTreeSet::from([
            "_id_",
            "issue_stats_expiration",
            "issue_stats_issue_timeline",
            "issue_stats_project_timeline",
        ]),
        "releases" => std::collections::BTreeSet::from([
            "_id_",
            "release_organization_timeline",
            "release_project_timeline",
        ]),
        "environments" => {
            std::collections::BTreeSet::from(["_id_", "environment_project_timeline"])
        }
        _ => std::collections::BTreeSet::new(),
    }
}

pub(crate) async fn create_finalization_indexes(database: &Database) -> Result<(), MongoError> {
    for model in hourly_indexes() {
        database
            .collection::<Document>("issue_stats_hourly")
            .create_index(model)
            .await?;
    }
    for model in release_indexes() {
        database
            .collection::<Document>("releases")
            .create_index(model)
            .await?;
    }
    for model in environment_indexes() {
        database
            .collection::<Document>("environments")
            .create_index(model)
            .await?;
    }
    Ok(())
}

pub(crate) async fn validate_finalization_indexes(database: &Database) -> Result<bool, MongoError> {
    for (collection, expected) in [
        ("issue_stats_hourly", hourly_indexes().to_vec()),
        ("releases", release_indexes().to_vec()),
        ("environments", environment_indexes().to_vec()),
    ] {
        let mut expected = expected
            .into_iter()
            .map(|model| {
                let name = model
                    .options
                    .as_ref()
                    .and_then(|value| value.name.clone())
                    .unwrap();
                (name, model)
            })
            .collect::<BTreeMap<_, _>>();
        let mut indexes = database
            .collection::<Document>(collection)
            .list_indexes()
            .await?;
        while let Some(actual) = indexes.try_next().await? {
            let Some(options) = actual.options.as_ref() else {
                return Ok(false);
            };
            let Some(name) = options.name.as_deref() else {
                return Ok(false);
            };
            if name == "_id_" {
                continue;
            }
            let Some(model) = expected.remove(name) else {
                return Ok(false);
            };
            let expected_options = model.options.as_ref().unwrap();
            if actual.keys != model.keys
                || options.partial_filter_expression != expected_options.partial_filter_expression
                || options.expire_after != expected_options.expire_after
            {
                return Ok(false);
            }
        }
        if !expected.is_empty() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn hourly_indexes() -> [IndexModel; 3] {
    [
        named_index(
            doc! { "project_id": 1, "issue_id": 1, "bucket_start": 1 },
            "issue_stats_issue_timeline",
            None,
        ),
        named_index(
            doc! { "project_id": 1, "bucket_start": 1, "issue_id": 1 },
            "issue_stats_project_timeline",
            None,
        ),
        named_index(
            doc! { "expire_at": 1 },
            "issue_stats_expiration",
            Some(std::time::Duration::ZERO),
        ),
    ]
}

fn release_indexes() -> [IndexModel; 2] {
    [
        named_index(
            doc! { "organization_id": 1, "last_seen": -1, "_id": -1 },
            "release_organization_timeline",
            None,
        ),
        named_index(
            doc! { "organization_id": 1, "project_ids": 1, "last_seen": -1, "_id": -1 },
            "release_project_timeline",
            None,
        ),
    ]
}

fn environment_indexes() -> [IndexModel; 1] {
    [named_index(
        doc! { "project_id": 1, "hidden": 1, "last_seen": -1, "_id": -1 },
        "environment_project_timeline",
        None,
    )]
}

fn named_index(
    keys: Document,
    name: &str,
    expire_after: Option<std::time::Duration>,
) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .expire_after(expire_after)
                .build(),
        )
        .build()
}
