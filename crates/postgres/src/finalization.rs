use crate::{
    EventCodecConfig, IssueCodecConfig, PostgresDatabase, PostgresError, PostgresIssueStore, codec,
};
use metric_domain::{
    EventKey, OrganizationId, Timestamp,
    event::{EventLevel, EventPlatform},
    finalization::{
        FinalizationPolicy, FinalizeBatch, FinalizeEvent, FinalizeResult, derive_environment_id,
        derive_hour_bucket_id, derive_release_id, hour_start,
    },
    grouping::verify_issue_id,
    releases::MAX_RELEASE_PROJECTS,
    signals::{SpanId, TraceId},
};
use metric_ports::{FinalizationStore, FinalizationStoreError, IssueStoreError, PortFuture};
use serde_json::Value;
use sqlx::{PgConnection, Row, types::Json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

// Matches application::Finalizer's supported maximum, including single-Issue batches.
const MAX_BATCH: usize = 10_000;
const DAY: i64 = 86_400_000;

#[derive(Clone)]
pub struct PostgresFinalizationStore {
    database: PostgresDatabase,
    event_codec: EventCodecConfig,
    issues: PostgresIssueStore,
}

impl PostgresFinalizationStore {
    pub fn new(
        database: PostgresDatabase,
        event_codec: EventCodecConfig,
        issue_codec: IssueCodecConfig,
    ) -> Result<Self, PostgresError> {
        // Reuse the Event encoding policy validation without retaining a second store.
        crate::PostgresEventStore::new(database.clone(), event_codec)?;
        let issues = PostgresIssueStore::new(database.clone(), issue_codec)?;
        Ok(Self {
            database,
            event_codec,
            issues,
        })
    }

    async fn finalize_inner(
        &self,
        batch: FinalizeBatch,
        policy: FinalizationPolicy,
    ) -> Result<FinalizeResult, FinalizationStoreError> {
        validate_policy(policy)?;
        let requested = batch.events.len();
        if !(1..=MAX_BATCH).contains(&requested) {
            return Err(FinalizationStoreError::InvalidData);
        }
        let mut prepared = BTreeMap::new();
        for event in batch.events {
            let value = prepare(event, self.event_codec, policy)?;
            if prepared.insert(value.event.key(), value).is_some() {
                return Err(FinalizationStoreError::InvalidData);
            }
        }
        let projects: Vec<_> = prepared
            .keys()
            .map(|key| key.project_id().get())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        // Lock order: projects -> per-project catalog usage -> Event keys -> Issue
        // keys -> hour tuples -> organization/Release IDs -> Environment IDs.
        // Separate usage rows avoid upgrading shared project locks and deadlocking
        // concurrent finalizers. Project fences cannot change until this commit.
        let rows = sqlx::query("SELECT id,organization_id,state FROM metric.projects WHERE id=ANY($1) ORDER BY id FOR SHARE")
            .bind(&projects).fetch_all(&mut *transaction).await.map_err(unavailable)?;
        let mut scopes = BTreeMap::new();
        for row in rows {
            let state: i16 = row.try_get("state").map_err(invalid)?;
            if matches!(state, 0 | 1) {
                let project: i32 = row.try_get("id").map_err(invalid)?;
                let organization: i64 = row.try_get("organization_id").map_err(invalid)?;
                scopes.insert(
                    project,
                    Scope {
                        organization: OrganizationId::new(
                            u64::try_from(organization).map_err(invalid)?,
                        )
                        .map_err(invalid)?,
                        release_day: None,
                        release_count: 0,
                        environments: 0,
                    },
                );
            }
        }
        let eligible: Vec<_> = scopes.keys().copied().collect();
        sqlx::query("INSERT INTO metric.project_catalog_usage(project_id) SELECT unnest($1::integer[]) ON CONFLICT(project_id) DO NOTHING")
            .bind(&eligible).execute(&mut *transaction).await.map_err(unavailable)?;
        let usages = sqlx::query("SELECT project_id,release_day,release_count,environment_count FROM metric.project_catalog_usage WHERE project_id=ANY($1) ORDER BY project_id FOR UPDATE")
            .bind(&eligible).fetch_all(&mut *transaction).await.map_err(unavailable)?;
        for row in usages {
            let scope = scopes
                .get_mut(&row.try_get::<i32, _>("project_id").map_err(invalid)?)
                .ok_or(FinalizationStoreError::InvalidData)?;
            scope.release_day = row.try_get("release_day").map_err(invalid)?;
            scope.release_count =
                u64::try_from(row.try_get::<i64, _>("release_count").map_err(invalid)?)
                    .map_err(invalid)?;
            scope.environments = u64::try_from(
                row.try_get::<i64, _>("environment_count")
                    .map_err(invalid)?,
            )
            .map_err(invalid)?;
        }
        let keys: Vec<_> = prepared
            .keys()
            .filter(|key| scopes.contains_key(&key.project_id().get()))
            .map(|key| key.as_bytes().to_vec())
            .collect();
        let rows = sqlx::query("SELECT project_id,event_id,event_key,received_at FROM metric.error_events WHERE event_key=ANY($1::bytea[]) AND project_id=ANY($2::integer[]) AND processing_state=0 ORDER BY event_key FOR UPDATE")
            .bind(keys).bind(&eligible).fetch_all(&mut *transaction).await.map_err(unavailable)?;
        let mut pending = Vec::with_capacity(rows.len());
        for row in rows {
            let bytes: Vec<u8> = row.try_get("event_key").map_err(invalid)?;
            let key = EventKey::from_bytes(bytes.try_into().map_err(invalid)?).map_err(invalid)?;
            let value = prepared
                .remove(&key)
                .ok_or(FinalizationStoreError::InvalidData)?;
            let stored_project: i32 = row.try_get("project_id").map_err(invalid)?;
            let stored_event: Vec<u8> = row.try_get("event_id").map_err(invalid)?;
            let stored_received: i64 = row.try_get("received_at").map_err(invalid)?;
            if key.project_id().get() != stored_project
                || value.event.event_id.as_bytes() != stored_event.as_slice()
                || value.event.received_at.unix_millis() != stored_received
            {
                return Err(FinalizationStoreError::InvalidData);
            }
            pending.push(value);
        }
        if pending.is_empty() {
            // Do not publish incidental quota rows for missing/fenced/completed
            // Events. There are no authoritative effects to commit in this path.
            transaction.rollback().await.map_err(unavailable)?;
            return Ok(FinalizeResult {
                requested,
                pending: 0,
                finalized: 0,
                skipped_completed: requested,
            });
        }
        let mut occurrences = BTreeMap::new();
        for value in &pending {
            occurrences
                .entry((value.event.project_id.get(), value.event.issue.issue_id))
                .or_insert_with(Vec::new)
                .push(value.event.issue.clone());
        }
        let mut activities = Vec::new();
        for values in occurrences.values() {
            let (_, activity) = self
                .issues
                .apply_batch(&mut transaction, values)
                .await
                .map_err(issue_error)?;
            activities.extend(activity);
        }
        update_hourly(&mut transaction, &pending, policy).await?;
        materialize_releases(
            &mut transaction,
            &pending,
            &mut scopes,
            policy.max_implicit_releases_per_project_day,
        )
        .await?;
        materialize_environments(
            &mut transaction,
            &pending,
            &mut scopes,
            policy.max_implicit_environments_per_project,
        )
        .await?;
        let finalized = finalize_rows(&mut transaction, &pending, &scopes, policy).await?;
        if finalized != pending.len() {
            return Err(FinalizationStoreError::Unavailable);
        }
        // The port has no Ambiguous variant. A lost commit acknowledgement is
        // Unavailable; the locked Event state makes the next attempt a safe no-op.
        transaction.commit().await.map_err(unavailable)?;
        for activity in activities {
            self.issues.emit_activity(activity).await;
        }
        Ok(FinalizeResult {
            requested,
            pending: pending.len(),
            finalized,
            skipped_completed: requested - pending.len(),
        })
    }
}

impl FinalizationStore for PostgresFinalizationStore {
    fn finalize(
        &self,
        batch: FinalizeBatch,
        policy: FinalizationPolicy,
    ) -> PortFuture<'_, Result<FinalizeResult, FinalizationStoreError>> {
        Box::pin(self.finalize_inner(batch, policy))
    }
}

