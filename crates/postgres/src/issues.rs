use crate::{PostgresDatabase, PostgresError, codec};
use metric_domain::{
    EventId, ProjectId, Timestamp,
    grouping::{
        GroupingComponent, GroupingComponentKind, GroupingExplanation, GroupingKey,
        GroupingStrategy, IssueId, verify_issue_id,
    },
    issue::{
        ActorRef, IssueActivityId, IssueCommand, IssueCommandAction, IssueCommandResult,
        IssueCulprit, IssueGroupingDetail, IssueMutationKind, IssueMutationResult,
        IssueNotificationKind, IssueOccurrence, IssueRelease, IssueSearchQuery, IssueSearchResult,
        IssueSnapshot, IssueStatus, IssueTitle, IssueWorkflow, RegressionSummary,
        command_activity_id, notification_transition_id, regression_activity_id,
    },
};
use metric_ports::{IssueStore, IssueStoreError, PortFuture};
use serde_json::{Value, json};
use sqlx::{PgConnection, Row, postgres::PgRow};
use std::num::NonZeroU64;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

const ROW_METADATA_BYTES: usize = 512;
// Finalizer permits 10,000 events; all can belong to the same Issue. This is
// independent of the smaller EventWriter insert batch limit.
const MAX_OCCURRENCE_BATCH: usize = 10_000;

#[derive(Debug, Clone, Copy)]
pub struct IssueCodecConfig {
    pub compression_level: i32,
    pub compression_min_savings: usize,
    pub max_decoded_body_bytes: usize,
    pub max_encoded_row_bytes: usize,
}

impl Default for IssueCodecConfig {
    fn default() -> Self {
        Self {
            compression_level: 3,
            compression_min_savings: 64,
            max_decoded_body_bytes: 64 * 1024,
            max_encoded_row_bytes: 128 * 1024,
        }
    }
}

#[derive(Clone)]
pub struct PostgresIssueStore {
    pub(crate) database: PostgresDatabase,
    codec: IssueCodecConfig,
}

impl PostgresIssueStore {
    pub fn new(database: PostgresDatabase, codec: IssueCodecConfig) -> Result<Self, PostgresError> {
        if !(-7..=22).contains(&codec.compression_level)
            || codec.max_decoded_body_bytes == 0
            || codec.max_encoded_row_bytes <= ROW_METADATA_BYTES + 2
            || codec.max_encoded_row_bytes > 128 * 1024
        {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self { database, codec })
    }

