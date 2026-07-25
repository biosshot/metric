//! Typed MongoDB query adapter for the native investigation API.

use std::num::NonZeroU64;

use metric_domain::{
    EventKey, OrganizationId, ProjectId, Timestamp,
    api::{
        ActivityAnchor, ActivityPage, EnvironmentAnchor, EnvironmentPage, EnvironmentView,
        EventAnchor, EventPage, EventView, IssueActivityKind, IssueActivityView, IssueListQuery,
        IssuePage, IssueStatBucket, ReleaseAnchor, ReleasePage, ReleaseView, SearchStorageAnchor,
        SearchStorageQuery,
    },
    event::{EventLevel, EventPlatform},
    finalization::{EnvironmentId, ReleaseId},
    grouping::IssueId,
    issue::{ActorRef, IssueActivityId, IssueStatus},
};
use metric_ports::{InvestigationStore, InvestigationStoreError, PortFuture};
use futures_util::TryStreamExt;
use mongodb::{
    Database,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
};

use crate::{EventCodecConfig, IssueCodecConfig, decode_finalized_event, decode_issue};

#[derive(Clone)]
pub struct MongoInvestigationStore {
    database: Database,
    event_codec: EventCodecConfig,
    issue_codec: IssueCodecConfig,
}

impl MongoInvestigationStore {
    #[must_use]
    pub fn from_database(
        database: Database,
        event_codec: EventCodecConfig,
        issue_codec: IssueCodecConfig,
    ) -> Self {
        Self {
            database,
            event_codec,
            issue_codec,
        }
    }