struct Scope {
    organization: OrganizationId,
    release_day: Option<i64>,
    release_count: u64,
    environments: u64,
}
struct Prepared {
    event: FinalizeEvent,
    body: Vec<u8>,
    expire: i64,
    trace: Option<(TraceId, SpanId)>,
}

fn prepare(
    event: FinalizeEvent,
    config: EventCodecConfig,
    policy: FinalizationPolicy,
) -> Result<Prepared, FinalizationStoreError> {
    if event.issue.project_id != event.project_id
        || event.issue.event_id != event.event_id
        || event.issue.received_at != event.received_at
        || event.issue.occurred_at != event.occurred_at
        || !verify_issue_id(
            event.project_id,
            event.issue.grouping_key,
            event.issue.issue_id,
        )
        || event.search_tokens.len() > 16
        || event
            .search_tokens
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != event.search_tokens.len()
        || event
            .environment
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 64)
        || event.payload.as_bytes().len() > config.max_decoded_body_bytes
    {
        return Err(FinalizationStoreError::InvalidData);
    }
    let value: Value = serde_json::from_slice(event.payload.as_bytes()).map_err(invalid)?;
    if !value.is_object() {
        return Err(FinalizationStoreError::InvalidData);
    }
    let (body, canonical) = codec::encode_json(
        &value,
        config.compression_level,
        config.compression_min_savings,
        config.max_decoded_body_bytes,
    )
    .map_err(invalid)?;
    if canonical != event.payload.as_bytes()
        || body
            .len()
            .saturating_add(256)
            .saturating_add(event.search_tokens.len() * 8)
            > config.max_encoded_row_bytes
    {
        return Err(FinalizationStoreError::InvalidData);
    }
    let trace = value.pointer("/contexts/trace").and_then(|trace| {
        Some((
            TraceId::parse(trace.get("trace_id")?.as_str()?).ok()?,
            SpanId::parse(trace.get("span_id")?.as_str()?).ok()?,
        ))
    });
    let expire = add_duration(event.received_at, policy.event_retention)?;
    add_duration(hour_start(event.occurred_at), policy.hourly_retention)?;
    Ok(Prepared {
        event,
        body,
        expire,
        trace,
    })
}