    /// Caller owns project locks before Issue locks. Finalizer reuses this inside
    /// its authoritative transaction and emits the deferred activity after commit.
    pub(crate) async fn apply_batch(
        &self,
        connection: &mut PgConnection,
        occurrences: &[IssueOccurrence],
    ) -> Result<(IssueMutationResult, Option<Activity>), IssueStoreError> {
        let summary = occurrences.first().ok_or(IssueStoreError::InvalidData)?;
        if occurrences.len() > MAX_OCCURRENCE_BATCH
            || occurrences.iter().any(|value| {
                value.project_id != summary.project_id
                    || value.issue_id != summary.issue_id
                    || value.grouping_key != summary.grouping_key
                    || !verify_issue_id(value.project_id, value.grouping_key, value.issue_id)
            })
        {
            return Err(IssueStoreError::InvalidData);
        }
        let first = occurrences
            .iter()
            .min_by_key(|value| (value.occurred_at, value.event_id.as_bytes()))
            .ok_or(IssueStoreError::InvalidData)?;
        let latest = occurrences
            .iter()
            .max_by_key(|value| (value.occurred_at, value.event_id.as_bytes()))
            .ok_or(IssueStoreError::InvalidData)?;
        let candidate = occurrences
            .iter()
            .max_by_key(|value| (value.received_at, value.event_id.as_bytes()))
            .ok_or(IssueStoreError::InvalidData)?;
        let increment = occurrences.iter().try_fold(0_i64, |total, value| {
            total
                .checked_add(
                    i64::try_from(value.increment.get())
                        .map_err(|_| IssueStoreError::InvalidData)?,
                )
                .ok_or(IssueStoreError::InvalidData)
        })?;
        let (body, _) = codec::encode_json(
            &grouping_value(&summary.grouping),
            self.codec.compression_level,
            self.codec.compression_min_savings,
            self.codec.max_decoded_body_bytes,
        )
        .map_err(invalid)?;
        let row_bytes = body
            .len()
            .saturating_add(ROW_METADATA_BYTES)
            .saturating_add(summary.title.as_str().len())
            .saturating_add(
                summary
                    .culprit
                    .as_ref()
                    .map_or(0, |value| value.as_str().len()),
            )
            .saturating_add(
                first
                    .release
                    .as_ref()
                    .map_or(0, |value| value.as_str().len()),
            )
            .saturating_add(
                latest
                    .release
                    .as_ref()
                    .map_or(0, |value| value.as_str().len()),
            );
        if row_bytes > self.codec.max_encoded_row_bytes {
            return Err(IssueStoreError::InvalidData);
        }
        let created = sqlx::query("INSERT INTO metric.issues(project_id,issue_id,grouping_key,title,culprit,search_projection,first_seen,last_seen,first_event_id,latest_event_id,representative_event_id,occurrence_count,first_release,last_release,grouping_body) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$10,$11,$12,$13,$14) ON CONFLICT(project_id,issue_id) DO NOTHING")
            .bind(summary.project_id.get()).bind(summary.issue_id.as_bytes().as_slice())
            .bind(summary.grouping_key.to_bytes().as_slice()).bind(summary.title.as_str().as_bytes())
            .bind(summary.culprit.as_ref().map(|value| value.as_str().as_bytes()))
            .bind(search_projection(summary.title.as_str())).bind(first.occurred_at.unix_millis())
            .bind(latest.occurred_at.unix_millis()).bind(first.event_id.as_bytes().as_slice())
            .bind(latest.event_id.as_bytes().as_slice()).bind(increment)
            .bind(first.release.as_ref().map(|value| value.as_str().as_bytes()))
            .bind(latest.release.as_ref().map(|value| value.as_str().as_bytes())).bind(&body)
            .execute(&mut *connection).await.map_err(write_error)?.rows_affected() == 1;
        let row = self
            .lock_issue(connection, summary.project_id, summary.issue_id)
            .await?;
        let stored_key: Vec<u8> = row.try_get("grouping_key").map_err(invalid)?;
        if stored_key != summary.grouping_key.to_bytes() {
            return Err(IssueStoreError::IdentityCollision);
        }
        let mut issue = self.decode(&row)?;
        if created {
            insert_transition(
                connection,
                &issue,
                IssueNotificationKind::NewIssue,
                first.event_id,
                first.received_at,
            )
            .await?;
            return Ok((
                IssueMutationResult {
                    kind: IssueMutationKind::Created,
                    issue,
                },
                None,
            ));
        }
        issue.occurrence_count = positive(
            i64::try_from(issue.occurrence_count.get())
                .map_err(invalid)?
                .checked_add(increment)
                .ok_or(IssueStoreError::InvalidData)?,
        )?;
        if (first.occurred_at, first.event_id.as_bytes())
            < (issue.first_seen, issue.first_event_id.as_bytes())
        {
            issue.first_seen = first.occurred_at;
            issue.first_event_id = first.event_id;
            issue.first_release = first.release.clone();
        }
        if (latest.occurred_at, latest.event_id.as_bytes())
            > (issue.last_seen, issue.latest_event_id.as_bytes())
        {
            issue.last_seen = latest.occurred_at;
            issue.latest_event_id = latest.event_id;
            issue.representative_event_id = latest.event_id;
            issue.last_release = latest.release.clone();
        }
        let regress = issue.status == IssueStatus::Resolved
            && issue
                .workflow
                .as_ref()
                .is_some_and(|workflow| candidate.received_at > workflow.at);
        if regress {
            let count = issue
                .regression
                .as_ref()
                .map_or(0, |value| value.count.get())
                .checked_add(1)
                .ok_or(IssueStoreError::InvalidData)?;
            if i64::try_from(count).is_err() {
                return Err(IssueStoreError::InvalidData);
            }
            issue.status = IssueStatus::Open;
            issue.workflow = None;
            issue.regression = Some(RegressionSummary {
                at: candidate.received_at,
                event_id: candidate.event_id,
                count: NonZeroU64::new(count).ok_or(IssueStoreError::InvalidData)?,
                release: candidate.release.clone(),
            });
        }
        save_issue(connection, &issue).await?;
        if regress {
            insert_transition(
                connection,
                &issue,
                IssueNotificationKind::Regression,
                candidate.event_id,
                candidate.received_at,
            )
            .await?;
        }
        let kind = if issue.status == IssueStatus::Open
            && issue.regression.as_ref().is_some_and(|stored| {
                stored.event_id == candidate.event_id && stored.at == candidate.received_at
            }) {
            IssueMutationKind::Regressed
        } else {
            IssueMutationKind::Updated
        };
        let activity = (kind == IssueMutationKind::Regressed).then(|| Activity {
            project: issue.project_id,
            issue: issue.issue_id,
            id: regression_activity_id(issue.project_id, issue.issue_id, candidate.event_id),
            kind: 6,
            actor: ActorRef::system(),
            assignee: None,
            event: Some(candidate.event_id),
            at: candidate.received_at,
        });
        Ok((IssueMutationResult { kind, issue }, activity))
    }

