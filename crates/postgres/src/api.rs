//! Native API queries: only static SQL fragments and bound scoped domain values.
use crate::{
    EventCodecConfig, IssueCodecConfig, PostgresDatabase, PostgresError, PostgresEventStore,
    PostgresIssueStore, codec,
};
use metric_domain::{
    EventId, EventKey, OrganizationId, ProjectId, Timestamp,
    api::{
        ActivityAnchor, ActivityPage, EnvironmentAnchor, EnvironmentPage, EnvironmentView,
        EventAnchor, EventPage, EventView, IssueActivityKind, IssueActivityView, IssueAnchor,
        IssueListQuery, IssuePage, IssueStatBucket, ReleaseAnchor, ReleasePage, ReleaseView,
        SearchStorageAnchor, SearchStorageQuery,
    },
    event::{EventLevel, EventPlatform},
    finalization::{
        EnvironmentId, ProcessedEventPayload, ReleaseId, derive_environment_id, derive_release_id,
    },
    grouping::IssueId,
    issue::{ActorRef, IssueActivityId, IssueStatus},
};
use metric_ports::{InvestigationStore, InvestigationStoreError, PortFuture};
use sqlx::{Postgres, QueryBuilder, Row, postgres::PgRow};
use std::num::NonZeroU64;

#[derive(Clone)]
pub struct PostgresInvestigationStore {
    database: PostgresDatabase,
    event_codec: EventCodecConfig,
    issues: PostgresIssueStore,
}

impl PostgresInvestigationStore {
    pub fn new(
        database: PostgresDatabase,
        event_codec: EventCodecConfig,
        issue_codec: IssueCodecConfig,
    ) -> Result<Self, PostgresError> {
        // Reuse the writers' policy validation and lossless bounded Issue decoder.
        PostgresEventStore::new(database.clone(), event_codec)?;
        let issues = PostgresIssueStore::new(database.clone(), issue_codec)?;
        Ok(Self {
            database,
            event_codec,
            issues,
        })
    }

    fn events(&self, project: ProjectId) -> QueryBuilder<Postgres> {
        let mut query = QueryBuilder::new(
            "SELECT project_id,event_id,event_key,issue_id,received_at,occurred_at,CASE WHEN octet_length(body)<=",
        );
        query.push_bind(i64::try_from(self.event_codec.max_encoded_row_bytes).unwrap_or(i64::MAX))
            .push(" THEN body END AS body FROM metric.error_events WHERE processing_state=2 AND project_id=").push_bind(project.get());
        query
    }

    async fn event_page(
        &self,
        mut query: QueryBuilder<Postgres>,
        database_limit: usize,
        page_limit: usize,
    ) -> Result<EventPage, InvestigationStoreError> {
        query
            .push(" ORDER BY occurred_at DESC,event_id DESC LIMIT ")
            .push_bind(database_limit as i64);
        let rows = query
            .build()
            .fetch_all(&self.database.pool)
            .await
            .map_err(unavailable)?;
        let mut items: Vec<_> = rows
            .iter()
            .map(|row| self.decode_event(row))
            .collect::<Result<_, _>>()?;
        let examined = items.len();
        let has_more = items.len() > page_limit;
        items.truncate(page_limit);
        let next = has_more
            .then(|| items.last())
            .flatten()
            .map(|event| EventAnchor {
                occurred_at: event.occurred_at,
                event_key: event.key,
            });
        Ok(EventPage {
            items,
            next,
            candidates_examined: examined,
        })
    }

