use crate::{PostgresDatabase, PostgresError};
use metric_domain::{
    api::{ProjectKeyView, ProjectPolicyUpdate, ProjectView},
    inbound_filter::*,
    *,
};
use metric_ports::{PortFuture, ProjectStore, ProjectStoreError};
use serde_json::{Value, json};
use sqlx::{PgConnection, Postgres, Row, Transaction, postgres::PgRow};
use std::{num::NonZeroU32, sync::Arc};

/// Identity state and key mutations lock the project first, then keys, consistently.
#[derive(Clone)]
pub struct PostgresProjectStore {
    database: PostgresDatabase,
    hmac_key: SecretBytes,
    max_keys: usize,
}

impl PostgresProjectStore {
    pub fn new(
        database: PostgresDatabase,
        hmac_key: SecretBytes,
        max_keys: usize,
    ) -> Result<Self, PostgresError> {
        if !(1..=1024).contains(&max_keys) {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self {
            database,
            hmac_key,
            max_keys,
        })
    }

    async fn lock_project(
        transaction: &mut Transaction<'_, Postgres>,
        id: ProjectId,
    ) -> Result<(), ProjectStoreError> {
        let exists =
            sqlx::query("SELECT id FROM metric.projects WHERE id=$1 AND state<3 FOR UPDATE")
                .bind(id.get())
                .fetch_optional(&mut **transaction)
                .await
                .map_err(error)?;
        if exists.is_none() {
            return Err(ProjectStoreError::NotFound);
        }
        Ok(())
    }

    async fn keys(
        &self,
        connection: &mut PgConnection,
        id: ProjectId,
    ) -> Result<Vec<ProjectKeyView>, ProjectStoreError> {
        let rows = sqlx::query("SELECT k.*, m.dsn AS existing_dsn FROM metric.project_keys k LEFT JOIN metric.dsn_mappings m ON m.project_id=k.project_id AND m.target_key=k.key WHERE k.project_id=$1 ORDER BY k.created_at, k.key LIMIT $2")
            .bind(id.get()).bind((self.max_keys+1) as i64).fetch_all(connection).await.map_err(error)?;
        if rows.len() > self.max_keys {
            return Err(ProjectStoreError::TooManyKeys);
        }
        rows.iter()
            .map(|row| {
                Ok(ProjectKeyView {
                    key: decode_key(get(row, "key")?)?,
                    project_id: id,
                    state: decode_key_state(get(row, "state")?)?,
                    label: ProjectKeyLabel::new(get::<String>(row, "label")?)
                        .map_err(|_| ProjectStoreError::InvalidData)?,
                    created_at: Timestamp::from_unix_millis(get(row, "created_at")?)
                        .map_err(|_| ProjectStoreError::InvalidData)?,
                    existing_dsn: get(row, "existing_dsn")?,
                })
            })
            .collect()
    }
}

impl ProjectStore for PostgresProjectStore {
    fn insert_organization(
        &self,
        organization: OrganizationIdentity,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(async move {
            sqlx::query("INSERT INTO metric.organizations(id,slug,display_name,created_at) VALUES ($1,$2,$3,$4)")
                .bind(organization.id.get() as i64).bind(organization.slug.as_str())
                .bind(organization.display_name.as_str()).bind(organization.created_at.unix_millis())
                .execute(&self.database.pool).await.map_err(error)?;
            Ok(())
        })
    }