    async fn lock_issue(
        &self,
        connection: &mut PgConnection,
        project: ProjectId,
        issue: IssueId,
    ) -> Result<PgRow, IssueStoreError> {
        sqlx::query("SELECT project_id,issue_id,grouping_key,title,culprit,first_seen,last_seen,first_event_id,latest_event_id,representative_event_id,occurrence_count,status,assignee,workflow_at,workflow_actor,regression_at,regression_event_id,regression_count,regression_release,first_release,last_release,CASE WHEN octet_length(grouping_body)<=$3 THEN grouping_body END AS grouping_body FROM metric.issues WHERE project_id=$1 AND issue_id=$2 FOR UPDATE")
            .bind(project.get()).bind(issue.as_bytes().as_slice()).bind(self.body_limit())
            .fetch_optional(connection).await.map_err(unavailable)?.ok_or(IssueStoreError::NotFound)
    }

    fn body_limit(&self) -> i64 {
        self.codec.max_encoded_row_bytes as i64
    }

    fn decode(&self, row: &PgRow) -> Result<IssueSnapshot, IssueStoreError> {
        let project_id =
            ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?;
        let issue_id = IssueId::from_bytes(fixed(row, "issue_id")?);
        let grouping_key =
            GroupingKey::parse(&row.try_get::<Vec<u8>, _>("grouping_key").map_err(invalid)?)
                .map_err(invalid)?;
        if !verify_issue_id(project_id, grouping_key, issue_id) {
            return Err(IssueStoreError::InvalidData);
        }
        let status = status(row.try_get("status").map_err(invalid)?)?;
        let workflow_at: Option<i64> = row.try_get("workflow_at").map_err(invalid)?;
        let workflow_actor: Option<Vec<u8>> = row.try_get("workflow_actor").map_err(invalid)?;
        let workflow = match (status, workflow_at, workflow_actor) {
            (IssueStatus::Open, None, None) => None,
            (IssueStatus::Resolved | IssueStatus::Ignored, Some(at), Some(actor)) => {
                Some(IssueWorkflow {
                    at: Timestamp::from_unix_millis(at).map_err(invalid)?,
                    actor: decode_actor(actor)?,
                })
            }
            _ => return Err(IssueStoreError::InvalidData),
        };
        let regression_at: Option<i64> = row.try_get("regression_at").map_err(invalid)?;
        let regression_event: Option<Vec<u8>> =
            row.try_get("regression_event_id").map_err(invalid)?;
        let regression_count: Option<i64> = row.try_get("regression_count").map_err(invalid)?;
        let regression_release = release(row, "regression_release")?;
        let regression = match (regression_at, regression_event, regression_count) {
            (None, None, None) if regression_release.is_none() => None,
            (Some(at), Some(event), Some(count)) => Some(RegressionSummary {
                at: Timestamp::from_unix_millis(at).map_err(invalid)?,
                event_id: EventId::from_bytes(event.try_into().map_err(invalid)?),
                count: positive(count)?,
                release: regression_release,
            }),
            _ => return Err(IssueStoreError::InvalidData),
        };
        let body: Vec<u8> = row.try_get("grouping_body").map_err(invalid)?;
        let title = IssueTitle::new(text(row, "title")?).map_err(invalid)?;
        let culprit = optional_text(row, "culprit")?
            .map(IssueCulprit::new)
            .transpose()
            .map_err(invalid)?;
        let first_release = release(row, "first_release")?;
        let last_release = release(row, "last_release")?;
        let size = body
            .len()
            .saturating_add(ROW_METADATA_BYTES)
            .saturating_add(title.as_str().len())
            .saturating_add(culprit.as_ref().map_or(0, |value| value.as_str().len()))
            .saturating_add(
                first_release
                    .as_ref()
                    .map_or(0, |value| value.as_str().len()),
            )
            .saturating_add(
                last_release
                    .as_ref()
                    .map_or(0, |value| value.as_str().len()),
            );
        if size > self.codec.max_encoded_row_bytes {
            return Err(IssueStoreError::InvalidData);
        }
        Ok(IssueSnapshot {
            project_id,
            issue_id,
            grouping_key,
            title,
            culprit,
            first_seen: timestamp(row, "first_seen")?,
            last_seen: timestamp(row, "last_seen")?,
            first_event_id: EventId::from_bytes(fixed(row, "first_event_id")?),
            latest_event_id: EventId::from_bytes(fixed(row, "latest_event_id")?),
            representative_event_id: EventId::from_bytes(fixed(row, "representative_event_id")?),
            occurrence_count: positive(row.try_get("occurrence_count").map_err(invalid)?)?,
            status,
            assignee: row
                .try_get::<Option<Vec<u8>>, _>("assignee")
                .map_err(invalid)?
                .map(decode_actor)
                .transpose()?,
            workflow,
            regression,
            first_release,
            last_release,
            grouping: decode_grouping(&body, self.codec.max_decoded_body_bytes)?,
        })
    }