async fn update_hourly(
    connection: &mut PgConnection,
    values: &[Prepared],
    policy: FinalizationPolicy,
) -> Result<(), FinalizationStoreError> {
    let mut groups = BTreeMap::new();
    for value in values {
        *groups
            .entry((
                value.event.project_id,
                value.event.issue.issue_id,
                hour_start(value.event.occurred_at),
            ))
            .or_insert(0_i64) += 1;
    }
    for ((project, issue, bucket), count) in groups {
        let id = derive_hour_bucket_id(project, issue, bucket);
        let updated = sqlx::query_scalar::<_,i64>("INSERT INTO metric.issue_stats_hourly AS h(project_id,issue_id,bucket_start,bucket_id,occurrence_count,expire_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(project_id,issue_id,bucket_start) DO UPDATE SET occurrence_count=h.occurrence_count+EXCLUDED.occurrence_count WHERE h.bucket_id=EXCLUDED.bucket_id RETURNING occurrence_count")
            .bind(project.get()).bind(issue.as_bytes().as_slice()).bind(bucket.unix_millis()).bind(id.as_bytes().as_slice()).bind(count).bind(add_duration(bucket,policy.hourly_retention)?)
            .fetch_optional(&mut *connection).await.map_err(write_error)?;
        if updated.is_none() {
            return Err(FinalizationStoreError::IdentityCollision);
        }
    }
    Ok(())
}