    fn insert_project(
        &self,
        project: ProjectIdentity,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(async move {
            sqlx::query("INSERT INTO metric.projects(id,organization_id,slug,display_name,state,policy_revision,ip_policy,items,max_event_bytes,max_events_per_second,burst,grouping_revision,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
                .bind(project.id.get()).bind(project.organization_id.get() as i64)
                .bind(project.slug.as_str()).bind(project.display_name.as_str()).bind(state_code(project.state))
                .bind(revision(project.policy_revision)?).bind(ip_code(project.ip_policy)).bind(items_code(project.items))
                .bind(i64::from(project.limits.max_event_bytes.get()))
                .bind(project.limits.max_events_per_second.map(|value| i64::from(value.get())))
                .bind(project.limits.burst.map(|value| i64::from(value.get())))
                .bind(revision(project.grouping_revision)?).bind(project.created_at.unix_millis())
                .execute(&self.database.pool).await.map_err(error)?;
            Ok(())
        })
    }

    fn insert_project_key(
        &self,
        key: ProjectKeyIdentity,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(async move {
            let mut transaction = self.database.pool.begin().await.map_err(error)?;
            Self::lock_project(&mut transaction, key.project_id).await?;
            let count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM metric.project_keys WHERE project_id=$1")
                    .bind(key.project_id.get())
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(error)?;
            if count >= self.max_keys as i64 {
                return Err(ProjectStoreError::TooManyKeys);
            }
            reserve_key(&mut transaction, key.project_id, key.key).await?;
            sqlx::query("INSERT INTO metric.project_keys(key,project_id,state,label,created_at) VALUES ($1,$2,$3,$4,$5)")
                .bind(key.key.as_bytes().as_slice()).bind(key.project_id.get()).bind(key_state_code(key.state))
                .bind(key.label.as_str()).bind(key.created_at.unix_millis())
                .execute(&mut *transaction).await.map_err(error)?;
            transaction.commit().await.map_err(error)
        })
    }

    fn load_project(
        &self,
        key: DsnKey,
    ) -> PortFuture<'_, Result<ProjectSnapshot, ProjectStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT p.*, k.state AS key_state FROM metric.project_keys k JOIN metric.projects p ON p.id=k.project_id WHERE k.key=$1")
                .bind(key.as_bytes().as_slice()).fetch_optional(&self.database.pool).await.map_err(error)?
                .ok_or(ProjectStoreError::UnknownKey)?;
            let key_state = decode_key_state(get(&row, "key_state")?)?;
            if key_state != ProjectKeyState::Active {
                return Err(ProjectStoreError::NotFound);
            }
            let view = decode_project(&row)?;
            Ok(ProjectSnapshot {
                project_id: view.id,
                organization_id: view.organization_id,
                state: view.state,
                key_state,
                scrub_policy: ScrubPolicy {
                    revision: view.policy_revision,
                    ip_policy: view.ip_policy,
                    hmac_key: self.hmac_key.clone(),
                },
                items: view.items,
                limits: view.limits,
                inbound_filters: Arc::new(
                    view.inbound_filters
                        .compile()
                        .map_err(|_| ProjectStoreError::InvalidData)?,
                ),
                grouping_revision: view.grouping_revision,
            })
        })
    }

    fn set_key_state(
        &self,
        key: DsnKey,
        state: ProjectKeyState,
    ) -> PortFuture<'_, Result<ProjectId, ProjectStoreError>> {
        Box::pin(async move {
            let id: i32 =
                sqlx::query_scalar("SELECT project_id FROM metric.project_keys WHERE key=$1")
                    .bind(key.as_bytes().as_slice())
                    .fetch_optional(&self.database.pool)
                    .await
                    .map_err(error)?
                    .ok_or(ProjectStoreError::NotFound)?;
            let id = ProjectId::new(id).map_err(|_| ProjectStoreError::InvalidData)?;
            self.set_project_key_state(id, key, state).await?;
            Ok(id)
        })
    }

    fn set_project_key_state(
        &self,
        id: ProjectId,
        key: DsnKey,
        state: ProjectKeyState,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(async move {
            let mut transaction = self.database.pool.begin().await.map_err(error)?;
            Self::lock_project(&mut transaction, id).await?;
            let changed = sqlx::query(
                "UPDATE metric.project_keys SET state=$1 WHERE project_id=$2 AND key=$3",
            )
            .bind(key_state_code(state))
            .bind(id.get())
            .bind(key.as_bytes().as_slice())
            .execute(&mut *transaction)
            .await
            .map_err(error)?;
            if changed.rows_affected() != 1 {
                return Err(ProjectStoreError::NotFound);
            }
            transaction.commit().await.map_err(error)
        })
    }

    fn set_project_acceptance(
        &self,
        id: ProjectId,
        state: ProjectAcceptanceState,
    ) -> PortFuture<'_, Result<Vec<DsnKey>, ProjectStoreError>> {
        Box::pin(async move {
            if matches!(
                state,
                ProjectAcceptanceState::Purging | ProjectAcceptanceState::Deleted
            ) {
                return Err(ProjectStoreError::InvalidData);
            }
            let mut transaction = self.database.pool.begin().await.map_err(error)?;
            Self::lock_project(&mut transaction, id).await?;
            let keys = self
                .keys(&mut transaction, id)
                .await?
                .into_iter()
                .map(|key| key.key)
                .collect();
            match state {
                ProjectAcceptanceState::Active => {
                    sqlx::query(
                        "UPDATE metric.project_keys SET state=0 WHERE project_id=$1 AND state=2",
                    )
                    .bind(id.get())
                    .execute(&mut *transaction)
                    .await
                    .map_err(error)?;
                }
                ProjectAcceptanceState::PendingDelete => {
                    sqlx::query(
                        "UPDATE metric.project_keys SET state=2 WHERE project_id=$1 AND state=0",
                    )
                    .bind(id.get())
                    .execute(&mut *transaction)
                    .await
                    .map_err(error)?;
                }
                _ => {}
            }
            sqlx::query("UPDATE metric.projects SET state=$1 WHERE id=$2")
                .bind(state_code(state))
                .bind(id.get())
                .execute(&mut *transaction)
                .await
                .map_err(error)?;
            transaction.commit().await.map_err(error)?;
            Ok(keys)
        })
    }

    fn list_projects(
        &self,
        organization: OrganizationId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ProjectView>, ProjectStoreError>> {
        Box::pin(async move {
            if !(1..=100).contains(&limit) {
                return Err(ProjectStoreError::InvalidData);
            }
            let rows = sqlx::query("SELECT * FROM metric.projects WHERE organization_id=$1 AND state<>4 ORDER BY slug,id LIMIT $2")
                .bind(organization.get() as i64).bind(limit as i64).fetch_all(&self.database.pool).await.map_err(error)?;
            rows.iter().map(decode_project).collect()
        })
    }

    fn load_project_by_id(
        &self,
        id: ProjectId,
    ) -> PortFuture<'_, Result<ProjectView, ProjectStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM metric.projects WHERE id=$1 AND state<>4")
                .bind(id.get())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(error)?
                .ok_or(ProjectStoreError::NotFound)?;
            decode_project(&row)
        })
    }

    fn list_project_keys(
        &self,
        id: ProjectId,
    ) -> PortFuture<'_, Result<Vec<ProjectKeyView>, ProjectStoreError>> {
        Box::pin(async move {
            let mut connection = self.database.pool.acquire().await.map_err(error)?;
            self.keys(&mut connection, id).await
        })
    }

    fn update_project_policy(
        &self,
        id: ProjectId,
        update: ProjectPolicyUpdate,
    ) -> PortFuture<'_, Result<(ProjectView, Vec<DsnKey>), ProjectStoreError>> {
        Box::pin(async move {
            let expected = revision(update.expected_revision)?;
            let next = expected
                .checked_add(1)
                .ok_or(ProjectStoreError::InvalidData)?;
            let filters = encode_filters(&update.inbound_filters)?;
            let mut transaction = self.database.pool.begin().await.map_err(error)?;
            Self::lock_project(&mut transaction, id).await?;
            let row = sqlx::query("UPDATE metric.projects SET policy_revision=$1,ip_policy=$2,items=$3,max_event_bytes=$4,max_events_per_second=$5,burst=$6,inbound_filters=$7 WHERE id=$8 AND policy_revision=$9 RETURNING *")
                .bind(next).bind(ip_code(update.ip_policy)).bind(items_code(update.items))
                .bind(i64::from(update.limits.max_event_bytes.get()))
                .bind(update.limits.max_events_per_second.map(|value| i64::from(value.get())))
                .bind(update.limits.burst.map(|value| i64::from(value.get())))
                .bind(filters).bind(id.get()).bind(expected)
                .fetch_optional(&mut *transaction).await.map_err(error)?.ok_or(ProjectStoreError::RevisionConflict)?;
            let project = decode_project(&row)?;
            let keys = self
                .keys(&mut transaction, id)
                .await?
                .into_iter()
                .map(|key| key.key)
                .collect();
            transaction.commit().await.map_err(error)?;
            Ok((project, keys))
        })
    }

    fn insert_dsn_mapping(
        &self,
        id: ProjectId,
        mapping: DsnMapping,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(async move {
            let mut transaction = self.database.pool.begin().await.map_err(error)?;
            Self::lock_project(&mut transaction, id).await?;
            let target: Option<(i32, i16)> =
                sqlx::query_as("SELECT project_id,state FROM metric.project_keys WHERE key=$1")
                    .bind(mapping.target_key.as_bytes().as_slice())
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(error)?;
            let Some((owner, state)) = target else {
                return Err(ProjectStoreError::UnknownKey);
            };
            if owner != id.get() {
                return Err(ProjectStoreError::InvalidData);
            }
            if state != 0 {
                return Err(ProjectStoreError::NotFound);
            }
            let validated = ExistingDsn::parse(&mapping.source.public_dsn)
                .map_err(|_| ProjectStoreError::InvalidData)?;
            if validated != mapping.source {
                return Err(ProjectStoreError::InvalidData);
            }
            reserve_key(&mut transaction, id, mapping.source.key).await?;
            sqlx::query("INSERT INTO metric.dsn_mappings(source_key,project_id,target_key,dsn) VALUES ($1,$2,$3,$4)")
                .bind(mapping.source.key.as_bytes().as_slice()).bind(id.get()).bind(mapping.target_key.as_bytes().as_slice()).bind(mapping.source.public_dsn)
                .execute(&mut *transaction).await.map_err(|error_value| if is_unique(&error_value) { ProjectStoreError::KeyCollision } else { error(error_value) })?;
            transaction.commit().await.map_err(error)
        })
    }

    fn load_dsn_mapping(
        &self,
        key: DsnKey,
    ) -> PortFuture<'_, Result<DsnMapping, ProjectStoreError>> {
        Box::pin(async move {
            let row =
                sqlx::query("SELECT dsn,target_key FROM metric.dsn_mappings WHERE source_key=$1")
                    .bind(key.as_bytes().as_slice())
                    .fetch_optional(&self.database.pool)
                    .await
                    .map_err(error)?
                    .ok_or(ProjectStoreError::NotFound)?;
            let source = ExistingDsn::parse(&get::<String>(&row, "dsn")?)
                .map_err(|_| ProjectStoreError::InvalidData)?;
            if source.key != key {
                return Err(ProjectStoreError::InvalidData);
            }
            Ok(DsnMapping {
                source,
                target_key: decode_key(get(&row, "target_key")?)?,
            })
        })
    }
}