    pub(crate) async fn emit_activity(&self, value: Activity) {
        let result = sqlx::query("INSERT INTO metric.issue_activities(project_id,activity_id,issue_id,kind,actor,assignee,event_id,at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(project_id,activity_id) DO NOTHING")
            .bind(value.project.get()).bind(value.id.as_bytes().as_slice()).bind(value.issue.as_bytes().as_slice())
            .bind(value.kind).bind(value.actor.to_bytes().as_slice()).bind(value.assignee.map(|actor|actor.to_bytes().to_vec()))
            .bind(value.event.map(|event|event.as_bytes().to_vec())).bind(value.at.unix_millis())
            .execute(&self.database.pool).await;
        if result.is_err() {
            // Names only. Do not log the driver error, statement, actor or title.
            tracing::warn!(
                operation = "issue_activity_insert",
                "PostgreSQL best-effort activity unavailable"
            );
        }
    }
}

impl IssueStore for PostgresIssueStore {
    fn apply_occurrence(
        &self,
        occurrence: IssueOccurrence,
    ) -> PortFuture<'_, Result<IssueMutationResult, IssueStoreError>> {
        Box::pin(async move {
            let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
            lock_project(&mut transaction, occurrence.project_id).await?;
            let (result, activity) = self
                .apply_batch(&mut transaction, std::slice::from_ref(&occurrence))
                .await?;
            transaction.commit().await.map_err(unavailable)?;
            if let Some(activity) = activity {
                self.emit_activity(activity).await;
            }
            Ok(result)
        })
    }

    fn apply_command(
        &self,
        command: IssueCommand,
    ) -> PortFuture<'_, Result<IssueCommandResult, IssueStoreError>> {
        Box::pin(async move {
            let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
            lock_project(&mut transaction, command.project_id).await?;
            let mut issue = self.decode(
                &self
                    .lock_issue(&mut transaction, command.project_id, command.issue_id)
                    .await?,
            )?;
            let applied = match command.action {
                IssueCommandAction::Resolve if issue.status == IssueStatus::Open => {
                    issue.status = IssueStatus::Resolved;
                    issue.workflow = Some(IssueWorkflow {
                        at: command.at,
                        actor: command.actor,
                    });
                    true
                }
                IssueCommandAction::Ignore if issue.status != IssueStatus::Ignored => {
                    issue.status = IssueStatus::Ignored;
                    issue.workflow = Some(IssueWorkflow {
                        at: command.at,
                        actor: command.actor,
                    });
                    true
                }
                IssueCommandAction::Reopen if issue.status != IssueStatus::Open => {
                    issue.status = IssueStatus::Open;
                    issue.workflow = None;
                    true
                }
                IssueCommandAction::Assign(assignee) if issue.assignee != assignee => {
                    issue.assignee = assignee;
                    true
                }
                _ => false,
            };
            if applied {
                save_issue(&mut transaction, &issue).await?;
                if command.action == IssueCommandAction::Resolve {
                    insert_transition(
                        &mut transaction,
                        &issue,
                        IssueNotificationKind::Resolved,
                        EventId::from_bytes(command.idempotency_key),
                        command.at,
                    )
                    .await?;
                }
            }
            transaction.commit().await.map_err(unavailable)?;
            if applied {
                self.emit_activity(Activity {
                    project: command.project_id,
                    issue: command.issue_id,
                    id: command_activity_id(command),
                    kind: i16::from(command.action.code()),
                    actor: command.actor,
                    assignee: match command.action {
                        IssueCommandAction::Assign(value) => value,
                        _ => None,
                    },
                    event: None,
                    at: command.at,
                })
                .await;
            }
            Ok(IssueCommandResult { applied, issue })
        })
    }

    fn load(
        &self,
        project: ProjectId,
        issue: IssueId,
    ) -> PortFuture<'_, Result<IssueSnapshot, IssueStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT project_id,issue_id,grouping_key,title,culprit,first_seen,last_seen,first_event_id,latest_event_id,representative_event_id,occurrence_count,status,assignee,workflow_at,workflow_actor,regression_at,regression_event_id,regression_count,regression_release,first_release,last_release,CASE WHEN octet_length(grouping_body)<=$3 THEN grouping_body END AS grouping_body FROM metric.issues WHERE project_id=$1 AND issue_id=$2")
                .bind(project.get()).bind(issue.as_bytes().as_slice()).bind(self.body_limit())
                .fetch_optional(&self.database.pool).await.map_err(unavailable)?.ok_or(IssueStoreError::NotFound)?;
            self.decode(&row)
        })
    }

    fn search_titles(
        &self,
        project: ProjectId,
        query: IssueSearchQuery,
    ) -> PortFuture<'_, Result<Vec<IssueSearchResult>, IssueStoreError>> {
        Box::pin(async move {
            let (positive, negative, phrases) = search_terms(query.text());
            if positive.is_empty() {
                return Ok(Vec::new());
            }
            // OR unquoted terms, require every quoted phrase, and exclude negative
            // terms. PostgreSQL websearch's default AND is not Mongo's term OR.
            // Both the full-text expression and substring phrases are bound data.
            let rows = sqlx::query("WITH q AS (SELECT pg_catalog.websearch_to_tsquery('pg_catalog.simple'::regconfig,$2) AS positive, pg_catalog.websearch_to_tsquery('pg_catalog.simple'::regconfig,$3) AS negative) SELECT issue_id,title,status,last_seen,occurrence_count FROM metric.issues CROSS JOIN q WHERE project_id=$1 AND title_search @@ q.positive AND (numnode(q.negative)=0 OR NOT title_search @@ q.negative) AND NOT EXISTS(SELECT 1 FROM unnest($5::text[]) phrase WHERE position(phrase IN search_projection)=0) ORDER BY ts_rank(title_search,q.positive) DESC,issue_id LIMIT $4")
                .bind(project.get()).bind(positive).bind(negative).bind(query.limit() as i64).bind(phrases)
                .fetch_all(&self.database.pool).await.map_err(unavailable)?;
            rows.iter()
                .map(|row| {
                    Ok(IssueSearchResult {
                        issue_id: IssueId::from_bytes(fixed(row, "issue_id")?),
                        title: IssueTitle::new(text(row, "title")?).map_err(invalid)?,
                        status: status(row.try_get("status").map_err(invalid)?)?,
                        last_seen: timestamp(row, "last_seen")?,
                        occurrence_count: positive_count(row)?,
                    })
                })
                .collect()
        })
    }
}