    async fn list_issues_inner(
        &self,
        project_id: ProjectId,
        query: IssueListQuery,
    ) -> Result<IssuePage, InvestigationStoreError> {
        validate_limit(query.limit)?;
        let mut filter = doc! { "p": project_id.get() };
        match query.status {
            None => {}
            Some(IssueStatus::Open) => {
                filter.insert("s", doc! { "$exists": false });
            }
            Some(IssueStatus::Resolved) => {
                filter.insert("s", 1_i32);
            }
            Some(IssueStatus::Ignored) => {
                filter.insert("s", 2_i32);
            }
        }
        if let Some(anchor) = query.before {
            filter.insert(
                "$or",
                vec![
                    doc! { "l": { "$lt": date(anchor.last_seen) } },
                    doc! {
                        "l": date(anchor.last_seen),
                        "_id": { "$lt": binary(anchor.issue_id.as_bytes()) },
                    },
                ],
            );
        }
        let mut cursor = self
            .database
            .collection::<Document>("issues")
            .find(filter)
            .sort(doc! { "l": -1, "_id": -1 })
            .limit(i64::try_from(query.limit.saturating_add(1)).unwrap_or(101))
            .await
            .map_err(unavailable)?;
        let mut items = Vec::with_capacity(query.limit.saturating_add(1));
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            items.push(
                decode_issue(&document, self.issue_codec)
                    .map_err(|_| InvestigationStoreError::InvalidData)?,
            );
        }
        let has_more = items.len() > query.limit;
        items.truncate(query.limit);
        let next = has_more.then(|| items.last()).flatten().map(|issue| {
            metric_domain::api::IssueAnchor {
                last_seen: issue.last_seen,
                issue_id: issue.issue_id,
            }
        });
        Ok(IssuePage { items, next })
    }

    async fn list_events_inner(
        &self,
        project_id: ProjectId,
        issue_id: Option<IssueId>,
        from: Timestamp,
        until: Timestamp,
        before: Option<EventAnchor>,
        limit: usize,
    ) -> Result<EventPage, InvestigationStoreError> {
        validate_limit(limit)?;
        validate_range(from, until)?;
        let mut filter = doc! {
            "p": project_id.get(),
            "o": { "$gte": date(from), "$lt": date(until) },
            "q": { "$exists": false },
            "u": { "$exists": true },
        };
        if let Some(issue_id) = issue_id {
            filter.insert("u", binary(issue_id.as_bytes()));
        }
        append_event_anchor(&mut filter, before);
        self.find_events(filter, limit.saturating_add(1), limit)
            .await
    }

    async fn load_event_inner(
        &self,
        project_id: ProjectId,
        event_key: EventKey,
    ) -> Result<EventView, InvestigationStoreError> {
        if event_key.project_id() != project_id {
            return Err(InvestigationStoreError::NotFound);
        }
        let document = self
            .database
            .collection::<Document>("error_events")
            .find_one(doc! {
                "_id": binary(event_key.as_bytes()),
                "p": project_id.get(),
                "q": { "$exists": false },
            })
            .await
            .map_err(unavailable)?
            .ok_or(InvestigationStoreError::NotFound)?;
        self.decode_event(&document)
    }

    async fn search_candidates_inner(
        &self,
        project_id: ProjectId,
        query: SearchStorageQuery,
    ) -> Result<EventPage, InvestigationStoreError> {
        if query.branches.is_empty()
            || query.branches.len() > 8
            || !(1..=10_000).contains(&query.candidate_limit)
        {
            return Err(InvestigationStoreError::InvalidData);
        }
        let mut branches = Vec::with_capacity(query.branches.len());
        for branch in query.branches {
            validate_range(branch.from, branch.until)?;
            let mut filter = doc! {
                "o": { "$gte": date(branch.from), "$lt": date(branch.until) },
            };
            match branch.anchor {
                SearchStorageAnchor::ProjectTimeline => {}
                SearchStorageAnchor::Event(event_key) => {
                    filter.insert("_id", binary(event_key.as_bytes()));
                }
                SearchStorageAnchor::Issue(issue_id) => {
                    filter.insert("u", binary(issue_id.as_bytes()));
                }
                SearchStorageAnchor::Token(token) => {
                    filter.insert("k", token.stored());
                    filter.insert("k.0", doc! { "$exists": true });
                }
            }
            branches.push(filter);
        }
        let mut filter = doc! {
            "p": project_id.get(),
            "q": { "$exists": false },
            "u": { "$exists": true },
            "$or": branches,
        };
        append_event_anchor(&mut filter, query.before);
        self.find_events(filter, query.candidate_limit, query.candidate_limit)
            .await
    }

    async fn find_events(
        &self,
        filter: Document,
        database_limit: usize,
        page_limit: usize,
    ) -> Result<EventPage, InvestigationStoreError> {
        let mut cursor = self
            .database
            .collection::<Document>("error_events")
            .find(filter)
            .sort(doc! { "o": -1, "_id": -1 })
            .limit(i64::try_from(database_limit).unwrap_or(10_000))
            .await
            .map_err(unavailable)?;
        let mut items = Vec::with_capacity(database_limit.min(256));
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            items.push(self.decode_event(&document)?);
        }
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

    fn decode_event(&self, document: &Document) -> Result<EventView, InvestigationStoreError> {
        let decoded = decode_finalized_event(document, self.event_codec)
            .map_err(|_| InvestigationStoreError::InvalidData)?;
        let value: serde_json::Value = serde_json::from_slice(decoded.payload.as_bytes())
            .map_err(|_| InvestigationStoreError::InvalidData)?;
        let level = match value.get("level").and_then(serde_json::Value::as_str) {
            Some("debug") => EventLevel::Debug,
            Some("info") => EventLevel::Info,
            Some("warning") => EventLevel::Warning,
            Some("fatal") => EventLevel::Fatal,
            Some("error") | None => EventLevel::Error,
            Some(_) => return Err(InvestigationStoreError::InvalidData),
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
            custom => EventPlatform::Custom(custom.into()),
        };
        Ok(EventView {
            key: decoded.key,
            issue_id: decoded.issue_id,
            received_at: timestamp(document, "r")?,
            occurred_at: timestamp(document, "o")?,
            level,
            platform,
            payload: decoded.payload,
        })
    }

    async fn issue_statistics_inner(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
        from: Timestamp,
        until: Timestamp,
        limit: usize,
    ) -> Result<Vec<IssueStatBucket>, InvestigationStoreError> {
        validate_limit(limit)?;
        validate_range(from, until)?;
        let mut cursor = self
            .database
            .collection::<Document>("issue_stats_hourly")
            .find(doc! {
                "project_id": project_id.get(),
                "issue_id": binary(issue_id.as_bytes()),
                "bucket_start": { "$gte": date(from), "$lt": date(until) },
            })
            .sort(doc! { "bucket_start": 1 })
            .limit(i64::try_from(limit).unwrap_or(100))
            .await
            .map_err(unavailable)?;
        let mut values = Vec::with_capacity(limit);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let count = u64::try_from(
                document
                    .get_i64("occurrence_count")
                    .map_err(|_| InvestigationStoreError::InvalidData)?,
            )
            .ok()
            .and_then(NonZeroU64::new)
            .ok_or(InvestigationStoreError::InvalidData)?;
            values.push(IssueStatBucket {
                bucket_start: timestamp(&document, "bucket_start")?,
                occurrence_count: count,
            });
        }
        Ok(values)
    }

    async fn issue_activity_inner(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
        before: Option<ActivityAnchor>,
        limit: usize,
    ) -> Result<ActivityPage, InvestigationStoreError> {
        validate_limit(limit)?;
        let mut filter = doc! {
            "p": project_id.get(),
            "u": binary(issue_id.as_bytes()),
        };
        if let Some(anchor) = before {
            filter.insert(
                "$or",
                vec![
                    doc! { "t": { "$lt": date(anchor.at) } },
                    doc! {
                        "t": date(anchor.at),
                        "_id": { "$lt": binary(anchor.id.as_bytes()) },
                    },
                ],
            );
        }
        let mut cursor = self
            .database
            .collection::<Document>("issue_activities")
            .find(filter)
            .sort(doc! { "t": -1, "_id": -1 })
            .limit(i64::try_from(limit.saturating_add(1)).unwrap_or(101))
            .await
            .map_err(unavailable)?;
        let mut items = Vec::with_capacity(limit.saturating_add(1));
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            items.push(decode_activity(project_id, &document)?);
        }
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next = has_more
            .then(|| items.last())
            .flatten()
            .map(|activity| ActivityAnchor {
                at: activity.at,
                id: activity.id,
            });
        Ok(ActivityPage { items, next })
    }

    async fn list_releases_inner(
        &self,
        organization_id: OrganizationId,
        project_id: ProjectId,
        before: Option<ReleaseAnchor>,
        limit: usize,
    ) -> Result<ReleasePage, InvestigationStoreError> {
        validate_limit(limit)?;
        let mut filter = doc! {
            "organization_id": i64::try_from(organization_id.get())
                .map_err(|_| InvestigationStoreError::InvalidData)?,
            "project_ids": project_id.get(),
        };
        append_catalog_anchor(
            &mut filter,
            before.map(|value| (value.last_seen, value.id.as_bytes())),
        );
        let mut cursor = self
            .database
            .collection::<Document>("releases")
            .find(filter)
            .sort(doc! { "last_seen": -1, "_id": -1 })
            .limit(i64::try_from(limit.saturating_add(1)).unwrap_or(101))
            .await
            .map_err(unavailable)?;
        let mut items = Vec::with_capacity(limit.saturating_add(1));
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            items.push(ReleaseView {
                id: ReleaseId::from_bytes(fixed_binary::<16>(&document, "_id")?),
                version: document
                    .get_str("version")
                    .map_err(|_| InvestigationStoreError::InvalidData)?
                    .into(),
                first_seen: timestamp(&document, "first_seen")?,
                last_seen: timestamp(&document, "last_seen")?,
            });
        }
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next = has_more
            .then(|| items.last())
            .flatten()
            .map(|value| ReleaseAnchor {
                last_seen: value.last_seen,
                id: value.id,
            });
        Ok(ReleasePage { items, next })
    }

    async fn list_environments_inner(
        &self,
        project_id: ProjectId,
        before: Option<EnvironmentAnchor>,
        limit: usize,
    ) -> Result<EnvironmentPage, InvestigationStoreError> {
        validate_limit(limit)?;
        let mut filter = doc! { "project_id": project_id.get(), "hidden": false };
        append_catalog_anchor(
            &mut filter,
            before.map(|value| (value.last_seen, value.id.as_bytes())),
        );
        let mut cursor = self
            .database
            .collection::<Document>("environments")
            .find(filter)
            .sort(doc! { "last_seen": -1, "_id": -1 })
            .limit(i64::try_from(limit.saturating_add(1)).unwrap_or(101))
            .await
            .map_err(unavailable)?;
        let mut items = Vec::with_capacity(limit.saturating_add(1));
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            items.push(EnvironmentView {
                id: EnvironmentId::from_bytes(fixed_binary::<16>(&document, "_id")?),
                name: document
                    .get_str("name")
                    .map_err(|_| InvestigationStoreError::InvalidData)?
                    .into(),
                first_seen: timestamp(&document, "first_seen")?,
                last_seen: timestamp(&document, "last_seen")?,
            });
        }
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next = has_more
            .then(|| items.last())
            .flatten()
            .map(|value| EnvironmentAnchor {
                last_seen: value.last_seen,
                id: value.id,
            });
        Ok(EnvironmentPage { items, next })
    }
}