async fn reserve_key(
    transaction: &mut Transaction<'_, Postgres>,
    id: ProjectId,
    key: DsnKey,
) -> Result<(), ProjectStoreError> {
    sqlx::query("INSERT INTO metric.reserved_keys(key,project_id) VALUES ($1,$2)")
        .bind(key.as_bytes().as_slice())
        .bind(id.get())
        .execute(&mut **transaction)
        .await
        .map_err(|value| {
            if is_unique(&value) {
                ProjectStoreError::KeyCollision
            } else {
                error(value)
            }
        })?;
    Ok(())
}

fn is_unique(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|error| error.code().as_deref() == Some("23505"))
}

fn error(value: sqlx::Error) -> ProjectStoreError {
    if let Some(database) = value.as_database_error() {
        match database.code().as_deref() {
            Some("23505") => {
                return match database.constraint() {
                    Some("organization_slug_unique") => ProjectStoreError::OrganizationSlugExists,
                    Some("project_organization_slug_unique") => {
                        ProjectStoreError::ProjectSlugExists
                    }
                    _ => ProjectStoreError::IdentityCollision,
                };
            }
            Some("23503") => return ProjectStoreError::NotFound,
            Some("23514") => return ProjectStoreError::InvalidData,
            _ => {}
        }
    }
    ProjectStoreError::Unavailable
}