pub(crate) struct Activity {
    project: ProjectId,
    issue: IssueId,
    id: IssueActivityId,
    kind: i16,
    actor: ActorRef,
    assignee: Option<ActorRef>,
    event: Option<EventId>,
    at: Timestamp,
}

async fn lock_project(
    connection: &mut PgConnection,
    project: ProjectId,
) -> Result<(), IssueStoreError> {
    let found = sqlx::query("SELECT id FROM metric.projects WHERE id=$1 FOR SHARE")
        .bind(project.get())
        .fetch_optional(connection)
        .await
        .map_err(unavailable)?
        .is_some();
    if found {
        Ok(())
    } else {
        Err(IssueStoreError::NotFound)
    }
}

async fn save_issue(
    connection: &mut PgConnection,
    issue: &IssueSnapshot,
) -> Result<(), IssueStoreError> {
    sqlx::query("UPDATE metric.issues SET first_seen=$3,last_seen=$4,first_event_id=$5,latest_event_id=$6,representative_event_id=$7,occurrence_count=$8,status=$9,assignee=$10,workflow_at=$11,workflow_actor=$12,regression_at=$13,regression_event_id=$14,regression_count=$15,regression_release=$16,first_release=$17,last_release=$18 WHERE project_id=$1 AND issue_id=$2")
        .bind(issue.project_id.get()).bind(issue.issue_id.as_bytes().as_slice()).bind(issue.first_seen.unix_millis()).bind(issue.last_seen.unix_millis())
        .bind(issue.first_event_id.as_bytes().as_slice()).bind(issue.latest_event_id.as_bytes().as_slice()).bind(issue.representative_event_id.as_bytes().as_slice())
        .bind(i64::try_from(issue.occurrence_count.get()).map_err(invalid)?)
        .bind(match issue.status { IssueStatus::Open=>0_i16, IssueStatus::Resolved=>1, IssueStatus::Ignored=>2 })
        .bind(issue.assignee.map(|actor|actor.to_bytes().to_vec())).bind(issue.workflow.as_ref().map(|value|value.at.unix_millis()))
        .bind(issue.workflow.as_ref().map(|value|value.actor.to_bytes().to_vec())).bind(issue.regression.as_ref().map(|value|value.at.unix_millis()))
        .bind(issue.regression.as_ref().map(|value|value.event_id.as_bytes().to_vec()))
        .bind(issue.regression.as_ref().map(|value|i64::try_from(value.count.get()).map_err(invalid)).transpose()?)
        .bind(issue.regression.as_ref().and_then(|value|value.release.as_ref()).map(|value|value.as_str().as_bytes()))
        .bind(issue.first_release.as_ref().map(|value|value.as_str().as_bytes())).bind(issue.last_release.as_ref().map(|value|value.as_str().as_bytes()))
        .execute(connection).await.map_err(write_error)?;
    Ok(())
}