async fn materialize_releases(
    connection: &mut PgConnection,
    values: &[Prepared],
    scopes: &mut BTreeMap<i32, Scope>,
    limit: u32,
) -> Result<(), FinalizationStoreError> {
    let mut groups = BTreeMap::new();
    for value in values {
        if let Some(version) = &value.event.issue.release {
            let scope = scopes
                .get(&value.event.project_id.get())
                .ok_or(FinalizationStoreError::InvalidData)?;
            let id = derive_release_id(scope.organization, version.as_str());
            groups
                .entry((scope.organization, id))
                .or_insert_with(Vec::new)
                .push(&value.event);
        }
    }
    for ((organization, id), events) in groups {
        let version = events[0]
            .issue
            .release
            .as_ref()
            .ok_or(FinalizationStoreError::InvalidData)?
            .as_str();
        if events
            .iter()
            .any(|event| event.issue.release.as_ref().map(|value| value.as_str()) != Some(version))
        {
            return Err(FinalizationStoreError::IdentityCollision);
        }
        let exists:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metric.releases WHERE organization_id=$1 AND release_id=$2)")
            .bind(i64::try_from(organization.get()).map_err(invalid)?).bind(id.as_bytes().as_slice()).fetch_one(&mut *connection).await.map_err(unavailable)?;
        let mut days = BTreeMap::<i32, i64>::new();
        for event in &events {
            let day = event.received_at.unix_millis().div_euclid(DAY) * DAY;
            days.entry(event.project_id.get())
                .and_modify(|old| *old = (*old).min(day))
                .or_insert(day);
        }
        let admitted = days.iter().find_map(|(&project, &day)| {
            let scope = scopes.get(&project)?;
            (scope.release_day != Some(day) || scope.release_count < u64::from(limit))
                .then_some((project, day))
        });
        if !exists && admitted.is_none() {
            metrics::counter!("metric_catalog_admission_total","catalog"=>"release","outcome"=>"limited").increment(1);
            continue;
        }
        let first = events
            .iter()
            .min_by_key(|event| (event.occurred_at, event.key()))
            .ok_or(FinalizationStoreError::InvalidData)?;
        let latest = events
            .iter()
            .max_by_key(|event| (event.occurred_at, event.key()))
            .ok_or(FinalizationStoreError::InvalidData)?;
        let created = sqlx::query("INSERT INTO metric.releases(organization_id,release_id,version,created_at,activity_at,first_seen,last_seen,first_event_key,latest_event_key) VALUES($1,$2,$3,$4,$5,$6,$5,$7,$8) ON CONFLICT(organization_id,release_id) DO NOTHING")
            .bind(i64::try_from(organization.get()).map_err(invalid)?).bind(id.as_bytes().as_slice()).bind(version.as_bytes())
            .bind(first.received_at.unix_millis()).bind(latest.occurred_at.unix_millis()).bind(first.occurred_at.unix_millis())
            .bind(first.key().as_bytes().as_slice()).bind(latest.key().as_bytes().as_slice()).execute(&mut *connection).await.map_err(write_error)?.rows_affected()==1;
        let row = sqlx::query("SELECT version FROM metric.releases WHERE organization_id=$1 AND release_id=$2 FOR UPDATE")
            .bind(i64::try_from(organization.get()).map_err(invalid)?).bind(id.as_bytes().as_slice()).fetch_one(&mut *connection).await.map_err(unavailable)?;
        if row.try_get::<Vec<u8>, _>("version").map_err(invalid)? != version.as_bytes() {
            return Err(FinalizationStoreError::IdentityCollision);
        }
        if created {
            let (project, day) = admitted.ok_or(FinalizationStoreError::InvalidData)?;
            let scope = scopes
                .get_mut(&project)
                .ok_or(FinalizationStoreError::InvalidData)?;
            scope.release_count = if scope.release_day == Some(day) {
                scope
                    .release_count
                    .checked_add(1)
                    .ok_or(FinalizationStoreError::InvalidData)?
            } else {
                1
            };
            scope.release_day = Some(day);
            sqlx::query("UPDATE metric.project_catalog_usage SET release_day=$2,release_count=$3 WHERE project_id=$1")
                .bind(project).bind(day).bind(i64::try_from(scope.release_count).map_err(invalid)?).execute(&mut *connection).await.map_err(unavailable)?;
        }
        sqlx::query("UPDATE metric.releases SET first_event_key=CASE WHEN first_seen IS NULL OR ($3,$4)<(first_seen,first_event_key) THEN $4 ELSE first_event_key END,first_seen=CASE WHEN first_seen IS NULL OR ($3,$4)<(first_seen,first_event_key) THEN $3 ELSE first_seen END,latest_event_key=CASE WHEN last_seen IS NULL OR ($5,$6)>(last_seen,latest_event_key) THEN $6 ELSE latest_event_key END,last_seen=CASE WHEN last_seen IS NULL OR ($5,$6)>(last_seen,latest_event_key) THEN $5 ELSE last_seen END,activity_at=GREATEST(activity_at,$5) WHERE organization_id=$1 AND release_id=$2")
            .bind(i64::try_from(organization.get()).map_err(invalid)?).bind(id.as_bytes().as_slice()).bind(first.occurred_at.unix_millis())
            .bind(first.key().as_bytes().as_slice()).bind(latest.occurred_at.unix_millis()).bind(latest.key().as_bytes().as_slice()).execute(&mut *connection).await.map_err(unavailable)?;
        let linked:Vec<i32> = sqlx::query_scalar("SELECT project_id FROM metric.release_projects WHERE organization_id=$1 AND release_id=$2 ORDER BY project_id LIMIT 257")
            .bind(i64::try_from(organization.get()).map_err(invalid)?).bind(id.as_bytes().as_slice()).fetch_all(&mut *connection).await.map_err(unavailable)?;
        let mut projects: BTreeSet<_> = linked.into_iter().collect();
        projects.extend(days.keys().copied());
        if projects.len() > MAX_RELEASE_PROJECTS {
            return Err(FinalizationStoreError::InvalidData);
        }
        sqlx::query("INSERT INTO metric.release_projects(organization_id,release_id,project_id) SELECT $1,$2,unnest($3::integer[]) ON CONFLICT(organization_id,release_id,project_id) DO NOTHING")
            .bind(i64::try_from(organization.get()).map_err(invalid)?).bind(id.as_bytes().as_slice()).bind(days.keys().copied().collect::<Vec<_>>()).execute(&mut *connection).await.map_err(unavailable)?;
    }
    Ok(())
}