fn get<T>(row: &PgRow, name: &str) -> Result<T, ProjectStoreError>
where
    T: for<'a> sqlx::Decode<'a, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get(name)
        .map_err(|_| ProjectStoreError::InvalidData)
}

fn revision(value: u64) -> Result<i64, ProjectStoreError> {
    i64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ProjectStoreError::InvalidData)
}

fn decode_key(value: Vec<u8>) -> Result<DsnKey, ProjectStoreError> {
    Ok(DsnKey::from_bytes(
        value
            .try_into()
            .map_err(|_| ProjectStoreError::InvalidData)?,
    ))
}

fn state_code(value: ProjectAcceptanceState) -> i16 {
    match value {
        ProjectAcceptanceState::Active => 0,
        ProjectAcceptanceState::Disabled => 1,
        ProjectAcceptanceState::PendingDelete => 2,
        ProjectAcceptanceState::Purging => 3,
        ProjectAcceptanceState::Deleted => 4,
    }
}
fn key_state_code(value: ProjectKeyState) -> i16 {
    match value {
        ProjectKeyState::Active => 0,
        ProjectKeyState::Disabled => 1,
        ProjectKeyState::SuspendedByDeletion => 2,
    }
}
fn decode_key_state(value: i16) -> Result<ProjectKeyState, ProjectStoreError> {
    match value {
        0 => Ok(ProjectKeyState::Active),
        1 => Ok(ProjectKeyState::Disabled),
        2 => Ok(ProjectKeyState::SuspendedByDeletion),
        _ => Err(ProjectStoreError::InvalidData),
    }
}
fn ip_code(value: IpScrubPolicy) -> i16 {
    match value {
        IpScrubPolicy::Hmac => 0,
        IpScrubPolicy::Keep => 1,
        IpScrubPolicy::Remove => 2,
        IpScrubPolicy::Truncate => 3,
    }
}
fn items_code(items: ItemCapabilities) -> i32 {
    [
        items.error,
        items.client_report,
        items.log,
        items.transaction,
        items.span,
        items.feedback,
        items.check_in,
        items.metric,
        items.replay,
    ]
    .into_iter()
    .enumerate()
    .fold(0, |code, (bit, enabled)| code | (i32::from(enabled) << bit))
}