async fn insert_transition(
    connection: &mut PgConnection,
    issue: &IssueSnapshot,
    kind: IssueNotificationKind,
    event: EventId,
    at: Timestamp,
) -> Result<(), IssueStoreError> {
    let id = notification_transition_id(issue.project_id, issue.issue_id, kind, event);
    sqlx::query("INSERT INTO metric.issue_transitions(project_id,transition_id,issue_id,kind,event_id,created_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(project_id,transition_id) DO NOTHING")
        .bind(issue.project_id.get()).bind(id.as_bytes().as_slice()).bind(issue.issue_id.as_bytes().as_slice())
        .bind(kind as i16).bind(event.as_bytes().as_slice()).bind(at.unix_millis()).execute(connection).await.map_err(unavailable)?;
    Ok(())
}

fn grouping_value(value: &IssueGroupingDetail) -> Value {
    json!({"strategy":value.strategy.as_str(),"summary":value.explanation.summary,"components":value.explanation.components.iter().map(|component|json!({"kind":component.kind as u8,"value":component.value})).collect::<Vec<_>>()})
}

fn decode_grouping(body: &[u8], maximum: usize) -> Result<IssueGroupingDetail, IssueStoreError> {
    let decoded = codec::decode_json(body, maximum).map_err(invalid)?;
    let value: Value = serde_json::from_slice(&decoded).map_err(invalid)?;
    let object = value
        .as_object()
        .filter(|value| value.len() == 3)
        .ok_or(IssueStoreError::InvalidData)?;
    let strategy = match object.get("strategy").and_then(Value::as_str) {
        Some("sdk_fingerprint") => GroupingStrategy::SdkFingerprint,
        Some("exception_stack") => GroupingStrategy::ExceptionStack,
        Some("native_stack") => GroupingStrategy::NativeStack,
        Some("message") => GroupingStrategy::Message,
        _ => return Err(IssueStoreError::InvalidData),
    };
    let summary = object
        .get("summary")
        .and_then(Value::as_str)
        .ok_or(IssueStoreError::InvalidData)?
        .into();
    let components = object
        .get("components")
        .and_then(Value::as_array)
        .ok_or(IssueStoreError::InvalidData)?
        .iter()
        .map(|value| {
            let object = value
                .as_object()
                .filter(|value| value.len() == 2)
                .ok_or(IssueStoreError::InvalidData)?;
            let kind = match object.get("kind").and_then(Value::as_u64) {
                Some(1) => GroupingComponentKind::SdkFingerprint,
                Some(2) => GroupingComponentKind::DefaultStrategy,
                Some(3) => GroupingComponentKind::DefaultDigest,
                Some(4) => GroupingComponentKind::ExceptionType,
                Some(5) => GroupingComponentKind::Frame,
                Some(6) => GroupingComponentKind::FrameFunction,
                Some(7) => GroupingComponentKind::FrameModule,
                Some(8) => GroupingComponentKind::FramePath,
                Some(9) => GroupingComponentKind::FrameLine,
                Some(10) => GroupingComponentKind::NativeModule,
                Some(11) => GroupingComponentKind::NativeRelativeAddress,
                Some(12) => GroupingComponentKind::Logger,
                Some(13) => GroupingComponentKind::Message,
                _ => return Err(IssueStoreError::InvalidData),
            };
            let value = object
                .get("value")
                .and_then(Value::as_str)
                .ok_or(IssueStoreError::InvalidData)?
                .into();
            Ok(GroupingComponent { kind, value })
        })
        .collect::<Result<Vec<_>, IssueStoreError>>()?;
    Ok(IssueGroupingDetail {
        strategy,
        explanation: GroupingExplanation {
            summary,
            components,
        },
    })
}