impl InvestigationStore for MongoInvestigationStore {
    fn list_issues(
        &self,
        project_id: ProjectId,
        query: IssueListQuery,
    ) -> PortFuture<'_, Result<IssuePage, InvestigationStoreError>> {
        Box::pin(self.list_issues_inner(project_id, query))
    }

    fn list_events(
        &self,
        project_id: ProjectId,
        issue_id: Option<IssueId>,
        from: Timestamp,
        until: Timestamp,
        before: Option<EventAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<EventPage, InvestigationStoreError>> {
        Box::pin(self.list_events_inner(project_id, issue_id, from, until, before, limit))
    }

    fn load_event(
        &self,
        project_id: ProjectId,
        event_key: EventKey,
    ) -> PortFuture<'_, Result<EventView, InvestigationStoreError>> {
        Box::pin(self.load_event_inner(project_id, event_key))
    }

    fn search_candidates(
        &self,
        project_id: ProjectId,
        query: SearchStorageQuery,
    ) -> PortFuture<'_, Result<EventPage, InvestigationStoreError>> {
        Box::pin(self.search_candidates_inner(project_id, query))
    }

    fn issue_statistics(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
        from: Timestamp,
        until: Timestamp,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<IssueStatBucket>, InvestigationStoreError>> {
        Box::pin(self.issue_statistics_inner(project_id, issue_id, from, until, limit))
    }

    fn issue_activity(
        &self,
        project_id: ProjectId,
        issue_id: IssueId,
        before: Option<ActivityAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<ActivityPage, InvestigationStoreError>> {
        Box::pin(self.issue_activity_inner(project_id, issue_id, before, limit))
    }

    fn list_releases(
        &self,
        organization_id: OrganizationId,
        project_id: ProjectId,
        before: Option<ReleaseAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<ReleasePage, InvestigationStoreError>> {
        Box::pin(self.list_releases_inner(organization_id, project_id, before, limit))
    }

    fn list_environments(
        &self,
        project_id: ProjectId,
        before: Option<EnvironmentAnchor>,
        limit: usize,
    ) -> PortFuture<'_, Result<EnvironmentPage, InvestigationStoreError>> {
        Box::pin(self.list_environments_inner(project_id, before, limit))
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

fn append_event_anchor(filter: &mut Document, before: Option<EventAnchor>) {
    if let Some(anchor) = before {
        filter.insert(
            "$and",
            vec![doc! { "$or": [
                { "o": { "$lt": date(anchor.occurred_at) } },
                {
                    "o": date(anchor.occurred_at),
                    "_id": { "$lt": binary(anchor.event_key.as_bytes()) },
                },
            ] }],
        );
    }
}

fn append_catalog_anchor(filter: &mut Document, before: Option<(Timestamp, [u8; 16])>) {
    if let Some((last_seen, id)) = before {
        filter.insert(
            "$or",
            vec![
                doc! { "last_seen": { "$lt": date(last_seen) } },
                doc! {
                    "last_seen": date(last_seen),
                    "_id": { "$lt": binary(id) },
                },
            ],
        );
    }
}

fn decode_activity(
    project_id: ProjectId,
    document: &Document,
) -> Result<IssueActivityView, InvestigationStoreError> {
    let kind = match document
        .get_i32("k")
        .map_err(|_| InvestigationStoreError::InvalidData)?
    {
        1 => IssueActivityKind::Resolved,
        2 => IssueActivityKind::Ignored,
        3 => IssueActivityKind::Reopened,
        4 => IssueActivityKind::Assigned,
        5 => IssueActivityKind::Unassigned,
        6 => IssueActivityKind::Regressed,
        _ => return Err(InvestigationStoreError::InvalidData),
    };
    let actor = ActorRef::from_bytes(fixed_binary::<17>(document, "a")?)
        .ok_or(InvestigationStoreError::InvalidData)?;
    let event_key = match document.get("e") {
        None => None,
        Some(Bson::Binary(value)) if value.bytes.len() == 16 => Some(EventKey::new(
            project_id,
            metric_domain::EventId::from_bytes(
                value
                    .bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| InvestigationStoreError::InvalidData)?,
            ),
        )),
        Some(_) => return Err(InvestigationStoreError::InvalidData),
    };
    Ok(IssueActivityView {
        id: IssueActivityId::from_bytes(fixed_binary::<16>(document, "_id")?),
        issue_id: IssueId::from_bytes(fixed_binary::<16>(document, "u")?),
        kind,
        actor,
        event_key,
        at: timestamp(document, "t")?,
    })
}

fn fixed_binary<const N: usize>(
    document: &Document,
    field: &str,
) -> Result<[u8; N], InvestigationStoreError> {
    document
        .get_binary_generic(field)
        .map_err(|_| InvestigationStoreError::InvalidData)?
        .as_slice()
        .try_into()
        .map_err(|_| InvestigationStoreError::InvalidData)
}

fn timestamp(document: &Document, field: &str) -> Result<Timestamp, InvestigationStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(field)
            .map_err(|_| InvestigationStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| InvestigationStoreError::InvalidData)
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn binary(bytes: impl AsRef<[u8]>) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.as_ref().to_vec(),
    }
}

fn unavailable(_: mongodb::error::Error) -> InvestigationStoreError {
    InvestigationStoreError::Unavailable
}
