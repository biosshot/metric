use crate::{PostgresDatabase, codec};
use metric_domain::{
    EventKey, OrganizationId, ProjectId, Timestamp,
    finalization::{ReleaseId, derive_release_id},
    grouping::IssueId,
    issue::{IssueRelease, IssueTitle},
    releases::{
        CreateDeploy, CreateRelease, DeployId, DeployRecord, FinalizeRelease, MAX_RELEASE_PROJECTS,
        ReleaseIssueSummary, ReleaseRecord, RepositoryReference, validate_version,
    },
};
use metric_ports::{PortFuture, ReleaseIssueKind, ReleaseStore, ReleaseStoreError};
use serde_json::{Value, json};
use sqlx::{PgConnection, Row, postgres::PgRow};
use std::collections::BTreeSet;

const REPOSITORY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct PostgresReleaseStore {
    database: PostgresDatabase,
}

impl PostgresReleaseStore {
    #[must_use]
    pub fn new(database: PostgresDatabase) -> Self {
        Self { database }
    }

    async fn create_release_inner(
        &self,
        command: CreateRelease,
    ) -> Result<ReleaseRecord, ReleaseStoreError> {
        let id = command.validate().map_err(invalid)?;
        let org = organization(command.organization_id)?;
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        lock_projects(
            &mut transaction,
            command.organization_id,
            &command.project_ids,
        )
        .await?;
        sqlx::query("INSERT INTO metric.releases(organization_id,release_id,version,created_at,activity_at,explicit) VALUES($1,$2,$3,$4,$4,true) ON CONFLICT(organization_id,release_id) DO NOTHING")
            .bind(org).bind(id.as_bytes().as_slice()).bind(command.version.as_bytes()).bind(command.created_at.unix_millis()).execute(&mut *transaction).await.map_err(write_error)?;
        let existing:Vec<u8>=sqlx::query_scalar("SELECT version FROM metric.releases WHERE organization_id=$1 AND release_id=$2 FOR UPDATE")
            .bind(org).bind(id.as_bytes().as_slice()).fetch_one(&mut *transaction).await.map_err(unavailable)?;
        if existing != command.version.as_bytes() {
            return Err(ReleaseStoreError::Conflict);
        }
        let repositories = if command.repositories.is_empty() {
            None
        } else {
            Some(encode_repositories(&command.repositories)?)
        };
        sqlx::query("UPDATE metric.releases SET explicit=true,activity_at=GREATEST(activity_at,$3),url=COALESCE($4,url),reference=COALESCE($5,reference),repositories_body=COALESCE($6,repositories_body) WHERE organization_id=$1 AND release_id=$2")
            .bind(org).bind(id.as_bytes().as_slice()).bind(command.created_at.unix_millis()).bind(command.url.as_ref().map(|value|value.as_bytes()))
            .bind(command.reference.as_ref().map(|value|value.as_bytes())).bind(repositories).execute(&mut *transaction).await.map_err(unavailable)?;
        let linked:Vec<i32>=sqlx::query_scalar("SELECT project_id FROM metric.release_projects WHERE organization_id=$1 AND release_id=$2 ORDER BY project_id LIMIT 257")
            .bind(org).bind(id.as_bytes().as_slice()).fetch_all(&mut *transaction).await.map_err(unavailable)?;
        let mut projects: BTreeSet<_> = linked.into_iter().collect();
        projects.extend(command.project_ids.iter().map(|project| project.get()));
        if projects.len() > MAX_RELEASE_PROJECTS {
            return Err(ReleaseStoreError::InvalidData);
        }
        sqlx::query("INSERT INTO metric.release_projects(organization_id,release_id,project_id) SELECT $1,$2,unnest($3::integer[]) ON CONFLICT(organization_id,release_id,project_id) DO NOTHING")
            .bind(org).bind(id.as_bytes().as_slice()).bind(command.project_ids.iter().map(|project|project.get()).collect::<Vec<_>>()).execute(&mut *transaction).await.map_err(unavailable)?;
        let result = load_release(&mut transaction, command.organization_id, id).await?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn create_deploy_inner(
        &self,
        command: CreateDeploy,
    ) -> Result<DeployRecord, ReleaseStoreError> {
        command.validate().map_err(invalid)?;
        let org = organization(command.organization_id)?;
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        lock_projects(
            &mut transaction,
            command.organization_id,
            &command.project_ids,
        )
        .await?;
        let exists=sqlx::query("SELECT release_id FROM metric.releases WHERE organization_id=$1 AND release_id=$2 FOR KEY SHARE")
            .bind(org).bind(command.release_id.as_bytes().as_slice()).fetch_optional(&mut *transaction).await.map_err(unavailable)?.is_some();
        if !exists {
            return Err(ReleaseStoreError::NotFound);
        }
        let created=sqlx::query("INSERT INTO metric.deploys(organization_id,deploy_id,release_id,environment,name,url,started_at,finished_at,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING")
            .bind(org).bind(command.deploy_id.as_bytes().as_slice()).bind(command.release_id.as_bytes().as_slice()).bind(command.environment.as_bytes())
            .bind(command.name.as_ref().map(|value|value.as_bytes())).bind(command.url.as_ref().map(|value|value.as_bytes()))
            .bind(command.started_at.unix_millis()).bind(command.finished_at.map(|value|value.unix_millis())).bind(command.created_at.unix_millis())
            .execute(&mut *transaction).await.map_err(write_error)?.rows_affected()==1;
        if created {
            sqlx::query("INSERT INTO metric.deploy_projects(organization_id,deploy_id,release_id,started_at,project_id,ordinal) SELECT $1,$2,$3,$4,project,(ordinal-1)::smallint FROM unnest($5::integer[]) WITH ORDINALITY p(project,ordinal)")
                .bind(org).bind(command.deploy_id.as_bytes().as_slice()).bind(command.release_id.as_bytes().as_slice()).bind(command.started_at.unix_millis())
                .bind(command.project_ids.iter().map(|project|project.get()).collect::<Vec<_>>()).execute(&mut *transaction).await.map_err(unavailable)?;
        }
        let result =
            load_deploy(&mut transaction, command.organization_id, command.deploy_id).await?;
        if result.release_id != command.release_id
            || result.project_ids != command.project_ids
            || result.environment != command.environment
            || result.name != command.name
            || result.url != command.url
            || result.started_at != command.started_at
            || result.finished_at != command.finished_at
            || result.created_at != command.created_at
        {
            return Err(ReleaseStoreError::Conflict);
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(result)
    }
}

impl ReleaseStore for PostgresReleaseStore {
    fn resolve_projects(
        &self,
        organization_slug: Box<str>,
        project_slugs: Vec<Box<str>>,
    ) -> PortFuture<'_, Result<(OrganizationId, Vec<ProjectId>), ReleaseStoreError>> {
        Box::pin(async move {
            if project_slugs.is_empty()
                || project_slugs.len() > MAX_RELEASE_PROJECTS
                || project_slugs.iter().collect::<BTreeSet<_>>().len() != project_slugs.len()
            {
                return Err(ReleaseStoreError::InvalidData);
            }
            // Stored Slugs are domain-validated. Invalid free lookup strings
            // cannot match one; avoid attempting a native text bind for U+0000.
            if metric_domain::Slug::new(organization_slug.as_ref()).is_err()
                || project_slugs
                    .iter()
                    .any(|value| metric_domain::Slug::new(value.as_ref()).is_err())
            {
                return Err(ReleaseStoreError::NotFound);
            }
            let org: Option<i64> =
                sqlx::query_scalar("SELECT id FROM metric.organizations WHERE slug=$1")
                    .bind(organization_slug.as_ref())
                    .fetch_optional(&self.database.pool)
                    .await
                    .map_err(unavailable)?;
            let org = org.ok_or(ReleaseStoreError::NotFound)?;
            let projects:Vec<i32>=sqlx::query_scalar("SELECT id FROM metric.projects WHERE organization_id=$1 AND slug=ANY($2::text[]) AND state IN (0,1) ORDER BY id")
                .bind(org).bind(project_slugs.iter().map(|value|value.as_ref()).collect::<Vec<_>>()).fetch_all(&self.database.pool).await.map_err(unavailable)?;
            if projects.len() != project_slugs.len() {
                return Err(ReleaseStoreError::NotFound);
            }
            Ok((
                OrganizationId::new(u64::try_from(org).map_err(invalid)?).map_err(invalid)?,
                projects
                    .into_iter()
                    .map(ProjectId::new)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(invalid)?,
            ))
        })
    }
    fn create_release(
        &self,
        command: CreateRelease,
    ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>> {
        Box::pin(self.create_release_inner(command))
    }
    fn finalize_release(
        &self,
        command: FinalizeRelease,
    ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>> {
        Box::pin(async move {
            let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
            let updated=sqlx::query("UPDATE metric.releases SET released_at=COALESCE(released_at,$3),activity_at=GREATEST(activity_at,$3),explicit=true WHERE organization_id=$1 AND release_id=$2")
                .bind(organization(command.organization_id)?).bind(command.release_id.as_bytes().as_slice()).bind(command.released_at.unix_millis()).execute(&mut *transaction).await.map_err(unavailable)?.rows_affected();
            if updated == 0 {
                return Err(ReleaseStoreError::NotFound);
            }
            let result = load_release(
                &mut transaction,
                command.organization_id,
                command.release_id,
            )
            .await?;
            transaction.commit().await.map_err(unavailable)?;
            Ok(result)
        })
    }
    fn load_release(
        &self,
        organization: OrganizationId,
        id: ReleaseId,
    ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>> {
        Box::pin(async move {
            let mut connection = self.database.pool.acquire().await.map_err(unavailable)?;
            load_release(&mut connection, organization, id).await
        })
    }
    fn create_deploy(
        &self,
        command: CreateDeploy,
    ) -> PortFuture<'_, Result<DeployRecord, ReleaseStoreError>> {
        Box::pin(self.create_deploy_inner(command))
    }
    fn finish_deploy(
        &self,
        organization_id: OrganizationId,
        id: DeployId,
        finished: Timestamp,
    ) -> PortFuture<'_, Result<DeployRecord, ReleaseStoreError>> {
        Box::pin(async move {
            let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
            let locked=sqlx::query("SELECT started_at,finished_at FROM metric.deploys WHERE organization_id=$1 AND deploy_id=$2 FOR UPDATE")
                .bind(organization(organization_id)?).bind(id.as_bytes().as_slice()).fetch_optional(&mut *transaction).await.map_err(unavailable)?.ok_or(ReleaseStoreError::NotFound)?;
            if finished.unix_millis() < locked.try_get::<i64, _>("started_at").map_err(invalid)? {
                return Err(ReleaseStoreError::InvalidData);
            }
            let current: Option<i64> = locked.try_get("finished_at").map_err(invalid)?;
            if current.is_some_and(|value| value != finished.unix_millis()) {
                return Err(ReleaseStoreError::Conflict);
            }
            sqlx::query("UPDATE metric.deploys SET finished_at=$3 WHERE organization_id=$1 AND deploy_id=$2")
                .bind(organization(organization_id)?).bind(id.as_bytes().as_slice()).bind(finished.unix_millis()).execute(&mut *transaction).await.map_err(unavailable)?;
            let result = load_deploy(&mut transaction, organization_id, id).await?;
            transaction.commit().await.map_err(unavailable)?;
            Ok(result)
        })
    }
    fn list_deploys(
        &self,
        organization_id: OrganizationId,
        project: ProjectId,
        release: ReleaseId,
        before: Option<(Timestamp, DeployId)>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<DeployRecord>, ReleaseStoreError>> {
        Box::pin(async move {
            check_limit(limit)?;
            let rows=sqlx::query("SELECT d.*,ARRAY(SELECT p.project_id FROM metric.deploy_projects p WHERE p.organization_id=d.organization_id AND p.deploy_id=d.deploy_id ORDER BY p.ordinal LIMIT 257) AS project_ids FROM metric.deploy_projects q JOIN metric.deploys d ON d.organization_id=q.organization_id AND d.deploy_id=q.deploy_id WHERE q.organization_id=$1 AND q.project_id=$2 AND q.release_id=$3 AND ($4::bigint IS NULL OR (q.started_at,q.deploy_id)<($4,$5::bytea)) ORDER BY q.started_at DESC,q.deploy_id DESC LIMIT $6")
                .bind(organization(organization_id)?).bind(project.get()).bind(release.as_bytes().as_slice())
                .bind(before.map(|value|value.0.unix_millis())).bind(before.map(|value|value.1.as_bytes().to_vec())).bind(limit as i64)
                .fetch_all(&self.database.pool).await.map_err(unavailable)?;
            rows.iter().map(decode_deploy).collect()
        })
    }
    fn list_release_issues(
        &self,
        project: ProjectId,
        release: Box<str>,
        kind: ReleaseIssueKind,
        before: Option<(Timestamp, IssueId)>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ReleaseIssueSummary>, ReleaseStoreError>> {
        Box::pin(async move {
            check_limit(limit)?;
            validate_version(&release).map_err(invalid)?;
            let query = match kind {
                ReleaseIssueKind::New => sqlx::query(
                    "SELECT issue_id,title,first_seen,last_seen,first_release,last_release FROM metric.issues WHERE project_id=$1 AND first_release=$2 AND ($3::bigint IS NULL OR (first_seen,issue_id)<($3,$4::bytea)) ORDER BY first_seen DESC,issue_id DESC LIMIT $5",
                ),
                ReleaseIssueKind::Regressed => sqlx::query(
                    "SELECT issue_id,title,first_seen,last_seen,first_release,last_release FROM metric.issues WHERE project_id=$1 AND regression_release=$2 AND ($3::bigint IS NULL OR (regression_at,issue_id)<($3,$4::bytea)) ORDER BY regression_at DESC,issue_id DESC LIMIT $5",
                ),
            };
            let rows = query
                .bind(project.get())
                .bind(release.as_bytes())
                .bind(before.map(|value| value.0.unix_millis()))
                .bind(before.map(|value| value.1.as_bytes().to_vec()))
                .bind(limit as i64)
                .fetch_all(&self.database.pool)
                .await
                .map_err(unavailable)?;
            rows.iter()
                .map(|row| {
                    Ok(ReleaseIssueSummary {
                        issue_id: IssueId::from_bytes(fixed(row, "issue_id")?),
                        title: IssueTitle::new(text(row, "title")?).map_err(invalid)?,
                        first_seen: timestamp(row, "first_seen")?,
                        last_seen: timestamp(row, "last_seen")?,
                        first_release: optional_text(row, "first_release")?
                            .map(IssueRelease::new)
                            .transpose()
                            .map_err(invalid)?,
                        last_release: optional_text(row, "last_release")?
                            .map(IssueRelease::new)
                            .transpose()
                            .map_err(invalid)?,
                    })
                })
                .collect()
        })
    }
}

async fn lock_projects(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    projects: &[ProjectId],
) -> Result<(), ReleaseStoreError> {
    let mut ids: Vec<_> = projects.iter().map(|project| project.get()).collect();
    ids.sort_unstable();
    let found:Vec<i32>=sqlx::query_scalar("SELECT id FROM metric.projects WHERE organization_id=$1 AND id=ANY($2::integer[]) AND state IN (0,1) ORDER BY id FOR SHARE")
        .bind(organization(organization_id)?).bind(&ids).fetch_all(connection).await.map_err(unavailable)?;
    if found == ids {
        Ok(())
    } else {
        Err(ReleaseStoreError::NotFound)
    }
}
async fn load_release(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    id: ReleaseId,
) -> Result<ReleaseRecord, ReleaseStoreError> {
    let row=sqlx::query("SELECT r.*,ARRAY(SELECT p.project_id FROM metric.release_projects p WHERE p.organization_id=r.organization_id AND p.release_id=r.release_id ORDER BY p.project_id LIMIT 257) AS project_ids FROM metric.releases r WHERE r.organization_id=$1 AND r.release_id=$2")
        .bind(organization(organization_id)?).bind(id.as_bytes().as_slice()).fetch_optional(connection).await.map_err(unavailable)?.ok_or(ReleaseStoreError::NotFound)?;
    decode_release(&row)
}
fn decode_release(row: &PgRow) -> Result<ReleaseRecord, ReleaseStoreError> {
    let org = OrganizationId::new(
        u64::try_from(row.try_get::<i64, _>("organization_id").map_err(invalid)?)
            .map_err(invalid)?,
    )
    .map_err(invalid)?;
    let id = ReleaseId::from_bytes(fixed(row, "release_id")?);
    let version = text(row, "version")?;
    let projects = project_ids(row)?;
    let repositories = row
        .try_get::<Option<Vec<u8>>, _>("repositories_body")
        .map_err(invalid)?
        .map(|body| decode_repositories(&body))
        .transpose()?
        .unwrap_or_default();
    let command = CreateRelease {
        organization_id: org,
        project_ids: projects.clone(),
        version: version.into(),
        url: optional_text(row, "url")?,
        reference: optional_text(row, "reference")?,
        repositories,
        created_at: timestamp(row, "created_at")?,
    };
    if command.validate().map_err(invalid)? != id || derive_release_id(org, &command.version) != id
    {
        return Err(ReleaseStoreError::InvalidData);
    }
    let first_seen = optional_timestamp(row, "first_seen")?;
    let last_seen = optional_timestamp(row, "last_seen")?;
    let first_event = row
        .try_get::<Option<Vec<u8>>, _>("first_event_key")
        .map_err(invalid)?
        .map(decode_event)
        .transpose()?;
    let latest_event = row
        .try_get::<Option<Vec<u8>>, _>("latest_event_key")
        .map_err(invalid)?
        .map(decode_event)
        .transpose()?;
    if (
        first_seen.is_some(),
        last_seen.is_some(),
        first_event.is_some(),
        latest_event.is_some(),
    ) != (false, false, false, false)
        && (first_seen.is_none()
            || last_seen.is_none()
            || first_event.is_none()
            || latest_event.is_none())
    {
        return Err(ReleaseStoreError::InvalidData);
    }
    Ok(ReleaseRecord {
        id,
        organization_id: org,
        version: command.version,
        project_ids: projects,
        created_at: command.created_at,
        activity_at: timestamp(row, "activity_at")?,
        released_at: optional_timestamp(row, "released_at")?,
        first_seen,
        last_seen,
        first_event_id: first_event.map(|value| value.event_id()),
        latest_event_id: latest_event.map(|value| value.event_id()),
        url: command.url,
        reference: command.reference,
        repositories: command.repositories,
        explicit: row.try_get("explicit").map_err(invalid)?,
    })
}
async fn load_deploy(
    connection: &mut PgConnection,
    organization_id: OrganizationId,
    id: DeployId,
) -> Result<DeployRecord, ReleaseStoreError> {
    let row=sqlx::query("SELECT d.*,ARRAY(SELECT p.project_id FROM metric.deploy_projects p WHERE p.organization_id=d.organization_id AND p.deploy_id=d.deploy_id ORDER BY p.ordinal LIMIT 257) AS project_ids FROM metric.deploys d WHERE d.organization_id=$1 AND d.deploy_id=$2")
        .bind(organization(organization_id)?).bind(id.as_bytes().as_slice()).fetch_optional(connection).await.map_err(unavailable)?.ok_or(ReleaseStoreError::NotFound)?;
    decode_deploy(&row)
}
fn decode_deploy(row: &PgRow) -> Result<DeployRecord, ReleaseStoreError> {
    let command = CreateDeploy {
        deploy_id: DeployId::from_bytes(fixed(row, "deploy_id")?),
        organization_id: OrganizationId::new(
            u64::try_from(row.try_get::<i64, _>("organization_id").map_err(invalid)?)
                .map_err(invalid)?,
        )
        .map_err(invalid)?,
        release_id: ReleaseId::from_bytes(fixed(row, "release_id")?),
        project_ids: project_ids(row)?,
        environment: text(row, "environment")?.into(),
        name: optional_text(row, "name")?,
        url: optional_text(row, "url")?,
        started_at: timestamp(row, "started_at")?,
        finished_at: optional_timestamp(row, "finished_at")?,
        created_at: timestamp(row, "created_at")?,
    };
    command.validate().map_err(invalid)?;
    Ok(DeployRecord {
        id: command.deploy_id,
        release_id: command.release_id,
        project_ids: command.project_ids,
        environment: command.environment,
        name: command.name,
        url: command.url,
        started_at: command.started_at,
        finished_at: command.finished_at,
        created_at: command.created_at,
    })
}
fn encode_repositories(values: &[RepositoryReference]) -> Result<Vec<u8>, ReleaseStoreError> {
    let value=Value::Array(values.iter().map(|value|json!({"repository":value.repository,"from":value.commit_from,"to":value.commit_to})).collect());
    codec::encode_json(&value, 3, 64, REPOSITORY_BYTES)
        .map(|value| value.0)
        .map_err(invalid)
}
fn decode_repositories(body: &[u8]) -> Result<Vec<RepositoryReference>, ReleaseStoreError> {
    let value: Value =
        serde_json::from_slice(&codec::decode_json(body, REPOSITORY_BYTES).map_err(invalid)?)
            .map_err(invalid)?;
    value
        .as_array()
        .filter(|array| array.len() <= 16)
        .ok_or(ReleaseStoreError::InvalidData)?
        .iter()
        .map(|value| {
            let object = value
                .as_object()
                .filter(|object| object.len() == 3)
                .ok_or(ReleaseStoreError::InvalidData)?;
            let nullable = |name| match object.get(name) {
                Some(Value::Null) => Ok(None),
                Some(Value::String(value)) => Ok(Some(value.clone().into_boxed_str())),
                _ => Err(ReleaseStoreError::InvalidData),
            };
            Ok(RepositoryReference {
                repository: object
                    .get("repository")
                    .and_then(Value::as_str)
                    .ok_or(ReleaseStoreError::InvalidData)?
                    .into(),
                commit_from: nullable("from")?,
                commit_to: nullable("to")?,
            })
        })
        .collect()
}
fn project_ids(row: &PgRow) -> Result<Vec<ProjectId>, ReleaseStoreError> {
    let values: Vec<i32> = row.try_get("project_ids").map_err(invalid)?;
    if values.is_empty()
        || values.len() > MAX_RELEASE_PROJECTS
        || values.iter().collect::<BTreeSet<_>>().len() != values.len()
    {
        return Err(ReleaseStoreError::InvalidData);
    }
    values
        .into_iter()
        .map(ProjectId::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)
}
fn decode_event(bytes: Vec<u8>) -> Result<EventKey, ReleaseStoreError> {
    EventKey::from_bytes(bytes.try_into().map_err(invalid)?).map_err(invalid)
}
fn organization(value: OrganizationId) -> Result<i64, ReleaseStoreError> {
    i64::try_from(value.get()).map_err(invalid)
}
fn fixed<const N: usize>(row: &PgRow, name: &str) -> Result<[u8; N], ReleaseStoreError> {
    row.try_get::<Vec<u8>, _>(name)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
fn text(row: &PgRow, name: &str) -> Result<String, ReleaseStoreError> {
    codec::decode_utf8(row.try_get(name).map_err(invalid)?).map_err(invalid)
}
fn optional_text(row: &PgRow, name: &str) -> Result<Option<Box<str>>, ReleaseStoreError> {
    row.try_get::<Option<Vec<u8>>, _>(name)
        .map_err(invalid)?
        .map(|bytes| codec::decode_utf8(bytes).map(String::into_boxed_str))
        .transpose()
        .map_err(invalid)
}
fn timestamp(row: &PgRow, name: &str) -> Result<Timestamp, ReleaseStoreError> {
    Timestamp::from_unix_millis(row.try_get(name).map_err(invalid)?).map_err(invalid)
}
fn optional_timestamp(row: &PgRow, name: &str) -> Result<Option<Timestamp>, ReleaseStoreError> {
    row.try_get::<Option<i64>, _>(name)
        .map_err(invalid)?
        .map(Timestamp::from_unix_millis)
        .transpose()
        .map_err(invalid)
}
fn check_limit(value: usize) -> Result<(), ReleaseStoreError> {
    if (1..=100).contains(&value) {
        Ok(())
    } else {
        Err(ReleaseStoreError::InvalidData)
    }
}
fn invalid<T>(_: T) -> ReleaseStoreError {
    ReleaseStoreError::InvalidData
}
fn unavailable<T>(_: T) -> ReleaseStoreError {
    ReleaseStoreError::Unavailable
}
fn write_error(error: sqlx::Error) -> ReleaseStoreError {
    if error
        .as_database_error()
        .and_then(|value| value.code())
        .as_deref()
        == Some("23505")
    {
        ReleaseStoreError::Conflict
    } else {
        ReleaseStoreError::Unavailable
    }
}

#[cfg(test)]
mod tests;