fn search_projection(value: &str) -> String {
    value
        .nfd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn search_terms(value: &str) -> (String, String, Vec<String>) {
    let value = search_projection(value);
    let mut characters = value.chars().peekable();
    let mut positive = Vec::new();
    let mut negative = Vec::new();
    let mut phrases = Vec::new();
    while characters.peek().is_some() {
        while characters
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            characters.next();
        }
        let exclude = characters.peek() == Some(&'-');
        if exclude {
            characters.next();
        }
        let phrase = characters.peek() == Some(&'"');
        if phrase {
            characters.next();
        }
        let mut term = String::new();
        while let Some(character) = characters.peek().copied() {
            if (phrase && character == '"') || (!phrase && character.is_whitespace()) {
                break;
            }
            term.push(character);
            characters.next();
        }
        if phrase && characters.peek() == Some(&'"') {
            characters.next();
        }
        if !term.is_empty() {
            // Quotes force a term/phrase interpretation, even for literal "or" or
            // PostgreSQL operators. All fragments remain bound query parameters.
            let fragment = format!("\"{}\"", term.replace('"', " "));
            if exclude {
                negative.push(fragment);
            } else {
                positive.push(fragment);
                if phrase {
                    phrases.push(term);
                }
            }
        }
    }
    (positive.join(" OR "), negative.join(" OR "), phrases)
}