    fn decode_event(&self, row: &PgRow) -> Result<EventView, InvestigationStoreError> {
        let project =
            ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?;
        let key = EventKey::from_bytes(fixed(row, "event_key")?).map_err(invalid)?;
        if key != EventKey::new(project, EventId::from_bytes(fixed(row, "event_id")?)) {
            return Err(InvestigationStoreError::InvalidData);
        }
        let body: Vec<u8> = row.try_get("body").map_err(invalid)?;
        if body.len().saturating_add(256) > self.event_codec.max_encoded_row_bytes {
            return Err(InvestigationStoreError::InvalidData);
        }
        let payload =
            codec::decode_json(&body, self.event_codec.max_decoded_body_bytes).map_err(invalid)?;
        let value: serde_json::Value = serde_json::from_slice(&payload).map_err(invalid)?;
        if !value.is_object() {
            return Err(InvestigationStoreError::InvalidData);
        }
        // Match the existing native API's payload-derived projection, including custom platforms.
        let level = match value.get("level").and_then(serde_json::Value::as_str) {
            Some("debug") => EventLevel::Debug,
            Some("info") => EventLevel::Info,
            Some("warning") => EventLevel::Warning,
            Some("error") | None => EventLevel::Error,
            Some("fatal") => EventLevel::Fatal,
            _ => return Err(InvestigationStoreError::InvalidData),
        };
        let platform = match value
            .get("platform")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("other")
        {
            "javascript" => EventPlatform::JavaScript,
            "node" => EventPlatform::Node,
            "python" => EventPlatform::Python,
            "java" => EventPlatform::Java,
            "csharp" => EventPlatform::DotNet,
            "go" => EventPlatform::Go,
            "rust" => EventPlatform::Rust,
            "php" => EventPlatform::Php,
            "ruby" => EventPlatform::Ruby,
            "cocoa" => EventPlatform::Cocoa,
            "native" => EventPlatform::Native,
            "dart" => EventPlatform::Dart,
            "other" => EventPlatform::Other,
            value => EventPlatform::Custom(value.into()),
        };
        Ok(EventView {
            key,
            issue_id: IssueId::from_bytes(fixed(row, "issue_id")?),
            received_at: timestamp(row, "received_at")?,
            occurred_at: timestamp(row, "occurred_at")?,
            level,
            platform,
            payload: ProcessedEventPayload::new(payload),
        })
    }
}