fn decode_project(row: &PgRow) -> Result<ProjectView, ProjectStoreError> {
    let code: i32 = get(row, "items")?;
    let enabled = |bit| code & (1 << bit) != 0;
    let limits_value = |name| -> Result<NonZeroU32, ProjectStoreError> {
        u32::try_from(get::<i64>(row, name)?)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or(ProjectStoreError::InvalidData)
    };
    let optional_limit = |name| -> Result<Option<NonZeroU32>, ProjectStoreError> {
        get::<Option<i64>>(row, name)?
            .map(|value| {
                u32::try_from(value)
                    .ok()
                    .and_then(NonZeroU32::new)
                    .ok_or(ProjectStoreError::InvalidData)
            })
            .transpose()
    };
    let positive = |name| -> Result<u64, ProjectStoreError> {
        u64::try_from(get::<i64>(row, name)?)
            .ok()
            .filter(|value| *value > 0)
            .ok_or(ProjectStoreError::InvalidData)
    };
    Ok(ProjectView {
        id: ProjectId::new(get(row, "id")?).map_err(|_| ProjectStoreError::InvalidData)?,
        organization_id: OrganizationId::new(positive("organization_id")?)
            .map_err(|_| ProjectStoreError::InvalidData)?,
        slug: Slug::new(get::<String>(row, "slug")?).map_err(|_| ProjectStoreError::InvalidData)?,
        display_name: DisplayName::new(get::<String>(row, "display_name")?)
            .map_err(|_| ProjectStoreError::InvalidData)?,
        state: match get::<i16>(row, "state")? {
            0 => ProjectAcceptanceState::Active,
            1 => ProjectAcceptanceState::Disabled,
            2 => ProjectAcceptanceState::PendingDelete,
            3 => ProjectAcceptanceState::Purging,
            4 => ProjectAcceptanceState::Deleted,
            _ => return Err(ProjectStoreError::InvalidData),
        },
        policy_revision: positive("policy_revision")?,
        ip_policy: match get::<i16>(row, "ip_policy")? {
            0 => IpScrubPolicy::Hmac,
            1 => IpScrubPolicy::Keep,
            2 => IpScrubPolicy::Remove,
            3 => IpScrubPolicy::Truncate,
            _ => return Err(ProjectStoreError::InvalidData),
        },
        items: ItemCapabilities {
            error: enabled(0),
            client_report: enabled(1),
            log: enabled(2),
            transaction: enabled(3),
            span: enabled(4),
            feedback: enabled(5),
            check_in: enabled(6),
            metric: enabled(7),
            replay: enabled(8),
        },
        limits: ProjectIngestLimits {
            max_event_bytes: limits_value("max_event_bytes")?,
            max_events_per_second: optional_limit("max_events_per_second")?,
            burst: optional_limit("burst")?,
        },
        inbound_filters: decode_filters(get(row, "inbound_filters")?)?,
        grouping_revision: positive("grouping_revision")?,
        created_at: Timestamp::from_unix_millis(get(row, "created_at")?)
            .map_err(|_| ProjectStoreError::InvalidData)?,
    })
}