fn fixed<const N: usize>(row: &PgRow, name: &str) -> Result<[u8; N], IssueStoreError> {
    row.try_get::<Vec<u8>, _>(name)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
fn text(row: &PgRow, name: &str) -> Result<String, IssueStoreError> {
    codec::decode_utf8(row.try_get(name).map_err(invalid)?).map_err(invalid)
}
fn optional_text(row: &PgRow, name: &str) -> Result<Option<String>, IssueStoreError> {
    row.try_get::<Option<Vec<u8>>, _>(name)
        .map_err(invalid)?
        .map(codec::decode_utf8)
        .transpose()
        .map_err(invalid)
}
fn release(row: &PgRow, name: &str) -> Result<Option<IssueRelease>, IssueStoreError> {
    optional_text(row, name)?
        .map(IssueRelease::new)
        .transpose()
        .map_err(invalid)
}
fn timestamp(row: &PgRow, name: &str) -> Result<Timestamp, IssueStoreError> {
    Timestamp::from_unix_millis(row.try_get(name).map_err(invalid)?).map_err(invalid)
}
fn decode_actor(bytes: Vec<u8>) -> Result<ActorRef, IssueStoreError> {
    ActorRef::from_bytes(bytes.try_into().map_err(invalid)?).ok_or(IssueStoreError::InvalidData)
}
fn positive(value: i64) -> Result<NonZeroU64, IssueStoreError> {
    u64::try_from(value)
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or(IssueStoreError::InvalidData)
}
fn positive_count(row: &PgRow) -> Result<NonZeroU64, IssueStoreError> {
    positive(row.try_get("occurrence_count").map_err(invalid)?)
}
fn status(value: i16) -> Result<IssueStatus, IssueStoreError> {
    match value {
        0 => Ok(IssueStatus::Open),
        1 => Ok(IssueStatus::Resolved),
        2 => Ok(IssueStatus::Ignored),
        _ => Err(IssueStoreError::InvalidData),
    }
}
fn invalid<T>(_: T) -> IssueStoreError {
    IssueStoreError::InvalidData
}
fn unavailable<T>(_: T) -> IssueStoreError {
    IssueStoreError::Unavailable
}
fn write_error(error: sqlx::Error) -> IssueStoreError {
    if error
        .as_database_error()
        .and_then(|value| value.code())
        .as_deref()
        == Some("23505")
    {
        IssueStoreError::IdentityCollision
    } else {
        IssueStoreError::Unavailable
    }
}

#[cfg(test)]
mod tests;