impl InvestigationStore for PostgresInvestigationStore {
    fn list_issues(
        &self,
        project: ProjectId,
        query: IssueListQuery,
    ) -> PortFuture<'_, Result<IssuePage, InvestigationStoreError>> {
        Box::pin(async move {
            validate_limit(query.limit)?;
            if let (Some(from), Some(until)) = (query.from, query.until) {
                validate_range(from, until)?;
            }
            let mut sql = self.issues.select();
            sql.push(" WHERE project_id=").push_bind(project.get());
            if let Some(status) = query.status {
                sql.push(" AND status=").push_bind(match status {
                    IssueStatus::Open => 0_i16,
                    IssueStatus::Resolved => 1,
                    IssueStatus::Ignored => 2,
                });
            }
            if let Some(from) = query.from {
                sql.push(" AND last_seen>=").push_bind(from.unix_millis());
            }
            if let Some(until) = query.until {
                sql.push(" AND last_seen<").push_bind(until.unix_millis());
            }
            if let Some(before) = query.before {
                sql.push(" AND (last_seen,issue_id)<(")
                    .push_bind(before.last_seen.unix_millis())
                    .push(",")
                    .push_bind(before.issue_id.as_bytes().to_vec())
                    .push(")");
            }
            sql.push(" ORDER BY last_seen DESC,issue_id DESC LIMIT ")
                .push_bind((query.limit + 1) as i64);
            let rows = sql
                .build()
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
            let mut items: Vec<_> = rows
                .iter()
                .map(|row| self.issues.decode(row).map_err(invalid))
                .collect::<Result<_, _>>()?;
            let more = items.len() > query.limit;
            items.truncate(query.limit);
            let next = more
                .then(|| items.last())
                .flatten()
                .map(|issue| IssueAnchor {
                    last_seen: issue.last_seen,
                    issue_id: issue.issue_id,
                });
            Ok(IssuePage { items, next })
        })
    }

    fn list_events(
        &self,
        project: ProjectId,
        issue: Option<IssueId>,
        from: Timestamp,
        until: Timestamp,
        before: Option<EventAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<EventPage, InvestigationStoreError>> {
        Box::pin(async move {
            validate_limit(limit)?;
            validate_range(from, until)?;
            let mut query = self.events(project);
            query
                .push(" AND occurred_at>=")
                .push_bind(from.unix_millis())
                .push(" AND occurred_at<")
                .push_bind(until.unix_millis());
            if let Some(issue) = issue {
                query
                    .push(" AND issue_id=")
                    .push_bind(issue.as_bytes().to_vec());
            }
            event_anchor(&mut query, project, before);
            self.event_page(query, limit + 1, limit).await
        })
    }

    fn load_event(
        &self,
        project: ProjectId,
        key: EventKey,
    ) -> PortFuture<'_, Result<EventView, InvestigationStoreError>> {
        Box::pin(async move {
            if key.project_id() != project {
                return Err(InvestigationStoreError::NotFound);
            }
            let row = self
                .events(project)
                .push(" AND event_id=")
                .push_bind(key.event_id().as_bytes().to_vec())
                .build()
                .fetch_optional(&self.database.pool)
                .await
                .map_err(unavailable)?
                .ok_or(InvestigationStoreError::NotFound)?;
            self.decode_event(&row)
        })
    }

    fn search_candidates(
        &self,
        project: ProjectId,
        query: SearchStorageQuery,
    ) -> PortFuture<'_, Result<EventPage, InvestigationStoreError>> {
        Box::pin(async move {
            if query.branches.is_empty()
                || query.branches.len() > 8
                || !(1..=10_000).contains(&query.candidate_limit)
            {
                return Err(InvestigationStoreError::InvalidData);
            }
            let mut sql = self.events(project);
            sql.push(" AND (");
            for (index, branch) in query.branches.into_iter().enumerate() {
                validate_range(branch.from, branch.until)?;
                if index > 0 {
                    sql.push(" OR ");
                }
                sql.push("(occurred_at>=")
                    .push_bind(branch.from.unix_millis())
                    .push(" AND occurred_at<")
                    .push_bind(branch.until.unix_millis());
                match branch.anchor {
                    SearchStorageAnchor::ProjectTimeline => {}
                    SearchStorageAnchor::Event(key) => {
                        if key.project_id() != project {
                            sql.push(" AND false");
                        } else {
                            sql.push(" AND event_id=")
                                .push_bind(key.event_id().as_bytes().to_vec());
                        }
                    }
                    SearchStorageAnchor::Issue(id) => {
                        sql.push(" AND issue_id=").push_bind(id.as_bytes().to_vec());
                    }
                    SearchStorageAnchor::Token(token) => {
                        sql.push(" AND search_tokens @> ")
                            .push_bind(vec![token.stored()]);
                    }
                }
                sql.push(")");
            }
            sql.push(")");
            event_anchor(&mut sql, project, query.before);
            self.event_page(sql, query.candidate_limit, query.candidate_limit)
                .await
        })
    }

    fn issue_statistics(
        &self,
        project: ProjectId,
        issue: IssueId,
        from: Timestamp,
        until: Timestamp,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<IssueStatBucket>, InvestigationStoreError>> {
        Box::pin(async move {
            validate_limit(limit)?;
            validate_range(from, until)?;
            let rows=sqlx::query("SELECT bucket_start,occurrence_count FROM metric.issue_stats_hourly WHERE project_id=$1 AND issue_id=$2 AND bucket_start>=$3 AND bucket_start<$4 ORDER BY bucket_start LIMIT $5")
                .bind(project.get()).bind(issue.as_bytes().as_slice()).bind(from.unix_millis()).bind(until.unix_millis()).bind(limit as i64).fetch_all(&self.database.pool).await.map_err(unavailable)?;
            rows.iter()
                .map(|row| {
                    Ok(IssueStatBucket {
                        bucket_start: timestamp(row, "bucket_start")?,
                        occurrence_count: u64::try_from(
                            row.try_get::<i64, _>("occurrence_count").map_err(invalid)?,
                        )
                        .ok()
                        .and_then(NonZeroU64::new)
                        .ok_or(InvestigationStoreError::InvalidData)?,
                    })
                })
                .collect()
        })
    }

    fn issue_activity(
        &self,
        project: ProjectId,
        issue: IssueId,
        before: Option<ActivityAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<ActivityPage, InvestigationStoreError>> {
        Box::pin(async move {
            validate_limit(limit)?;
            let mut query = QueryBuilder::<Postgres>::new(
                "SELECT activity_id,issue_id,kind,actor,event_id,at FROM metric.issue_activities WHERE project_id=",
            );
            query
                .push_bind(project.get())
                .push(" AND issue_id=")
                .push_bind(issue.as_bytes().to_vec());
            if let Some(before) = before {
                query
                    .push(" AND (at,activity_id)<(")
                    .push_bind(before.at.unix_millis())
                    .push(",")
                    .push_bind(before.id.as_bytes().to_vec())
                    .push(")");
            }
            query
                .push(" ORDER BY at DESC,activity_id DESC LIMIT ")
                .push_bind((limit + 1) as i64);
            let rows = query
                .build()
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
            let mut items: Vec<_> = rows
                .iter()
                .map(|row| {
                    let kind = match row.try_get::<i16, _>("kind").map_err(invalid)? {
                        1 => IssueActivityKind::Resolved,
                        2 => IssueActivityKind::Ignored,
                        3 => IssueActivityKind::Reopened,
                        4 => IssueActivityKind::Assigned,
                        5 => IssueActivityKind::Unassigned,
                        6 => IssueActivityKind::Regressed,
                        _ => return Err(InvestigationStoreError::InvalidData),
                    };
                    let event: Option<Vec<u8>> = row.try_get("event_id").map_err(invalid)?;
                    Ok(IssueActivityView {
                        id: IssueActivityId::from_bytes(fixed(row, "activity_id")?),
                        issue_id: IssueId::from_bytes(fixed(row, "issue_id")?),
                        kind,
                        actor: ActorRef::from_bytes(fixed(row, "actor")?)
                            .ok_or(InvestigationStoreError::InvalidData)?,
                        event_key: event
                            .map(|bytes| {
                                Ok(EventKey::new(
                                    project,
                                    EventId::from_bytes(bytes.try_into().map_err(invalid)?),
                                ))
                            })
                            .transpose()?,
                        at: timestamp(row, "at")?,
                    })
                })
                .collect::<Result<_, _>>()?;
            let more = items.len() > limit;
            items.truncate(limit);
            let next = more
                .then(|| items.last())
                .flatten()
                .map(|item| ActivityAnchor {
                    at: item.at,
                    id: item.id,
                });
            Ok(ActivityPage { items, next })
        })
    }

    fn list_releases(
        &self,
        organization: OrganizationId,
        project: ProjectId,
        before: Option<ReleaseAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<ReleasePage, InvestigationStoreError>> {
        Box::pin(async move {
            validate_limit(limit)?;
            let org = i64::try_from(organization.get()).map_err(invalid)?;
            let mut query = QueryBuilder::<Postgres>::new(
                "SELECT release_id,version,activity_at,first_seen,last_seen,released_at,explicit FROM metric.releases r WHERE organization_id=",
            );
            query.push_bind(org).push(" AND EXISTS(SELECT 1 FROM metric.release_projects p WHERE p.organization_id=r.organization_id AND p.release_id=r.release_id AND p.project_id=").push_bind(project.get()).push(")");
            if let Some(before) = before {
                query
                    .push(" AND (activity_at,release_id)<(")
                    .push_bind(before.activity_at.unix_millis())
                    .push(",")
                    .push_bind(before.id.as_bytes().to_vec())
                    .push(")");
            }
            query
                .push(" ORDER BY activity_at DESC,release_id DESC LIMIT ")
                .push_bind((limit + 1) as i64);
            let rows = query
                .build()
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
            let mut items: Vec<_> = rows
                .iter()
                .map(|row| {
                    let id = ReleaseId::from_bytes(fixed(row, "release_id")?);
                    let version = codec::decode_utf8(row.try_get("version").map_err(invalid)?)
                        .map_err(invalid)?;
                    metric_domain::releases::validate_version(&version).map_err(invalid)?;
                    if derive_release_id(organization, &version) != id {
                        return Err(InvestigationStoreError::InvalidData);
                    }
                    let first_seen = optional_timestamp(row, "first_seen")?;
                    let last_seen = optional_timestamp(row, "last_seen")?;
                    if first_seen.is_some() != last_seen.is_some() || first_seen > last_seen {
                        return Err(InvestigationStoreError::InvalidData);
                    }
                    Ok(ReleaseView {
                        id,
                        version: version.into(),
                        activity_at: timestamp(row, "activity_at")?,
                        first_seen,
                        last_seen,
                        released_at: optional_timestamp(row, "released_at")?,
                        explicit: row.try_get("explicit").map_err(invalid)?,
                    })
                })
                .collect::<Result<_, _>>()?;
            let more = items.len() > limit;
            items.truncate(limit);
            let next = more
                .then(|| items.last())
                .flatten()
                .map(|item| ReleaseAnchor {
                    activity_at: item.activity_at,
                    id: item.id,
                });
            Ok(ReleasePage { items, next })
        })
    }

    fn list_environments(
        &self,
        project: ProjectId,
        before: Option<EnvironmentAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<EnvironmentPage, InvestigationStoreError>> {
        Box::pin(async move {
            validate_limit(limit)?;
            let mut query = QueryBuilder::<Postgres>::new(
                "SELECT environment_id,name,first_seen,last_seen FROM metric.environments WHERE NOT hidden AND project_id=",
            );
            query.push_bind(project.get());
            if let Some(before) = before {
                query
                    .push(" AND (last_seen,environment_id)<(")
                    .push_bind(before.last_seen.unix_millis())
                    .push(",")
                    .push_bind(before.id.as_bytes().to_vec())
                    .push(")");
            }
            query
                .push(" ORDER BY last_seen DESC,environment_id DESC LIMIT ")
                .push_bind((limit + 1) as i64);
            let rows = query
                .build()
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
            let mut items: Vec<_> = rows
                .iter()
                .map(|row| {
                    let id = EnvironmentId::from_bytes(fixed(row, "environment_id")?);
                    let name = codec::decode_utf8(row.try_get("name").map_err(invalid)?)
                        .map_err(invalid)?;
                    if name.is_empty()
                        || name.len() > 64
                        || derive_environment_id(project, &name) != id
                    {
                        return Err(InvestigationStoreError::InvalidData);
                    }
                    let first_seen = timestamp(row, "first_seen")?;
                    let last_seen = timestamp(row, "last_seen")?;
                    if first_seen > last_seen {
                        return Err(InvestigationStoreError::InvalidData);
                    }
                    Ok(EnvironmentView {
                        id,
                        name: name.into(),
                        first_seen,
                        last_seen,
                    })
                })
                .collect::<Result<_, _>>()?;
            let more = items.len() > limit;
            items.truncate(limit);
            let next = more
                .then(|| items.last())
                .flatten()
                .map(|item| EnvironmentAnchor {
                    last_seen: item.last_seen,
                    id: item.id,
                });
            Ok(EnvironmentPage { items, next })
        })
    }
}

fn event_anchor(
    query: &mut QueryBuilder<Postgres>,
    project: ProjectId,
    before: Option<EventAnchor>,
) {
    if let Some(before) = before {
        // Compare full keys for foreign-project cursors too; normal same-project
        // cursors use the timeline's event_id directly.
        if before.event_key.project_id() == project {
            query
                .push(" AND (occurred_at,event_id)<(")
                .push_bind(before.occurred_at.unix_millis())
                .push(",")
                .push_bind(before.event_key.event_id().as_bytes().to_vec())
                .push(")");
        } else {
            query
                .push(" AND (occurred_at,event_key)<(")
                .push_bind(before.occurred_at.unix_millis())
                .push(",")
                .push_bind(before.event_key.as_bytes().to_vec())
                .push(")");
        }
    }
}
fn validate_limit(limit: usize) -> Result<(), InvestigationStoreError> {
    if (1..=100).contains(&limit) {
        Ok(())
    } else {
        Err(InvestigationStoreError::InvalidData)
    }
}
fn validate_range(from: Timestamp, until: Timestamp) -> Result<(), InvestigationStoreError> {
    if from < until {
        Ok(())
    } else {
        Err(InvestigationStoreError::InvalidData)
    }
}
fn fixed<const N: usize>(row: &PgRow, name: &str) -> Result<[u8; N], InvestigationStoreError> {
    row.try_get::<Vec<u8>, _>(name)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
fn timestamp(row: &PgRow, name: &str) -> Result<Timestamp, InvestigationStoreError> {
    Timestamp::from_unix_millis(row.try_get(name).map_err(invalid)?).map_err(invalid)
}
fn optional_timestamp(
    row: &PgRow,
    name: &str,
) -> Result<Option<Timestamp>, InvestigationStoreError> {
    row.try_get::<Option<i64>, _>(name)
        .map_err(invalid)?
        .map(Timestamp::from_unix_millis)
        .transpose()
        .map_err(invalid)
}
fn invalid<T>(_: T) -> InvestigationStoreError {
    InvestigationStoreError::InvalidData
}
fn unavailable<T>(_: T) -> InvestigationStoreError {
    InvestigationStoreError::Unavailable
}

#[cfg(test)]
mod tests;