fn encode_filters(policy: &InboundFilterPolicy) -> Result<Value, ProjectStoreError> {
    policy
        .compile()
        .map_err(|_| ProjectStoreError::InvalidData)?;
    Ok(Value::Array(
        policy
            .rules()
            .iter()
            .map(|rule| {
                json!([
                    rule.signal.as_str(),
                    rule.field.as_str(),
                    rule.operation.as_str(),
                    rule.pattern.as_ref()
                ])
            })
            .collect(),
    ))
}

fn decode_filters(value: Value) -> Result<InboundFilterPolicy, ProjectStoreError> {
    let rows = value.as_array().ok_or(ProjectStoreError::InvalidData)?;
    if rows.len() > MAX_INBOUND_FILTER_RULES {
        return Err(ProjectStoreError::InvalidData);
    }
    let mut rules = Vec::with_capacity(rows.len());
    for row in rows {
        let fields = row
            .as_array()
            .filter(|fields| fields.len() == 4)
            .ok_or(ProjectStoreError::InvalidData)?;
        let text = |index: usize| fields[index].as_str().ok_or(ProjectStoreError::InvalidData);
        let signal = [
            InboundFilterSignal::Error,
            InboundFilterSignal::Log,
            InboundFilterSignal::Transaction,
            InboundFilterSignal::Span,
        ]
        .into_iter()
        .find(|value| value.as_str() == text(0).unwrap_or(""))
        .ok_or(ProjectStoreError::InvalidData)?;
        let field = [
            InboundFilterField::Release,
            InboundFilterField::Environment,
            InboundFilterField::Service,
            InboundFilterField::Message,
            InboundFilterField::ExceptionType,
            InboundFilterField::Logger,
            InboundFilterField::RequestHost,
            InboundFilterField::RequestPath,
            InboundFilterField::Severity,
            InboundFilterField::Name,
            InboundFilterField::Operation,
            InboundFilterField::Status,
            InboundFilterField::Duration,
        ]
        .into_iter()
        .find(|value| value.as_str() == text(1).unwrap_or(""))
        .ok_or(ProjectStoreError::InvalidData)?;
        let operation = [
            InboundFilterOperation::Exact,
            InboundFilterOperation::Prefix,
            InboundFilterOperation::Suffix,
            InboundFilterOperation::Contains,
            InboundFilterOperation::Glob,
        ]
        .into_iter()
        .find(|value| value.as_str() == text(2).unwrap_or(""))
        .ok_or(ProjectStoreError::InvalidData)?;
        rules.push(InboundFilterRule {
            signal,
            field,
            operation,
            pattern: text(3)?.into(),
        });
    }
    InboundFilterPolicy::new(rules).map_err(|_| ProjectStoreError::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MigrationPolicy, PostgresSchemaManager, test_support::Fixture};

    fn now() -> Timestamp {
        Timestamp::from_unix_millis(1_700_000_000_000).unwrap()
    }
    fn project(id: i32, organization: u64) -> ProjectIdentity {
        crate::test_support::project_identity(id, organization)
    }
    fn key(id: i32, byte: u8) -> ProjectKeyIdentity {
        ProjectKeyIdentity {
            key: DsnKey::from_bytes([byte; 16]),
            project_id: ProjectId::new(id).unwrap(),
            state: ProjectKeyState::Active,
            label: ProjectKeyLabel::new("' OR 1=1; --").unwrap(),
            created_at: now(),
        }
    }
    async fn setup(max_keys: usize) -> (Fixture, PostgresDatabase, PostgresProjectStore) {
        let fixture = Fixture::new().await;
        PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
            .await
            .unwrap();
        let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        let store = crate::test_support::seed_projects(database.clone(), max_keys).await;
        (fixture, database, store)
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn identity_round_trip_scope_states_and_bound_values() {
        let (fixture, database, store) = setup(32).await;
        let first = project(1, 1);
        let primary = key(1, 10);
        let other = key(2, 20);
        store.insert_project_key(primary.clone()).await.unwrap();
        store.insert_project_key(other.clone()).await.unwrap();
        let view = store.load_project_by_id(first.id).await.unwrap();
        assert_eq!(view.display_name, first.display_name);
        assert_eq!(view.items, first.items);
        let snapshot = store.load_project(primary.key).await.unwrap();
        assert_eq!(snapshot.organization_id, first.organization_id);
        assert_eq!(snapshot.scrub_policy.hmac_key, SecretBytes::new([42; 32]));
        assert_eq!(
            store.list_project_keys(first.id).await.unwrap()[0].label,
            primary.label
        );
        assert_eq!(
            store
                .list_projects(OrganizationId::new(2).unwrap(), 100)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .set_project_key_state(first.id, other.key, ProjectKeyState::Disabled)
                .await,
            Err(ProjectStoreError::NotFound)
        );
        assert!(store.load_project(other.key).await.is_ok());
        store
            .set_key_state(primary.key, ProjectKeyState::Disabled)
            .await
            .unwrap();
        assert_eq!(
            store.load_project(primary.key).await,
            Err(ProjectStoreError::NotFound)
        );
        store
            .set_project_key_state(first.id, primary.key, ProjectKeyState::Active)
            .await
            .unwrap();
        store
            .set_project_acceptance(first.id, ProjectAcceptanceState::PendingDelete)
            .await
            .unwrap();
        assert_eq!(
            store.list_project_keys(first.id).await.unwrap()[0].state,
            ProjectKeyState::SuspendedByDeletion
        );
        store
            .set_project_acceptance(first.id, ProjectAcceptanceState::Active)
            .await
            .unwrap();
        assert!(store.load_project(primary.key).await.is_ok());
        assert_eq!(
            store
                .set_project_acceptance(first.id, ProjectAcceptanceState::Purging)
                .await,
            Err(ProjectStoreError::InvalidData)
        );
        assert_eq!(
            store.list_projects(first.organization_id, 0).await,
            Err(ProjectStoreError::InvalidData)
        );
        let duplicate_slug = ProjectIdentity {
            id: ProjectId::new(3).unwrap(),
            ..first.clone()
        };
        assert_eq!(
            store.insert_project(duplicate_slug).await,
            Err(ProjectStoreError::ProjectSlugExists)
        );
        assert_eq!(
            store.insert_project(first).await,
            Err(ProjectStoreError::IdentityCollision)
        );
        database.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn policy_cas_filters_and_key_limit_are_atomic() {
        let (fixture, database, store) = setup(1).await;
        let one = key(1, 10);
        let two = key(1, 11);
        let (a, b) = tokio::join!(store.insert_project_key(one), store.insert_project_key(two));
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(
            a == Err(ProjectStoreError::TooManyKeys) || b == Err(ProjectStoreError::TooManyKeys)
        );
        let id = ProjectId::new(1).unwrap();
        let original = store.load_project_by_id(id).await.unwrap();
        let update = ProjectPolicyUpdate {
            expected_revision: 1,
            ip_policy: IpScrubPolicy::Remove,
            items: original.items,
            limits: original.limits,
            inbound_filters: InboundFilterPolicy::new(vec![InboundFilterRule {
                signal: InboundFilterSignal::Error,
                field: InboundFilterField::Message,
                operation: InboundFilterOperation::Contains,
                pattern: "'; SELECT * FROM metric.projects; -- %_".into(),
            }])
            .unwrap(),
        };
        let (a, b) = tokio::join!(
            store.update_project_policy(id, update.clone()),
            store.update_project_policy(id, update.clone())
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(
            a == Err(ProjectStoreError::RevisionConflict)
                || b == Err(ProjectStoreError::RevisionConflict)
        );
        let current = store.load_project_by_id(id).await.unwrap();
        assert_eq!(current.policy_revision, 2);
        assert_eq!(current.inbound_filters, update.inbound_filters);
        let persisted = store
            .load_project(store.list_project_keys(id).await.unwrap()[0].key)
            .await
            .unwrap();
        assert!(!persisted.inbound_filters.is_empty());
        assert_eq!(
            store
                .load_project_by_id(ProjectId::new(2).unwrap())
                .await
                .unwrap()
                .policy_revision,
            1
        );
        database.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn dsn_alias_and_native_key_reservations_share_one_namespace() {
        let (fixture, database, store) = setup(32).await;
        let target = key(1, 10);
        store.insert_project_key(target.clone()).await.unwrap();
        let source = ExistingDsn::parse(&format!(
            "http://{}@localhost/999999999999",
            DsnKey::from_bytes([30; 16])
        ))
        .unwrap();
        let mapping = DsnMapping {
            source: source.clone(),
            target_key: target.key,
        };
        assert_eq!(
            store
                .insert_dsn_mapping(ProjectId::new(2).unwrap(), mapping.clone())
                .await,
            Err(ProjectStoreError::InvalidData)
        );
        let conflicting = key(2, 30);
        let (alias, native) = tokio::join!(
            store.insert_dsn_mapping(target.project_id, mapping.clone()),
            store.insert_project_key(conflicting)
        );
        assert_eq!(usize::from(alias.is_ok()) + usize::from(native.is_ok()), 1);
        assert!(
            alias == Err(ProjectStoreError::KeyCollision)
                || native == Err(ProjectStoreError::KeyCollision)
        );
        let source2 = ExistingDsn::parse(&format!(
            "https://{}@example.invalid/18446744073709551615",
            DsnKey::from_bytes([31; 16])
        ))
        .unwrap();
        let mapping2 = DsnMapping {
            source: source2.clone(),
            target_key: target.key,
        };
        if alias.is_err() {
            store
                .insert_dsn_mapping(target.project_id, mapping2.clone())
                .await
                .unwrap();
            assert_eq!(store.load_dsn_mapping(source2.key).await.unwrap(), mapping2);
        } else {
            assert_eq!(store.load_dsn_mapping(source.key).await.unwrap(), mapping);
        }
        let displayed = store.list_project_keys(target.project_id).await.unwrap();
        assert!(displayed[0].existing_dsn.is_some());
        assert!(
            store
                .list_project_keys(ProjectId::new(2).unwrap())
                .await
                .unwrap()
                .iter()
                .all(|key| key.existing_dsn.is_none())
        );
        database.close().await;
        fixture.cleanup().await;
    }
}