async fn materialize_environments(
    connection: &mut PgConnection,
    values: &[Prepared],
    scopes: &mut BTreeMap<i32, Scope>,
    limit: u32,
) -> Result<(), FinalizationStoreError> {
    let mut groups = BTreeMap::new();
    for value in values {
        if let Some(name) = &value.event.environment {
            groups
                .entry((
                    value.event.project_id,
                    derive_environment_id(value.event.project_id, name),
                ))
                .or_insert_with(Vec::new)
                .push(&value.event);
        }
    }
    for ((project, id), events) in groups {
        let name = events[0]
            .environment
            .as_deref()
            .ok_or(FinalizationStoreError::InvalidData)?;
        if events
            .iter()
            .any(|event| event.environment.as_deref() != Some(name))
        {
            return Err(FinalizationStoreError::IdentityCollision);
        }
        let existing:Option<Vec<u8>> = sqlx::query_scalar("SELECT name FROM metric.environments WHERE project_id=$1 AND environment_id=$2 FOR UPDATE")
            .bind(project.get()).bind(id.as_bytes().as_slice()).fetch_optional(&mut *connection).await.map_err(unavailable)?;
        if existing
            .as_ref()
            .is_some_and(|value| value != name.as_bytes())
        {
            return Err(FinalizationStoreError::IdentityCollision);
        }
        let scope = scopes
            .get_mut(&project.get())
            .ok_or(FinalizationStoreError::InvalidData)?;
        if existing.is_none() && scope.environments >= u64::from(limit) {
            metrics::counter!("metric_catalog_admission_total","catalog"=>"environment","outcome"=>"limited").increment(1);
            continue;
        }
        let first = events
            .iter()
            .map(|event| event.occurred_at)
            .min()
            .ok_or(FinalizationStoreError::InvalidData)?;
        let last = events
            .iter()
            .map(|event| event.occurred_at)
            .max()
            .ok_or(FinalizationStoreError::InvalidData)?;
        sqlx::query("INSERT INTO metric.environments AS e(project_id,environment_id,name,first_seen,last_seen) VALUES($1,$2,$3,$4,$5) ON CONFLICT(project_id,environment_id) DO UPDATE SET first_seen=LEAST(e.first_seen,EXCLUDED.first_seen),last_seen=GREATEST(e.last_seen,EXCLUDED.last_seen) WHERE e.name=EXCLUDED.name")
            .bind(project.get()).bind(id.as_bytes().as_slice()).bind(name.as_bytes()).bind(first.unix_millis()).bind(last.unix_millis()).execute(&mut *connection).await.map_err(write_error)?;
        if existing.is_none() {
            scope.environments = scope
                .environments
                .checked_add(1)
                .ok_or(FinalizationStoreError::InvalidData)?;
            sqlx::query(
                "UPDATE metric.project_catalog_usage SET environment_count=$2 WHERE project_id=$1",
            )
            .bind(project.get())
            .bind(i64::try_from(scope.environments).map_err(invalid)?)
            .execute(&mut *connection)
            .await
            .map_err(unavailable)?;
        }
    }
    Ok(())
}

