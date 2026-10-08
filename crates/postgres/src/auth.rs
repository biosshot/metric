use crate::PostgresDatabase;
use metric_domain::{
    api::{ApiTokenView, AuditLogView, OrganizationMemberView},
    auth::*,
    *,
};
use metric_ports::{AuthStore, AuthStoreError, BootstrapTokenInstall, PortFuture};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};

// Control-plane identity changes share a transaction advisory lock. This serializes
// membership changes with global user disabling, including new memberships created
// concurrently. Hot ingest/query/credential lookups never take this lock.
const IDENTITY_LOCK: i64 = 0x4d45_5452_4943_4155;

#[derive(Clone)]
pub struct PostgresAuthStore {
    database: PostgresDatabase,
}

impl PostgresAuthStore {
    #[must_use]
    pub fn new(database: PostgresDatabase) -> Self {
        Self { database }
    }

    async fn identity_transaction(&self) -> Result<Transaction<'_, Postgres>, AuthStoreError> {
        let mut transaction = self.database.pool.begin().await.map_err(error)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(IDENTITY_LOCK)
            .execute(&mut *transaction)
            .await
            .map_err(error)?;
        Ok(transaction)
    }
}

impl AuthStore for PostgresAuthStore {
    fn install_bootstrap_token(
        &self,
        token: SetupToken,
    ) -> PortFuture<'_, Result<BootstrapTokenInstall, AuthStoreError>> {
        Box::pin(async move {
            if token.purpose != SetupPurpose::Bootstrap || token.user_id.is_some() {
                return Err(AuthStoreError::InvalidData);
            }
            let mut tx = self.identity_transaction().await?;
            let users: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metric.users)")
                .fetch_one(&mut *tx)
                .await
                .map_err(error)?;
            if users {
                return Ok(BootstrapTokenInstall::Closed);
            }
            let installed: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM metric.setup_tokens WHERE purpose=0)",
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(error)?;
            if installed {
                return Ok(BootstrapTokenInstall::AlreadyInstalled);
            }
            insert_setup(&mut tx, &token).await?;
            tx.commit().await.map_err(error)?;
            Ok(BootstrapTokenInstall::Created)
        })
    }

    fn consume_bootstrap(
        &self,
        identity: BootstrapIdentity,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            if identity.membership.organization_id != identity.organization_id
                || identity.membership.user_id != identity.user.id
                || identity.membership.role != OrganizationRole::Owner
                || identity.membership.created_by != identity.user.id
            {
                return Err(AuthStoreError::InvalidData);
            }
            let mut tx = self.identity_transaction().await?;
            let token = sqlx::query("SELECT consumed_at,operation_id FROM metric.setup_tokens WHERE digest=$1 AND purpose=0 AND expires_at>$2 FOR UPDATE")
                .bind(identity.token_digest.expose().as_slice()).bind(identity.timestamp.unix_millis())
                .fetch_optional(&mut *tx).await.map_err(error)?.ok_or(AuthStoreError::InvalidCredential)?;
            let consumed: Option<i64> = get(&token, "consumed_at")?;
            if consumed.is_some() {
                if get::<Option<i64>>(&token, "operation_id")?
                    != Some(identity.operation_id.get() as i64)
                {
                    return Err(AuthStoreError::InvalidCredential);
                }
                let user = sqlx::query("SELECT * FROM metric.users WHERE id=$1")
                    .bind(identity.user.id.get() as i64)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(error)?
                    .ok_or(AuthStoreError::InvalidData)?;
                let membership = sqlx::query("SELECT * FROM metric.organization_memberships WHERE organization_id=$1 AND user_id=$2")
                    .bind(identity.organization_id.get() as i64).bind(identity.user.id.get() as i64).fetch_optional(&mut *tx).await.map_err(error)?.ok_or(AuthStoreError::InvalidData)?;
                let org = sqlx::query("SELECT * FROM metric.organizations WHERE id=$1")
                    .bind(identity.organization_id.get() as i64)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(error)?
                    .ok_or(AuthStoreError::InvalidData)?;
                if decode_user(&user)? != identity.user
                    || decode_membership(&membership)? != identity.membership
                    || get::<String>(&org, "slug")? != identity.organization_slug.as_str()
                    || get::<String>(&org, "display_name")? != identity.organization_name.as_str()
                {
                    return Err(AuthStoreError::InvalidData);
                }
                return Ok(());
            }
            let closed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metric.users)")
                .fetch_one(&mut *tx)
                .await
                .map_err(error)?;
            if closed {
                return Err(AuthStoreError::BootstrapClosed);
            }
            insert_organization(
                &mut tx,
                &OrganizationIdentity {
                    id: identity.organization_id,
                    slug: identity.organization_slug,
                    display_name: identity.organization_name,
                    created_at: identity.timestamp,
                },
            )
            .await?;
            insert_user(&mut tx, &identity.user).await?;
            insert_membership(&mut tx, &identity.membership).await?;
            sqlx::query(
                "UPDATE metric.setup_tokens SET consumed_at=$1,operation_id=$2 WHERE digest=$3",
            )
            .bind(identity.timestamp.unix_millis())
            .bind(identity.operation_id.get() as i64)
            .bind(identity.token_digest.expose().as_slice())
            .execute(&mut *tx)
            .await
            .map_err(error)?;
            tx.commit().await.map_err(error)
        })
    }

    fn create_owned_organization(
        &self,
        organization: OrganizationIdentity,
        membership: OrganizationMembership,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            if organization.id != membership.organization_id
                || membership.role != OrganizationRole::Owner
            {
                return Err(AuthStoreError::InvalidData);
            }
            let mut tx = self.identity_transaction().await?;
            insert_organization(&mut tx, &organization).await?;
            insert_membership(&mut tx, &membership).await?;
            tx.commit().await.map_err(error)
        })
    }

    fn create_invited_user(
        &self,
        user: UserAccount,
        membership: OrganizationMembership,
        token: SetupToken,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            if membership.user_id != user.id
                || token.user_id != Some(user.id)
                || token.purpose != SetupPurpose::PasswordSetup
            {
                return Err(AuthStoreError::InvalidData);
            }
            let mut tx = self.identity_transaction().await?;
            insert_user(&mut tx, &user).await?;
            insert_membership(&mut tx, &membership).await?;
            insert_setup(&mut tx, &token).await?;
            tx.commit().await.map_err(error)
        })
    }

    fn create_password_setup_token(
        &self,
        token: SetupToken,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let user = token.user_id.ok_or(AuthStoreError::InvalidData)?;
            if token.purpose == SetupPurpose::Bootstrap {
                return Err(AuthStoreError::InvalidData);
            }
            let mut tx = self.identity_transaction().await?;
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metric.users WHERE id=$1)")
                    .bind(user.get() as i64)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(error)?;
            if !exists {
                return Err(AuthStoreError::NotFound);
            }
            if token.purpose == SetupPurpose::PasswordSetup {
                let changed = sqlx::query("UPDATE metric.setup_tokens SET digest=$1,created_at=$2,expires_at=$3,operation_id=NULL WHERE user_id=$4 AND purpose=1 AND consumed_at IS NULL")
                    .bind(token.digest.expose().as_slice()).bind(token.created_at.unix_millis()).bind(token.expires_at.unix_millis())
                    .bind(user.get() as i64).execute(&mut *tx).await.map_err(error)?;
                if changed.rows_affected() == 1 {
                    return tx.commit().await.map_err(error);
                }
            }
            insert_setup(&mut tx, &token).await?;
            tx.commit().await.map_err(error)
        })
    }

    fn consume_password_setup(
        &self,
        digest: SecretDigest,
        now: Timestamp,
        hash: PasswordHash,
    ) -> PortFuture<'_, Result<UserId, AuthStoreError>> {
        Box::pin(async move {
            let mut tx = self.identity_transaction().await?;
            let user: i64 = sqlx::query_scalar("UPDATE metric.setup_tokens SET consumed_at=$1 WHERE digest=$2 AND purpose IN (1,2) AND expires_at>$1 AND consumed_at IS NULL RETURNING user_id")
                .bind(now.unix_millis()).bind(digest.expose().as_slice()).fetch_optional(&mut *tx).await.map_err(error)?.ok_or(AuthStoreError::InvalidCredential)?;
            let changed = sqlx::query(
                "UPDATE metric.users SET password_hash=$1,password_changed_at=$2 WHERE id=$3",
            )
            .bind(hash.expose())
            .bind(now.unix_millis())
            .bind(user)
            .execute(&mut *tx)
            .await
            .map_err(error)?;
            matched(changed.rows_affected())?;
            tx.commit().await.map_err(error)?;
            user_id(user)
        })
    }

    fn load_user_by_email<'a>(
        &'a self,
        email: &'a EmailAddress,
    ) -> PortFuture<'a, Result<UserAccount, AuthStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM metric.users WHERE canonical_email=$1")
                .bind(email.canonical())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(error)?
                .ok_or(AuthStoreError::NotFound)?;
            decode_user(&row)
        })
    }
    fn load_user(&self, id: UserId) -> PortFuture<'_, Result<UserAccount, AuthStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM metric.users WHERE id=$1")
                .bind(id.get() as i64)
                .fetch_optional(&self.database.pool)
                .await
                .map_err(error)?
                .ok_or(AuthStoreError::NotFound)?;
            decode_user(&row)
        })
    }
    fn update_password_hash(
        &self,
        id: UserId,
        hash: PasswordHash,
        changed_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            matched(
                sqlx::query(
                    "UPDATE metric.users SET password_hash=$1,password_changed_at=$2 WHERE id=$3",
                )
                .bind(hash.expose())
                .bind(changed_at.unix_millis())
                .bind(id.get() as i64)
                .execute(&self.database.pool)
                .await
                .map_err(error)?
                .rows_affected(),
            )
        })
    }
    fn load_membership(
        &self,
        user: UserId,
        organization: OrganizationId,
    ) -> PortFuture<'_, Result<OrganizationMembership, AuthStoreError>> {
        Box::pin(async move {
            let row=sqlx::query("SELECT * FROM metric.organization_memberships WHERE user_id=$1 AND organization_id=$2")
                .bind(user.get() as i64).bind(organization.get() as i64).fetch_optional(&self.database.pool).await.map_err(error)?.ok_or(AuthStoreError::NotFound)?;
            decode_membership(&row)
        })
    }
    fn list_user_memberships(
        &self,
        user: UserId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<OrganizationMembership>, AuthStoreError>> {
        Box::pin(async move {
            let rows=sqlx::query("SELECT * FROM metric.organization_memberships WHERE user_id=$1 ORDER BY organization_id LIMIT $2")
                .bind(user.get() as i64).bind(bound(limit)?).fetch_all(&self.database.pool).await.map_err(error)?;
            rows.iter().map(decode_membership).collect()
        })
    }
    fn mutate_membership(
        &self,
        mutation: MembershipMutation,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let mut tx = self.identity_transaction().await?;
            let existing:Option<i16>=sqlx::query_scalar("SELECT role FROM metric.organization_memberships WHERE organization_id=$1 AND user_id=$2")
                .bind(mutation.organization_id.get() as i64).bind(mutation.user_id.get() as i64).fetch_optional(&mut *tx).await.map_err(error)?;
            let remains = matches!(
                mutation.kind,
                MembershipMutationKind::Create(OrganizationRole::Owner)
                    | MembershipMutationKind::ChangeRole(OrganizationRole::Owner)
            );
            if existing == Some(0) && !remains {
                ensure_active_owner_survives(&mut tx, mutation.organization_id, mutation.user_id)
                    .await?;
            }
            match mutation.kind {
                MembershipMutationKind::Create(role)=>insert_membership(&mut tx,&OrganizationMembership { organization_id:mutation.organization_id,user_id:mutation.user_id,role,created_at:mutation.timestamp,created_by:mutation.actor_user_id }).await?,
                MembershipMutationKind::ChangeRole(role)=>matched(sqlx::query("UPDATE metric.organization_memberships SET role=$1 WHERE organization_id=$2 AND user_id=$3")
                    .bind(role_code(role)).bind(mutation.organization_id.get() as i64).bind(mutation.user_id.get() as i64).execute(&mut *tx).await.map_err(error)?.rows_affected())?,
                MembershipMutationKind::Remove=>matched(sqlx::query("DELETE FROM metric.organization_memberships WHERE organization_id=$1 AND user_id=$2")
                    .bind(mutation.organization_id.get() as i64).bind(mutation.user_id.get() as i64).execute(&mut *tx).await.map_err(error)?.rows_affected())?,
            }
            tx.commit().await.map_err(error)
        })
    }
    fn set_user_disabled(
        &self,
        user: UserId,
        disabled_at: Option<Timestamp>,
        _operation_id: CredentialId,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let mut tx = self.identity_transaction().await?;
            if disabled_at.is_some() {
                let organizations:Vec<i64>=sqlx::query_scalar("SELECT organization_id FROM metric.organization_memberships WHERE user_id=$1 AND role=0 ORDER BY organization_id")
                    .bind(user.get() as i64).fetch_all(&mut *tx).await.map_err(error)?;
                for organization in organizations {
                    ensure_active_owner_survives(&mut tx, organization_id(organization)?, user)
                        .await?;
                }
            }
            matched(
                sqlx::query("UPDATE metric.users SET disabled_at=$1 WHERE id=$2")
                    .bind(disabled_at.map(Timestamp::unix_millis))
                    .bind(user.get() as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(error)?
                    .rows_affected(),
            )?;
            tx.commit().await.map_err(error)
        })
    }
    fn create_session(&self, session: WebSession) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            sqlx::query("INSERT INTO metric.web_sessions(id,digest,csrf_digest,user_id,created_at,last_seen_at,idle_expires_at,absolute_expires_at,revoked_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                .bind(session.id.get() as i64).bind(session.digest.expose().as_slice()).bind(session.csrf_digest.expose().as_slice()).bind(session.user_id.get() as i64)
                .bind(session.created_at.unix_millis()).bind(session.last_seen_at.unix_millis()).bind(session.idle_expires_at.unix_millis()).bind(session.absolute_expires_at.unix_millis()).bind(session.revoked_at.map(Timestamp::unix_millis))
                .execute(&self.database.pool).await.map_err(error)?;
            Ok(())
        })
    }
    fn load_session(
        &self,
        digest: SecretDigest,
    ) -> PortFuture<'_, Result<WebSession, AuthStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM metric.web_sessions WHERE digest=$1")
                .bind(digest.expose().as_slice())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(error)?
                .ok_or(AuthStoreError::NotFound)?;
            decode_session(&row)
        })
    }
    fn touch_session(
        &self,
        id: CredentialId,
        last_seen: Timestamp,
        idle_expires: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            matched(
                sqlx::query(
                    "UPDATE metric.web_sessions SET last_seen_at=$1,idle_expires_at=$2 WHERE id=$3",
                )
                .bind(last_seen.unix_millis())
                .bind(idle_expires.unix_millis())
                .bind(id.get() as i64)
                .execute(&self.database.pool)
                .await
                .map_err(error)?
                .rows_affected(),
            )
        })
    }
    fn revoke_session(
        &self,
        digest: SecretDigest,
        at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            matched(sqlx::query("UPDATE metric.web_sessions SET revoked_at=$1 WHERE digest=$2 AND revoked_at IS NULL")
            .bind(at.unix_millis()).bind(digest.expose().as_slice()).execute(&self.database.pool).await.map_err(error)?.rows_affected())
        })
    }
    fn revoke_user_sessions(
        &self,
        user: UserId,
        at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            sqlx::query("UPDATE metric.web_sessions SET revoked_at=$1 WHERE user_id=$2 AND revoked_at IS NULL").bind(at.unix_millis()).bind(user.get() as i64).execute(&self.database.pool).await.map_err(error)?;
            Ok(())
        })
    }
    fn create_api_token(&self, token: ApiToken) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let scopes = token
                .scopes
                .iter()
                .fold(0_i32, |bits, permission| bits | (1 << permission as u8));
            sqlx::query("INSERT INTO metric.api_tokens(id,digest,user_id,organization_id,name,scopes,created_at,expires_at,last_used_at,revoked_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                .bind(token.id.get() as i64).bind(token.digest.expose().as_slice()).bind(token.user_id.get() as i64).bind(token.organization_id.get() as i64).bind(token.name.as_str()).bind(scopes)
                .bind(token.created_at.unix_millis()).bind(token.expires_at.unix_millis()).bind(token.last_used_at.map(Timestamp::unix_millis)).bind(token.revoked_at.map(Timestamp::unix_millis))
                .execute(&self.database.pool).await.map_err(error)?;
            Ok(())
        })
    }
    fn load_api_token(
        &self,
        digest: SecretDigest,
    ) -> PortFuture<'_, Result<ApiToken, AuthStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM metric.api_tokens WHERE digest=$1")
                .bind(digest.expose().as_slice())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(error)?
                .ok_or(AuthStoreError::NotFound)?;
            decode_api_token(&row)
        })
    }
    fn touch_api_token(
        &self,
        id: CredentialId,
        at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            matched(
                sqlx::query("UPDATE metric.api_tokens SET last_used_at=$1 WHERE id=$2")
                    .bind(at.unix_millis())
                    .bind(id.get() as i64)
                    .execute(&self.database.pool)
                    .await
                    .map_err(error)?
                    .rows_affected(),
            )
        })
    }
    fn revoke_api_token(
        &self,
        id: CredentialId,
        user: UserId,
        organization: OrganizationId,
        at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            matched(sqlx::query("UPDATE metric.api_tokens SET revoked_at=$1 WHERE id=$2 AND user_id=$3 AND organization_id=$4 AND revoked_at IS NULL")
            .bind(at.unix_millis()).bind(id.get() as i64).bind(user.get() as i64).bind(organization.get() as i64).execute(&self.database.pool).await.map_err(error)?.rows_affected())
        })
    }
    fn project_access(
        &self,
        id: ProjectId,
    ) -> PortFuture<'_, Result<(OrganizationId, ProjectAcceptanceState), AuthStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT organization_id,state FROM metric.projects WHERE id=$1")
                .bind(id.get())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(error)?
                .ok_or(AuthStoreError::NotFound)?;
            Ok((
                organization_id(get(&row, "organization_id")?)?,
                match get::<i16>(&row, "state")? {
                    0 => ProjectAcceptanceState::Active,
                    1 => ProjectAcceptanceState::Disabled,
                    2 => ProjectAcceptanceState::PendingDelete,
                    3 => ProjectAcceptanceState::Purging,
                    4 => ProjectAcceptanceState::Deleted,
                    _ => return Err(AuthStoreError::InvalidData),
                },
            ))
        })
    }
    fn append_audit(&self, record: AuditRecord) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let metadata = Value::Array(
                record
                    .metadata
                    .values()
                    .iter()
                    .map(|(key, value)| json!([key.name(), value.as_str()]))
                    .collect(),
            );
            sqlx::query("INSERT INTO metric.audit_log(request_id,organization_id,actor,actor_user_id,action,target_kind,target_id,timestamp,metadata) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(request_id) DO NOTHING")
                .bind(record.request_id.as_str()).bind(record.organization_id.get() as i64).bind(actor_code(record.actor)).bind(record.actor_user_id.get() as i64)
                .bind(record.action.name()).bind(record.target_kind).bind(record.target_id.as_str()).bind(record.timestamp.unix_millis()).bind(metadata)
                .execute(&self.database.pool).await.map_err(error)?;
            Ok(())
        })
    }
    fn list_api_tokens(
        &self,
        user: UserId,
        organization: OrganizationId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ApiTokenView>, AuthStoreError>> {
        Box::pin(async move {
            let rows=sqlx::query("SELECT * FROM metric.api_tokens WHERE user_id=$1 AND organization_id=$2 AND revoked_at IS NULL ORDER BY created_at DESC,id DESC LIMIT $3")
                .bind(user.get() as i64).bind(organization.get() as i64).bind(bound(limit)?).fetch_all(&self.database.pool).await.map_err(error)?;
            rows.iter()
                .map(|row| {
                    let token = decode_api_token(row)?;
                    Ok(ApiTokenView {
                        id: token.id,
                        name: token.name.as_str().into(),
                        scopes: token
                            .scopes
                            .iter()
                            .map(|permission| permission.scope().into())
                            .collect(),
                        created_at: token.created_at,
                        expires_at: token.expires_at,
                        last_used_at: token.last_used_at,
                    })
                })
                .collect()
        })
    }
    fn load_organization(
        &self,
        id: OrganizationId,
    ) -> PortFuture<'_, Result<OrganizationIdentity, AuthStoreError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM metric.organizations WHERE id=$1")
                .bind(id.get() as i64)
                .fetch_optional(&self.database.pool)
                .await
                .map_err(error)?
                .ok_or(AuthStoreError::NotFound)?;
            Ok(OrganizationIdentity {
                id,
                slug: Slug::new(get::<String>(&row, "slug")?)
                    .map_err(|_| AuthStoreError::InvalidData)?,
                display_name: DisplayName::new(get::<String>(&row, "display_name")?)
                    .map_err(|_| AuthStoreError::InvalidData)?,
                created_at: timestamp(&row, "created_at")?,
            })
        })
    }
    fn list_organization_members(
        &self,
        organization: OrganizationId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<OrganizationMemberView>, AuthStoreError>> {
        Box::pin(async move {
            let rows=sqlx::query("SELECT m.*,u.email,u.display_name,u.disabled_at FROM metric.organization_memberships m JOIN metric.users u ON u.id=m.user_id WHERE m.organization_id=$1 ORDER BY m.created_at,m.user_id LIMIT $2")
                .bind(organization.get() as i64).bind(bound(limit)?).fetch_all(&self.database.pool).await.map_err(error)?;
            rows.iter()
                .map(|row| {
                    Ok(OrganizationMemberView {
                        user_id: user_id(get(row, "user_id")?)?,
                        email: get::<String>(row, "email")?.into(),
                        display_name: get::<String>(row, "display_name")?.into(),
                        role: decode_role(get(row, "role")?)?,
                        disabled_at: optional_timestamp(row, "disabled_at")?,
                        joined_at: timestamp(row, "created_at")?,
                    })
                })
                .collect()
        })
    }
    fn list_audit_log(
        &self,
        organization: OrganizationId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<AuditLogView>, AuthStoreError>> {
        Box::pin(async move {
            let rows=sqlx::query("SELECT * FROM metric.audit_log WHERE organization_id=$1 ORDER BY timestamp DESC,request_id DESC LIMIT $2")
                .bind(organization.get() as i64).bind(bound(limit)?).fetch_all(&self.database.pool).await.map_err(error)?;
            rows.iter()
                .map(|row| {
                    let value: Value = get(row, "metadata")?;
                    let values = value
                        .as_array()
                        .filter(|values| values.len() <= MAX_AUDIT_METADATA)
                        .ok_or(AuthStoreError::InvalidData)?;
                    let metadata = values
                        .iter()
                        .map(|value| {
                            let pair = value
                                .as_array()
                                .filter(|pair| pair.len() == 2)
                                .ok_or(AuthStoreError::InvalidData)?;
                            let key = match pair[0].as_str() {
                                Some("role") => AuditMetadataKey::Role,
                                Some("credential_kind") => AuditMetadataKey::CredentialKind,
                                Some("outcome") => AuditMetadataKey::Outcome,
                                Some("project_id") => AuditMetadataKey::ProjectId,
                                Some("selected_event_count") => {
                                    AuditMetadataKey::SelectedEventCount
                                }
                                Some("result_size_class") => AuditMetadataKey::ResultSizeClass,
                                _ => return Err(AuthStoreError::InvalidData),
                            };
                            let value = AuditMetadataValue::new(
                                pair[1].as_str().ok_or(AuthStoreError::InvalidData)?,
                            )
                            .map_err(|_| AuthStoreError::InvalidData)?;
                            Ok((key, value))
                        })
                        .collect::<Result<Vec<_>, AuthStoreError>>()?;
                    let metadata =
                        AuditMetadata::new(metadata).map_err(|_| AuthStoreError::InvalidData)?;
                    let metadata = metadata
                        .values()
                        .iter()
                        .map(|(key, value)| (key.name().into(), value.as_str().into()))
                        .collect();
                    Ok(AuditLogView {
                        request_id: get::<String>(row, "request_id")?.into(),
                        actor: match get::<i16>(row, "actor")? {
                            0 => "web_session",
                            1 => "personal_api_token",
                            2 => "bootstrap",
                            _ => return Err(AuthStoreError::InvalidData),
                        }
                        .into(),
                        actor_user_id: user_id(get(row, "actor_user_id")?)?,
                        action: get::<String>(row, "action")?.into(),
                        target_kind: get::<String>(row, "target_kind")?.into(),
                        target_id: get::<String>(row, "target_id")?.into(),
                        timestamp: timestamp(row, "timestamp")?,
                        metadata,
                    })
                })
                .collect()
        })
    }
}

async fn insert_organization(
    tx: &mut Transaction<'_, Postgres>,
    organization: &OrganizationIdentity,
) -> Result<(), AuthStoreError> {
    sqlx::query(
        "INSERT INTO metric.organizations(id,slug,display_name,created_at) VALUES($1,$2,$3,$4)",
    )
    .bind(organization.id.get() as i64)
    .bind(organization.slug.as_str())
    .bind(organization.display_name.as_str())
    .bind(organization.created_at.unix_millis())
    .execute(&mut **tx)
    .await
    .map_err(error)?;
    Ok(())
}
async fn insert_user(
    tx: &mut Transaction<'_, Postgres>,
    user: &UserAccount,
) -> Result<(), AuthStoreError> {
    sqlx::query("INSERT INTO metric.users(id,email,canonical_email,display_name,password_hash,disabled_at,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(user.id.get() as i64).bind(user.email.display()).bind(user.email.canonical()).bind(user.display_name.as_str())
        .bind(user.password_hash.as_ref().map(PasswordHash::expose)).bind(user.disabled_at.map(Timestamp::unix_millis)).bind(user.created_at.unix_millis())
        .execute(&mut **tx).await.map_err(error)?;
    Ok(())
}
async fn insert_membership(
    tx: &mut Transaction<'_, Postgres>,
    member: &OrganizationMembership,
) -> Result<(), AuthStoreError> {
    sqlx::query("INSERT INTO metric.organization_memberships(organization_id,user_id,role,created_at,created_by) VALUES($1,$2,$3,$4,$5)")
        .bind(member.organization_id.get() as i64).bind(member.user_id.get() as i64).bind(role_code(member.role)).bind(member.created_at.unix_millis()).bind(member.created_by.get() as i64)
        .execute(&mut **tx).await.map_err(error)?;
    Ok(())
}
async fn insert_setup(
    tx: &mut Transaction<'_, Postgres>,
    token: &SetupToken,
) -> Result<(), AuthStoreError> {
    sqlx::query("INSERT INTO metric.setup_tokens(id,digest,purpose,user_id,created_at,expires_at,consumed_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(token.id.get() as i64).bind(token.digest.expose().as_slice()).bind(purpose_code(token.purpose)).bind(token.user_id.map(|user|user.get() as i64))
        .bind(token.created_at.unix_millis()).bind(token.expires_at.unix_millis()).bind(token.consumed_at.map(Timestamp::unix_millis))
        .execute(&mut **tx).await.map_err(error)?;
    Ok(())
}
async fn ensure_active_owner_survives(
    tx: &mut Transaction<'_, Postgres>,
    organization: OrganizationId,
    excluded_user: UserId,
) -> Result<(), AuthStoreError> {
    let active: bool =
        sqlx::query_scalar("SELECT disabled_at IS NULL FROM metric.users WHERE id=$1")
            .bind(excluded_user.get() as i64)
            .fetch_optional(&mut **tx)
            .await
            .map_err(error)?
            .ok_or(AuthStoreError::NotFound)?;
    if !active {
        return Ok(());
    }
    let survives: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM metric.organization_memberships m JOIN metric.users u ON u.id=m.user_id WHERE m.organization_id=$1 AND m.role=0 AND m.user_id<>$2 AND u.disabled_at IS NULL)",
    )
    .bind(organization.get() as i64)
    .bind(excluded_user.get() as i64)
    .fetch_one(&mut **tx)
    .await
    .map_err(error)?;
    if !survives {
        Err(AuthStoreError::FinalOwner)
    } else {
        Ok(())
    }
}
fn error(value: sqlx::Error) -> AuthStoreError {
    if let Some(db) = value.as_database_error() {
        match db.code().as_deref() {
            Some("23505") => {
                return match db.constraint() {
                    Some(
                        "user_canonical_email_unique"
                        | "organization_slug_unique"
                        | "organization_memberships_pkey",
                    ) => AuthStoreError::AlreadyExists,
                    _ => AuthStoreError::IdentityCollision,
                };
            }
            Some("23503") => return AuthStoreError::NotFound,
            Some("23514") => return AuthStoreError::InvalidData,
            _ => {}
        }
    }
    AuthStoreError::Unavailable
}
fn matched(count: u64) -> Result<(), AuthStoreError> {
    if count == 1 {
        Ok(())
    } else {
        Err(AuthStoreError::NotFound)
    }
}
fn bound(limit: usize) -> Result<i64, AuthStoreError> {
    if (1..=100).contains(&limit) {
        Ok(limit as i64)
    } else {
        Err(AuthStoreError::InvalidData)
    }
}
fn get<T>(row: &PgRow, name: &str) -> Result<T, AuthStoreError>
where
    T: for<'a> sqlx::Decode<'a, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get(name).map_err(|_| AuthStoreError::InvalidData)
}
fn user_id(value: i64) -> Result<UserId, AuthStoreError> {
    UserId::new(value as u64).map_err(|_| AuthStoreError::InvalidData)
}
fn credential_id(value: i64) -> Result<CredentialId, AuthStoreError> {
    CredentialId::new(value as u64).map_err(|_| AuthStoreError::InvalidData)
}
fn organization_id(value: i64) -> Result<OrganizationId, AuthStoreError> {
    OrganizationId::new(value as u64).map_err(|_| AuthStoreError::InvalidData)
}
fn timestamp(row: &PgRow, name: &str) -> Result<Timestamp, AuthStoreError> {
    Timestamp::from_unix_millis(get(row, name)?).map_err(|_| AuthStoreError::InvalidData)
}
fn optional_timestamp(row: &PgRow, name: &str) -> Result<Option<Timestamp>, AuthStoreError> {
    get::<Option<i64>>(row, name)?
        .map(|value| Timestamp::from_unix_millis(value).map_err(|_| AuthStoreError::InvalidData))
        .transpose()
}
fn digest(row: &PgRow, name: &str) -> Result<SecretDigest, AuthStoreError> {
    Ok(SecretDigest::new(
        get::<Vec<u8>>(row, name)?
            .try_into()
            .map_err(|_| AuthStoreError::InvalidData)?,
    ))
}
fn role_code(role: OrganizationRole) -> i16 {
    match role {
        OrganizationRole::Owner => 0,
        OrganizationRole::Admin => 1,
        OrganizationRole::Member => 2,
        OrganizationRole::Viewer => 3,
    }
}
fn decode_role(role: i16) -> Result<OrganizationRole, AuthStoreError> {
    match role {
        0 => Ok(OrganizationRole::Owner),
        1 => Ok(OrganizationRole::Admin),
        2 => Ok(OrganizationRole::Member),
        3 => Ok(OrganizationRole::Viewer),
        _ => Err(AuthStoreError::InvalidData),
    }
}
fn purpose_code(purpose: SetupPurpose) -> i16 {
    match purpose {
        SetupPurpose::Bootstrap => 0,
        SetupPurpose::PasswordSetup => 1,
        SetupPurpose::PasswordReset => 2,
    }
}
fn actor_code(actor: Actor) -> i16 {
    match actor {
        Actor::WebSession => 0,
        Actor::PersonalApiToken => 1,
        Actor::Bootstrap => 2,
    }
}
fn decode_user(row: &PgRow) -> Result<UserAccount, AuthStoreError> {
    let email = EmailAddress::parse(get::<String>(row, "email")?)
        .map_err(|_| AuthStoreError::InvalidData)?;
    if get::<String>(row, "canonical_email")? != email.canonical() {
        return Err(AuthStoreError::InvalidData);
    }
    Ok(UserAccount {
        id: user_id(get(row, "id")?)?,
        email,
        display_name: UserDisplayName::new(get::<String>(row, "display_name")?)
            .map_err(|_| AuthStoreError::InvalidData)?,
        password_hash: get::<Option<String>>(row, "password_hash")?
            .map(PasswordHash::new)
            .transpose()
            .map_err(|_| AuthStoreError::InvalidData)?,
        disabled_at: optional_timestamp(row, "disabled_at")?,
        created_at: timestamp(row, "created_at")?,
    })
}
fn decode_membership(row: &PgRow) -> Result<OrganizationMembership, AuthStoreError> {
    Ok(OrganizationMembership {
        organization_id: organization_id(get(row, "organization_id")?)?,
        user_id: user_id(get(row, "user_id")?)?,
        role: decode_role(get(row, "role")?)?,
        created_at: timestamp(row, "created_at")?,
        created_by: user_id(get(row, "created_by")?)?,
    })
}
fn decode_session(row: &PgRow) -> Result<WebSession, AuthStoreError> {
    Ok(WebSession {
        id: credential_id(get(row, "id")?)?,
        digest: digest(row, "digest")?,
        csrf_digest: digest(row, "csrf_digest")?,
        user_id: user_id(get(row, "user_id")?)?,
        created_at: timestamp(row, "created_at")?,
        last_seen_at: timestamp(row, "last_seen_at")?,
        idle_expires_at: timestamp(row, "idle_expires_at")?,
        absolute_expires_at: timestamp(row, "absolute_expires_at")?,
        revoked_at: optional_timestamp(row, "revoked_at")?,
    })
}
fn decode_api_token(row: &PgRow) -> Result<ApiToken, AuthStoreError> {
    let bits: i32 = get(row, "scopes")?;
    if bits < 0 || bits >= (1 << Permission::ALL.len()) {
        return Err(AuthStoreError::InvalidData);
    }
    Ok(ApiToken {
        id: credential_id(get(row, "id")?)?,
        digest: digest(row, "digest")?,
        user_id: user_id(get(row, "user_id")?)?,
        organization_id: organization_id(get(row, "organization_id")?)?,
        name: TokenName::new(get::<String>(row, "name")?)
            .map_err(|_| AuthStoreError::InvalidData)?,
        scopes: PermissionSet::from_permissions(
            Permission::ALL
                .into_iter()
                .filter(|permission| bits & (1 << *permission as u8) != 0),
        ),
        created_at: timestamp(row, "created_at")?,
        expires_at: timestamp(row, "expires_at")?,
        last_used_at: optional_timestamp(row, "last_used_at")?,
        revoked_at: optional_timestamp(row, "revoked_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MigrationPolicy, PostgresSchemaManager, test_support::Fixture};

    fn now() -> Timestamp {
        Timestamp::from_unix_millis(1_700_000_000_000).unwrap()
    }
    fn later() -> Timestamp {
        Timestamp::from_unix_millis(now().unix_millis() + 60_000).unwrap()
    }
    fn user(id: u64) -> UserAccount {
        UserAccount {
            id: UserId::new(id).unwrap(),
            email: EmailAddress::parse(format!("User{id}@example.COM")).unwrap(),
            display_name: UserDisplayName::new("' OR 1=1; -- Юникод").unwrap(),
            password_hash: Some(
                PasswordHash::new("$argon2id$synthetic-hash-not-a-password").unwrap(),
            ),
            disabled_at: None,
            created_at: now(),
        }
    }
    fn member(user: u64, organization: u64, role: OrganizationRole) -> OrganizationMembership {
        OrganizationMembership {
            organization_id: OrganizationId::new(organization).unwrap(),
            user_id: UserId::new(user).unwrap(),
            role,
            created_at: now(),
            created_by: UserId::new(1).unwrap(),
        }
    }
    fn token(id: u64, purpose: SetupPurpose, user: Option<u64>) -> SetupToken {
        SetupToken {
            id: CredentialId::new(id).unwrap(),
            digest: SecretDigest::new([id as u8; 32]),
            purpose,
            user_id: user.map(|id| UserId::new(id).unwrap()),
            created_at: now(),
            expires_at: later(),
            consumed_at: None,
        }
    }
    fn bootstrap() -> BootstrapIdentity {
        BootstrapIdentity {
            operation_id: CredentialId::new(100).unwrap(),
            token_digest: token(10, SetupPurpose::Bootstrap, None).digest,
            organization_id: OrganizationId::new(1).unwrap(),
            organization_slug: Slug::new("org-one").unwrap(),
            organization_name: DisplayName::new("Organization one").unwrap(),
            user: user(1),
            membership: member(1, 1, OrganizationRole::Owner),
            timestamp: now(),
        }
    }
    async fn setup() -> (Fixture, PostgresDatabase, PostgresAuthStore) {
        let fixture = Fixture::new().await;
        PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
            .await
            .unwrap();
        let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        let store = PostgresAuthStore::new(database.clone());
        store
            .install_bootstrap_token(token(10, SetupPurpose::Bootstrap, None))
            .await
            .unwrap();
        store.consume_bootstrap(bootstrap()).await.unwrap();
        (fixture, database, store)
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn disabled_owners_do_not_satisfy_final_owner_guard() {
        let fixture = Fixture::new().await;
        PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
            .await
            .unwrap();
        let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        let store = PostgresAuthStore::new(database.clone());
        let result = metric_testkit::auth_conformance::active_owner_guard(&store, now()).await;
        database.close().await;
        fixture.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn bootstrap_concurrent_install_consume_and_retry_are_atomic() {
        let fixture = Fixture::new().await;
        PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
            .await
            .unwrap();
        let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        let store = PostgresAuthStore::new(database.clone());
        let (a, b) = tokio::join!(
            store.install_bootstrap_token(token(10, SetupPurpose::Bootstrap, None)),
            store.install_bootstrap_token(token(11, SetupPurpose::Bootstrap, None))
        );
        let installed = if a == Ok(BootstrapTokenInstall::Created) {
            10
        } else {
            11
        };
        assert!(matches!(
            (a, b),
            (
                Ok(BootstrapTokenInstall::Created),
                Ok(BootstrapTokenInstall::AlreadyInstalled)
            ) | (
                Ok(BootstrapTokenInstall::AlreadyInstalled),
                Ok(BootstrapTokenInstall::Created)
            )
        ));
        let identity = BootstrapIdentity {
            token_digest: token(installed, SetupPurpose::Bootstrap, None).digest,
            ..bootstrap()
        };
        let other = BootstrapIdentity {
            operation_id: CredentialId::new(101).unwrap(),
            ..identity.clone()
        };
        let (a, b) = tokio::join!(
            store.consume_bootstrap(identity.clone()),
            store.consume_bootstrap(other.clone())
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(
            a == Err(AuthStoreError::InvalidCredential)
                || b == Err(AuthStoreError::InvalidCredential)
        );
        store
            .consume_bootstrap(if a.is_ok() { identity } else { other })
            .await
            .unwrap();
        assert_eq!(
            store
                .install_bootstrap_token(token(12, SetupPurpose::Bootstrap, None))
                .await,
            Ok(BootstrapTokenInstall::Closed)
        );
        assert_eq!(
            store
                .list_user_memberships(UserId::new(1).unwrap(), 100)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .list_organization_members(OrganizationId::new(1).unwrap(), 100)
                .await
                .unwrap()
                .len(),
            1
        );
        database.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn bootstrap_collision_rolls_back_identity_and_token_consumption() {
        let fixture = Fixture::new().await;
        PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
            .await
            .unwrap();
        let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        let store = PostgresAuthStore::new(database.clone());
        let project_store =
            crate::PostgresProjectStore::new(database.clone(), SecretBytes::new([1; 32]), 32)
                .unwrap();
        metric_ports::ProjectStore::insert_organization(
            &project_store,
            OrganizationIdentity {
                id: OrganizationId::new(99).unwrap(),
                slug: bootstrap().organization_slug,
                display_name: DisplayName::new("Existing").unwrap(),
                created_at: now(),
            },
        )
        .await
        .unwrap();
        store
            .install_bootstrap_token(token(10, SetupPurpose::Bootstrap, None))
            .await
            .unwrap();
        assert_eq!(
            store.consume_bootstrap(bootstrap()).await,
            Err(AuthStoreError::AlreadyExists)
        );
        assert_eq!(
            store.load_user(UserId::new(1).unwrap()).await,
            Err(AuthStoreError::NotFound)
        );
        assert_eq!(
            store
                .load_organization(OrganizationId::new(1).unwrap())
                .await,
            Err(AuthStoreError::NotFound)
        );
        let fixed = BootstrapIdentity {
            organization_slug: Slug::new("new-org").unwrap(),
            ..bootstrap()
        };
        store.consume_bootstrap(fixed).await.unwrap();
        database.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn invitation_reissue_password_consumption_and_canonical_email() {
        let (fixture, database, store) = setup().await;
        let mut invited = user(2);
        invited.password_hash = None;
        let membership = member(2, 1, OrganizationRole::Member);
        let first = token(20, SetupPurpose::PasswordSetup, Some(2));
        store
            .create_invited_user(invited.clone(), membership.clone(), first.clone())
            .await
            .unwrap();
        assert_eq!(
            store
                .load_user_by_email(&EmailAddress::parse("user2@EXAMPLE.com").unwrap())
                .await
                .unwrap(),
            invited
        );
        assert_eq!(
            store
                .load_membership(invited.id, OrganizationId::new(2).unwrap())
                .await,
            Err(AuthStoreError::NotFound)
        );
        let replacement = token(21, SetupPurpose::PasswordSetup, Some(2));
        store
            .create_password_setup_token(replacement.clone())
            .await
            .unwrap();
        let hash = PasswordHash::new("$argon2id$replacement-hash").unwrap();
        assert_eq!(
            store
                .consume_password_setup(first.digest, now(), hash.clone())
                .await,
            Err(AuthStoreError::InvalidCredential)
        );
        assert_eq!(
            store
                .consume_password_setup(replacement.digest, now(), hash.clone())
                .await
                .unwrap(),
            invited.id
        );
        assert_eq!(
            store
                .consume_password_setup(replacement.digest, now(), hash.clone())
                .await,
            Err(AuthStoreError::InvalidCredential)
        );
        assert_eq!(
            store.load_user(invited.id).await.unwrap().password_hash,
            Some(hash)
        );
        let reset = token(22, SetupPurpose::PasswordReset, Some(2));
        store
            .create_password_setup_token(reset.clone())
            .await
            .unwrap();
        assert_eq!(
            store
                .consume_password_setup(
                    reset.digest,
                    later(),
                    PasswordHash::new("expired").unwrap()
                )
                .await,
            Err(AuthStoreError::InvalidCredential)
        );
        store
            .consume_password_setup(reset.digest, now(), PasswordHash::new("reset").unwrap())
            .await
            .unwrap();
        store
            .update_password_hash(invited.id, PasswordHash::new("changed").unwrap(), now())
            .await
            .unwrap();
        assert_eq!(
            store
                .load_user(invited.id)
                .await
                .unwrap()
                .password_hash
                .unwrap()
                .expose(),
            "changed"
        );
        let missing_member = member(3, 99, OrganizationRole::Member);
        assert_eq!(
            store
                .create_invited_user(
                    user(3),
                    missing_member,
                    token(23, SetupPurpose::PasswordSetup, Some(3))
                )
                .await,
            Err(AuthStoreError::NotFound)
        );
        assert_eq!(
            store.load_user(UserId::new(3).unwrap()).await,
            Err(AuthStoreError::NotFound)
        );
        let duplicate = UserAccount {
            id: UserId::new(4).unwrap(),
            ..invited
        };
        assert_eq!(
            store
                .create_invited_user(
                    duplicate,
                    member(4, 1, OrganizationRole::Member),
                    token(24, SetupPurpose::PasswordSetup, Some(4))
                )
                .await,
            Err(AuthStoreError::AlreadyExists)
        );
        database.close().await;
        fixture.cleanup().await;
    }

    fn mutation(user: u64, organization: u64, kind: MembershipMutationKind) -> MembershipMutation {
        MembershipMutation {
            organization_id: OrganizationId::new(organization).unwrap(),
            user_id: UserId::new(user).unwrap(),
            actor_user_id: UserId::new(1).unwrap(),
            operation_id: CredentialId::new(500 + user).unwrap(),
            kind,
            timestamp: now(),
        }
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn membership_final_owner_and_user_disabling_are_serialized() {
        let (fixture, database, store) = setup().await;
        let owner = UserId::new(1).unwrap();
        let org = OrganizationId::new(1).unwrap();
        assert_eq!(
            store
                .mutate_membership(mutation(1, 1, MembershipMutationKind::Remove))
                .await,
            Err(AuthStoreError::FinalOwner)
        );
        assert_eq!(
            store
                .set_user_disabled(owner, Some(now()), CredentialId::new(700).unwrap())
                .await,
            Err(AuthStoreError::FinalOwner)
        );
        store
            .create_invited_user(
                user(2),
                member(2, 1, OrganizationRole::Owner),
                token(20, SetupPurpose::PasswordSetup, Some(2)),
            )
            .await
            .unwrap();
        let (a, b) = tokio::join!(
            store.mutate_membership(mutation(
                1,
                1,
                MembershipMutationKind::ChangeRole(OrganizationRole::Member)
            )),
            store.mutate_membership(mutation(
                2,
                1,
                MembershipMutationKind::ChangeRole(OrganizationRole::Member)
            ))
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(a == Err(AuthStoreError::FinalOwner) || b == Err(AuthStoreError::FinalOwner));
        let demoted = if a.is_ok() {
            owner
        } else {
            UserId::new(2).unwrap()
        };
        store
            .set_user_disabled(demoted, Some(now()), CredentialId::new(701).unwrap())
            .await
            .unwrap();
        assert_eq!(
            store.load_user(demoted).await.unwrap().disabled_at,
            Some(now())
        );
        store
            .set_user_disabled(demoted, None, CredentialId::new(702).unwrap())
            .await
            .unwrap();
        assert_eq!(store.load_user(demoted).await.unwrap().disabled_at, None);
        store
            .mutate_membership(mutation(demoted.get(), 1, MembershipMutationKind::Remove))
            .await
            .unwrap();
        assert_eq!(
            store.load_membership(demoted, org).await,
            Err(AuthStoreError::NotFound)
        );
        store
            .mutate_membership(mutation(
                demoted.get(),
                1,
                MembershipMutationKind::Create(OrganizationRole::Viewer),
            ))
            .await
            .unwrap();
        assert_eq!(
            store
                .mutate_membership(mutation(
                    demoted.get(),
                    1,
                    MembershipMutationKind::Create(OrganizationRole::Viewer)
                ))
                .await,
            Err(AuthStoreError::AlreadyExists)
        );
        let organization = OrganizationIdentity {
            id: OrganizationId::new(2).unwrap(),
            slug: Slug::new("org-two").unwrap(),
            display_name: DisplayName::new("Two").unwrap(),
            created_at: now(),
        };
        store
            .create_owned_organization(organization.clone(), member(1, 2, OrganizationRole::Owner))
            .await
            .unwrap();
        assert_eq!(
            store.load_organization(organization.id).await.unwrap(),
            organization
        );
        assert_eq!(
            store.list_user_memberships(owner, 100).await.unwrap().len(),
            2
        );
        assert_eq!(
            store.list_organization_members(org, 0).await,
            Err(AuthStoreError::InvalidData)
        );
        database.close().await;
        fixture.cleanup().await;
    }

    fn session(id: u64, user: u64) -> WebSession {
        WebSession {
            id: CredentialId::new(id).unwrap(),
            digest: SecretDigest::new([id as u8; 32]),
            csrf_digest: SecretDigest::new([id as u8 + 1; 32]),
            user_id: UserId::new(user).unwrap(),
            created_at: now(),
            last_seen_at: now(),
            idle_expires_at: later(),
            absolute_expires_at: later(),
            revoked_at: None,
        }
    }
    fn api_token(id: u64, organization: u64) -> ApiToken {
        ApiToken {
            id: CredentialId::new(id).unwrap(),
            digest: SecretDigest::new([id as u8; 32]),
            user_id: UserId::new(1).unwrap(),
            organization_id: OrganizationId::new(organization).unwrap(),
            name: TokenName::new("'; OR 1=1 --").unwrap(),
            scopes: PermissionSet::from_permissions([
                Permission::IssueRead,
                Permission::ProjectRead,
            ]),
            created_at: now(),
            expires_at: later(),
            last_used_at: None,
            revoked_at: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn sessions_and_api_tokens_preserve_digests_revocation_and_scope() {
        let (fixture, database, store) = setup().await;
        let first = session(30, 1);
        let second = session(31, 1);
        store.create_session(first.clone()).await.unwrap();
        store.create_session(second.clone()).await.unwrap();
        assert_eq!(store.load_session(first.digest).await.unwrap(), first);
        store
            .touch_session(first.id, later(), later())
            .await
            .unwrap();
        assert_eq!(
            store.load_session(first.digest).await.unwrap().last_seen_at,
            later()
        );
        store.revoke_session(first.digest, now()).await.unwrap();
        assert_eq!(
            store.revoke_session(first.digest, now()).await,
            Err(AuthStoreError::NotFound)
        );
        store
            .revoke_user_sessions(first.user_id, now())
            .await
            .unwrap();
        assert_eq!(
            store.load_session(second.digest).await.unwrap().revoked_at,
            Some(now())
        );
        let token = api_token(40, 1);
        store.create_api_token(token.clone()).await.unwrap();
        assert_eq!(store.load_api_token(token.digest).await.unwrap(), token);
        assert_eq!(
            store.create_api_token(token.clone()).await,
            Err(AuthStoreError::IdentityCollision)
        );
        store.touch_api_token(token.id, now()).await.unwrap();
        assert_eq!(
            store
                .load_api_token(token.digest)
                .await
                .unwrap()
                .last_used_at,
            Some(now())
        );
        assert_eq!(
            store
                .list_api_tokens(token.user_id, OrganizationId::new(2).unwrap(), 100)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store
                .list_api_tokens(UserId::new(2).unwrap(), token.organization_id, 100)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store
                .revoke_api_token(
                    token.id,
                    UserId::new(2).unwrap(),
                    token.organization_id,
                    now()
                )
                .await,
            Err(AuthStoreError::NotFound)
        );
        assert_eq!(
            store
                .revoke_api_token(
                    token.id,
                    token.user_id,
                    OrganizationId::new(2).unwrap(),
                    now()
                )
                .await,
            Err(AuthStoreError::NotFound)
        );
        assert_eq!(
            store
                .list_api_tokens(token.user_id, token.organization_id, 100)
                .await
                .unwrap()
                .len(),
            1
        );
        store
            .revoke_api_token(token.id, token.user_id, token.organization_id, now())
            .await
            .unwrap();
        assert_eq!(
            store
                .list_api_tokens(token.user_id, token.organization_id, 100)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store.load_api_token(token.digest).await.unwrap().revoked_at,
            Some(now())
        );
        database.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn audit_is_bounded_idempotent_and_organization_scoped() {
        let (fixture, database, store) = setup().await;
        let org = OrganizationId::new(1).unwrap();
        let audit = AuditRecord {
            request_id: RequestCorrelationId::new("'; SELECT * FROM metric.users; --").unwrap(),
            organization_id: org,
            actor: Actor::WebSession,
            actor_user_id: UserId::new(1).unwrap(),
            action: AuditAction::ProjectCreated,
            target_kind: "project",
            target_id: AuditTargetId::new("1").unwrap(),
            timestamp: now(),
            metadata: AuditMetadata::new([(
                AuditMetadataKey::Outcome,
                AuditMetadataValue::new("'; OR true; --").unwrap(),
            )])
            .unwrap(),
        };
        store.append_audit(audit.clone()).await.unwrap();
        store.append_audit(audit.clone()).await.unwrap();
        let rows = store.list_audit_log(org, 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].metadata[0].1.as_ref(),
            audit.metadata.values()[0].1.as_str()
        );
        assert_eq!(rows[0].request_id.as_ref(), audit.request_id.as_str());
        assert!(
            store
                .list_audit_log(OrganizationId::new(2).unwrap(), 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.list_audit_log(org, 101).await,
            Err(AuthStoreError::InvalidData)
        );
        assert_eq!(
            store.project_access(ProjectId::new(1).unwrap()).await,
            Err(AuthStoreError::NotFound)
        );
        // An operator-corrupted row must not bypass the bounded domain decoder.
        for metadata in [
            json!([["unexpected_key", "value"]]),
            json!([["outcome", "ok"], ["outcome", "duplicate"]]),
            json!([["outcome", "x".repeat(129)]]),
        ] {
            sqlx::query("UPDATE metric.audit_log SET metadata=$1 WHERE organization_id=$2")
                .bind(metadata)
                .bind(org.get() as i64)
                .execute(&database.pool)
                .await
                .unwrap();
            assert_eq!(
                store.list_audit_log(org, 100).await,
                Err(AuthStoreError::InvalidData)
            );
        }
        database.close().await;
        fixture.cleanup().await;
    }
}