async fn finalize_rows(
    connection: &mut PgConnection,
    values: &[Prepared],
    scopes: &BTreeMap<i32, Scope>,
    policy: FinalizationPolicy,
) -> Result<usize, FinalizationStoreError> {
    if values.is_empty() {
        return Ok(0);
    }
    let projects: Vec<_> = values
        .iter()
        .map(|value| value.event.project_id.get())
        .collect();
    let events: Vec<_> = values
        .iter()
        .map(|value| value.event.event_id.as_bytes().to_vec())
        .collect();
    let bodies: Vec<_> = values.iter().map(|value| value.body.clone()).collect();
    let issues: Vec<_> = values
        .iter()
        .map(|value| value.event.issue.issue_id.as_bytes().to_vec())
        .collect();
    let occurred: Vec<_> = values
        .iter()
        .map(|value| value.event.occurred_at.unix_millis())
        .collect();
    let levels: Vec<i16> = values
        .iter()
        .map(|value| match value.event.level {
            EventLevel::Error => 0,
            EventLevel::Debug => 1,
            EventLevel::Info => 2,
            EventLevel::Warning => 3,
            EventLevel::Fatal => 4,
        })
        .collect();
    let platforms: Vec<_> = values
        .iter()
        .map(|value| platform_code(&value.event.platform))
        .collect();
    let tokens: Vec<_> = values
        .iter()
        .map(|value| {
            Json(Value::from(
                value
                    .event
                    .search_tokens
                    .iter()
                    .map(|token| token.stored())
                    .collect::<Vec<_>>(),
            ))
        })
        .collect();
    let expiry: Vec<_> = values.iter().map(|value| value.expire).collect();
    let releases: Vec<_> = values
        .iter()
        .map(|value| {
            value.event.issue.release.as_ref().map(|release| {
                derive_release_id(
                    scopes[&value.event.project_id.get()].organization,
                    release.as_str(),
                )
                .as_bytes()
                .to_vec()
            })
        })
        .collect();
    let environments: Vec<_> = values
        .iter()
        .map(|value| {
            value.event.environment.as_deref().map(|name| {
                derive_environment_id(value.event.project_id, name)
                    .as_bytes()
                    .to_vec()
            })
        })
        .collect();
    let traces: Vec<_> = values
        .iter()
        .map(|value| value.trace.map(|trace| trace.0.as_bytes().to_vec()))
        .collect();
    let spans: Vec<_> = values
        .iter()
        .map(|value| value.trace.map(|trace| trace.1.as_bytes().to_vec()))
        .collect();
    let count = sqlx::query("UPDATE metric.error_events AS e SET processing_state=2,next_attempt_at=NULL,last_error_code=NULL,body=f.body,issue_id=f.issue,occurred_at=f.occurred,level=f.level,platform=f.platform,search_tokens=ARRAY(SELECT v::bigint FROM jsonb_array_elements_text(f.tokens) v),expire_at=CASE WHEN $14 THEN NULL ELSE f.expire END,archive_due=CASE WHEN $14 THEN f.expire ELSE NULL END,archive_segment=NULL,release_id=f.release,environment_id=f.environment,trace_id=f.trace,span_id=f.span FROM unnest($1::integer[],$2::bytea[],$3::bytea[],$4::bytea[],$5::bigint[],$6::smallint[],$7::smallint[],$8::jsonb[],$9::bigint[],$10::bytea[],$11::bytea[],$12::bytea[],$13::bytea[]) f(project,event,body,issue,occurred,level,platform,tokens,expire,release,environment,trace,span) WHERE e.project_id=f.project AND e.event_id=f.event AND e.processing_state=0")
        .bind(projects).bind(events).bind(bodies).bind(issues).bind(occurred).bind(levels).bind(platforms).bind(tokens).bind(expiry).bind(releases).bind(environments).bind(traces).bind(spans).bind(policy.archive_events)
        .execute(connection).await.map_err(unavailable)?.rows_affected();
    usize::try_from(count).map_err(invalid)
}

fn platform_code(value: &EventPlatform) -> i16 {
    match value {
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
fn validate_policy(value: FinalizationPolicy) -> Result<(), FinalizationStoreError> {
    if value.event_retention.is_zero()
        || value.hourly_retention.is_zero()
        || value.max_implicit_releases_per_project_day == 0
        || value.max_implicit_environments_per_project == 0
        || i64::try_from(value.event_retention.as_millis()).is_err()
        || i64::try_from(value.hourly_retention.as_millis()).is_err()
    {
        return Err(FinalizationStoreError::InvalidData);
    }
    Ok(())
}
fn add_duration(timestamp: Timestamp, duration: Duration) -> Result<i64, FinalizationStoreError> {
    let value = timestamp
        .unix_millis()
        .checked_add(i64::try_from(duration.as_millis()).map_err(invalid)?)
        .ok_or(FinalizationStoreError::InvalidData)?;
    Timestamp::from_unix_millis(value).map_err(invalid)?;
    Ok(value)
}
fn issue_error(error: IssueStoreError) -> FinalizationStoreError {
    match error {
        IssueStoreError::IdentityCollision => FinalizationStoreError::IdentityCollision,
        IssueStoreError::InvalidData | IssueStoreError::NotFound => {
            FinalizationStoreError::InvalidData
        }
        IssueStoreError::Unavailable => FinalizationStoreError::Unavailable,
    }
}
fn invalid<T>(_: T) -> FinalizationStoreError {
    FinalizationStoreError::InvalidData
}
fn unavailable<T>(_: T) -> FinalizationStoreError {
    FinalizationStoreError::Unavailable
}
fn write_error(error: sqlx::Error) -> FinalizationStoreError {
    match error
        .as_database_error()
        .and_then(|value| value.code())
        .as_deref()
    {
        Some("23505") => FinalizationStoreError::IdentityCollision,
        Some("22003") => FinalizationStoreError::InvalidData,
        _ => FinalizationStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
