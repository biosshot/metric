//! MongoDB identity, credential, authorization, and audit adapter.

use std::{
    collections::{BTreeSet, HashMap},
    time::Duration,
};

use futures_util::TryStreamExt;
use metric_domain::{
    DisplayName, OrganizationId, OrganizationIdentity, ProjectAcceptanceState, ProjectId, Slug,
    Timestamp,
    api::{ApiTokenView, AuditLogView, OrganizationMemberView},
    auth::{
        Actor, ApiToken, AuditRecord, BootstrapIdentity, CredentialId, EmailAddress,
        MembershipMutation, MembershipMutationKind, OrganizationMembership, OrganizationRole,
        PasswordHash, Permission, PermissionSet, SecretDigest, SetupPurpose, SetupToken, TokenName,
        UserAccount, UserDisplayName, UserId, WebSession,
    },
};
use metric_ports::{AuthStore, AuthStoreError, BootstrapTokenInstall, PortFuture};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    error::{Error as MongoError, ErrorKind, WriteFailure},
    options::{IndexOptions, ReturnDocument},
};

const AUTH_LOCK_DURATION: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct MongoAuthStore {
    database: Database,
}

impl MongoAuthStore {
    #[must_use]
    pub fn from_database(database: Database) -> Self {
        Self { database }
    }

    async fn install_bootstrap_token_inner(
        &self,
        token: SetupToken,
    ) -> Result<BootstrapTokenInstall, AuthStoreError> {
        if self
            .database
            .collection::<Document>("users")
            .count_documents(doc! {})
            .limit(1)
            .await
            .map_err(unavailable)?
            > 0
        {
            return Ok(BootstrapTokenInstall::Closed);
        }
        let collection = self
            .database
            .collection::<Document>("password_setup_tokens");
        if collection
            .find_one(doc! { "purpose": "bootstrap" })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(unavailable)?
            .is_some()
        {
            return Ok(BootstrapTokenInstall::AlreadyInstalled);
        }
        match collection.insert_one(encode_setup_token(&token)?).await {
            Ok(_) => Ok(BootstrapTokenInstall::Created),
            Err(error) if duplicate_write(&error) => {
                if collection
                    .find_one(doc! { "purpose": "bootstrap" })
                    .projection(doc! { "_id": 1 })
                    .await
                    .map_err(unavailable)?
                    .is_some()
                {
                    Ok(BootstrapTokenInstall::AlreadyInstalled)
                } else {
                    Err(AuthStoreError::AlreadyExists)
                }
            }
            Err(_) => Err(AuthStoreError::Unavailable),
        }
    }

    async fn consume_bootstrap_inner(
        &self,
        identity: BootstrapIdentity,
    ) -> Result<(), AuthStoreError> {
        let operation_id = id_i64(identity.operation_id)?;
        let token = self
            .database
            .collection::<Document>("password_setup_tokens")
            .find_one_and_update(
                doc! {
                    "digest": digest_binary(identity.token_digest),
                    "purpose": "bootstrap",
                    "expires_at": { "$gt": date(identity.timestamp) },
                    "$or": [
                        { "consumed_at": { "$exists": false } },
                        { "operation_id": operation_id },
                    ],
                },
                doc! { "$set": {
                    "consumed_at": date(identity.timestamp),
                    "operation_id": operation_id,
                    "operation_state": "applying",
                }},
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::InvalidCredential)?;
        if token.get_i64("operation_id") != Ok(operation_id) {
            return Err(AuthStoreError::InvalidCredential);
        }

        let users = self.database.collection::<Document>("users");
        if users
            .count_documents(doc! { "_id": { "$ne": user_i64(identity.user.id)? } })
            .limit(1)
            .await
            .map_err(unavailable)?
            > 0
        {
            return Err(AuthStoreError::BootstrapClosed);
        }

        self.database
            .collection::<Document>("organizations")
            .update_one(
                doc! { "_id": organization_i64(identity.organization_id)? },
                doc! { "$setOnInsert": {
                    "slug": identity.organization_slug.as_str(),
                    "display_name": identity.organization_name.as_str(),
                    "created_at": date(identity.timestamp),
                }},
            )
            .upsert(true)
            .await
            .map_err(classify_duplicate)?;
        users
            .update_one(
                doc! { "_id": user_i64(identity.user.id)? },
                doc! { "$setOnInsert": encode_user_fields(&identity.user)? },
            )
            .upsert(true)
            .await
            .map_err(classify_duplicate)?;
        self.database
            .collection::<Document>("organization_memberships")
            .update_one(
                doc! { "_id": membership_id(
                    identity.membership.organization_id,
                    identity.membership.user_id,
                )? },
                doc! { "$setOnInsert": encode_membership_fields(&identity.membership)? },
            )
            .upsert(true)
            .await
            .map_err(classify_duplicate)?;
        let stored_organization = self
            .database
            .collection::<Document>("organizations")
            .find_one(doc! { "_id": organization_i64(identity.organization_id)? })
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::InvalidData)?;
        let stored_user = users
            .find_one(doc! { "_id": user_i64(identity.user.id)? })
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::InvalidData)?;
        let stored_membership = self
            .database
            .collection::<Document>("organization_memberships")
            .find_one(doc! { "_id": membership_id(
                identity.membership.organization_id,
                identity.membership.user_id,
            )? })
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::InvalidData)?;
        if stored_organization.get_str("slug") != Ok(identity.organization_slug.as_str())
            || stored_organization.get_str("display_name")
                != Ok(identity.organization_name.as_str())
            || decode_user(&stored_user)? != identity.user
            || decode_membership(&stored_membership)? != identity.membership
        {
            return Err(AuthStoreError::InvalidCredential);
        }
        self.database
            .collection::<Document>("password_setup_tokens")
            .update_one(
                doc! { "_id": token.get_i64("_id").map_err(|_| AuthStoreError::InvalidData)?, "operation_id": operation_id },
                doc! { "$set": { "operation_state": "complete" } },
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn create_owned_organization_inner(
        &self,
        organization: OrganizationIdentity,
        membership: OrganizationMembership,
    ) -> Result<(), AuthStoreError> {
        if membership.organization_id != organization.id
            || membership.role != OrganizationRole::Owner
        {
            return Err(AuthStoreError::InvalidData);
        }
        let organizations = self.database.collection::<Document>("organizations");
        organizations
            .insert_one(doc! {
                "_id": organization_i64(organization.id)?,
                "slug": organization.slug.as_str(),
                "display_name": organization.display_name.as_str(),
                "created_at": date(organization.created_at),
            })
            .await
            .map_err(classify_organization_duplicate)?;
        if let Err(error) = self
            .database
            .collection::<Document>("organization_memberships")
            .insert_one(encode_membership(&membership)?)
            .await
        {
            let _ = organizations
                .delete_one(doc! { "_id": organization_i64(organization.id)? })
                .await;
            return Err(classify_membership_duplicate(error));
        }
        Ok(())
    }

    async fn create_invited_user_inner(
        &self,
        user: UserAccount,
        membership: OrganizationMembership,
        setup_token: SetupToken,
    ) -> Result<(), AuthStoreError> {
        let organization_exists = self
            .database
            .collection::<Document>("organizations")
            .find_one(doc! { "_id": organization_i64(membership.organization_id)? })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(unavailable)?
            .is_some();
        if !organization_exists {
            return Err(AuthStoreError::NotFound);
        }
        let users = self.database.collection::<Document>("users");
        users
            .insert_one(encode_user(&user)?)
            .await
            .map_err(classify_user_duplicate)?;
        let memberships = self
            .database
            .collection::<Document>("organization_memberships");
        if let Err(error) = memberships
            .insert_one(encode_membership(&membership)?)
            .await
        {
            let _ = users.delete_one(doc! { "_id": user_i64(user.id)? }).await;
            return Err(classify_membership_duplicate(error));
        }
        if let Err(error) = self
            .database
            .collection::<Document>("password_setup_tokens")
            .insert_one(encode_setup_token(&setup_token)?)
            .await
        {
            let _ = memberships
                .delete_one(doc! { "_id": membership_id(
                    membership.organization_id,
                    membership.user_id,
                )? })
                .await;
            let _ = users.delete_one(doc! { "_id": user_i64(user.id)? }).await;
            return Err(classify_duplicate(error));
        }
        Ok(())
    }

    async fn consume_password_setup_inner(
        &self,
        digest: SecretDigest,
        now: Timestamp,
        password_hash: PasswordHash,
    ) -> Result<UserId, AuthStoreError> {
        let token = self
            .database
            .collection::<Document>("password_setup_tokens")
            .find_one_and_update(
                doc! {
                    "digest": digest_binary(digest),
                    "purpose": { "$in": ["password_setup", "password_reset"] },
                    "expires_at": { "$gt": date(now) },
                    "consumed_at": { "$exists": false },
                },
                doc! { "$set": { "consumed_at": date(now) } },
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::InvalidCredential)?;
        let user_id = UserId::new(
            u64::try_from(
                token
                    .get_i64("user_id")
                    .map_err(|_| AuthStoreError::InvalidData)?,
            )
            .map_err(|_| AuthStoreError::InvalidData)?,
        )
        .map_err(|_| AuthStoreError::InvalidData)?;
        let result = self
            .database
            .collection::<Document>("users")
            .update_one(
                doc! { "_id": user_i64(user_id)? },
                doc! {
                    "$set": {
                        "password_hash": password_hash.expose(),
                        "password_changed_at": date(now),
                    }
                },
            )
            .await
            .map_err(unavailable)?;
        if result.matched_count != 1 {
            return Err(AuthStoreError::NotFound);
        }
        Ok(user_id)
    }

    async fn load_user_by_email_inner(
        &self,
        email: &EmailAddress,
    ) -> Result<UserAccount, AuthStoreError> {
        let document = self
            .database
            .collection::<Document>("users")
            .find_one(doc! { "canonical_email": email.canonical() })
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::NotFound)?;
        decode_user(&document)
    }

    async fn load_user_inner(&self, user_id: UserId) -> Result<UserAccount, AuthStoreError> {
        let document = self
            .database
            .collection::<Document>("users")
            .find_one(doc! { "_id": user_i64(user_id)? })
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::NotFound)?;
        decode_user(&document)
    }

    async fn update_password_hash_inner(
        &self,
        user_id: UserId,
        password_hash: PasswordHash,
        changed_at: Timestamp,
    ) -> Result<(), AuthStoreError> {
        let result = self
            .database
            .collection::<Document>("users")
            .update_one(
                doc! { "_id": user_i64(user_id)? },
                doc! { "$set": {
                    "password_hash": password_hash.expose(),
                    "password_changed_at": date(changed_at),
                }},
            )
            .await
            .map_err(unavailable)?;
        matched(result.matched_count)
    }

    async fn load_membership_inner(
        &self,
        user_id: UserId,
        organization_id: OrganizationId,
    ) -> Result<OrganizationMembership, AuthStoreError> {
        let document = self
            .database
            .collection::<Document>("organization_memberships")
            .find_one(doc! { "_id": membership_id(organization_id, user_id)? })
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::NotFound)?;
        decode_membership(&document)
    }

    async fn list_user_memberships_inner(
        &self,
        user_id: UserId,
        limit: usize,
    ) -> Result<Vec<OrganizationMembership>, AuthStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(AuthStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("organization_memberships")
            .find(doc! { "user_id": user_i64(user_id)? })
            .sort(doc! { "created_at": 1, "organization_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(100))
            .await
            .map_err(unavailable)?;
        let mut memberships = Vec::with_capacity(limit);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            memberships.push(decode_membership(&document)?);
        }
        Ok(memberships)
    }

    async fn mutate_membership_inner(
        &self,
        mutation: MembershipMutation,
    ) -> Result<(), AuthStoreError> {
        self.acquire_organization_lock(
            mutation.organization_id,
            mutation.operation_id,
            mutation.timestamp,
        )
        .await?;
        let result = self.mutate_membership_locked(&mutation).await;
        self.release_organization_lock(mutation.organization_id, mutation.operation_id)
            .await;
        result
    }

    async fn mutate_membership_locked(
        &self,
        mutation: &MembershipMutation,
    ) -> Result<(), AuthStoreError> {
        let collection = self
            .database
            .collection::<Document>("organization_memberships");
        let key = membership_id(mutation.organization_id, mutation.user_id)?;
        let existing = collection
            .find_one(doc! { "_id": key.clone() })
            .await
            .map_err(unavailable)?;
        let was_owner = existing
            .as_ref()
            .and_then(|document| document.get_str("role").ok())
            == Some("owner");
        let remains_owner = matches!(
            mutation.kind,
            MembershipMutationKind::Create(OrganizationRole::Owner)
                | MembershipMutationKind::ChangeRole(OrganizationRole::Owner)
        );
        if was_owner && !remains_owner {
            let owners = collection
                .count_documents(doc! {
                    "organization_id": organization_i64(mutation.organization_id)?,
                    "role": "owner",
                })
                .await
                .map_err(unavailable)?;
            if owners <= 1 {
                return Err(AuthStoreError::FinalOwner);
            }
        }
        match mutation.kind {
            MembershipMutationKind::Create(role) => collection
                .insert_one(doc! {
                    "_id": key,
                    "organization_id": organization_i64(mutation.organization_id)?,
                    "user_id": user_i64(mutation.user_id)?,
                    "role": role_name(role),
                    "created_at": date(mutation.timestamp),
                    "created_by": user_i64(mutation.actor_user_id)?,
                })
                .await
                .map(|_| ())
                .map_err(classify_membership_duplicate),
            MembershipMutationKind::ChangeRole(role) => {
                let result = collection
                    .update_one(
                        doc! { "_id": key },
                        doc! { "$set": { "role": role_name(role) } },
                    )
                    .await
                    .map_err(unavailable)?;
                matched(result.matched_count)
            }
            MembershipMutationKind::Remove => {
                let result = collection
                    .delete_one(doc! { "_id": key })
                    .await
                    .map_err(unavailable)?;
                matched(result.deleted_count)
            }
        }
    }

    async fn set_user_disabled_inner(
        &self,
        user_id: UserId,
        disabled_at: Option<Timestamp>,
        operation_id: CredentialId,
    ) -> Result<(), AuthStoreError> {
        let now = disabled_at.unwrap_or_else(|| {
            Timestamp::from_unix_millis(DateTime::now().timestamp_millis())
                .expect("MongoDB current time is supported")
        });
        let mut cursor = self
            .database
            .collection::<Document>("organization_memberships")
            .find(doc! { "user_id": user_i64(user_id)?, "role": "owner" })
            .projection(doc! { "organization_id": 1 })
            .await
            .map_err(unavailable)?;
        let mut organizations = Vec::new();
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            organizations.push(parse_organization_id(
                document
                    .get_i64("organization_id")
                    .map_err(|_| AuthStoreError::InvalidData)?,
            )?);
        }
        organizations.sort_unstable();
        let mut locked = Vec::new();
        for organization_id in organizations {
            if let Err(error) = self
                .acquire_organization_lock(organization_id, operation_id, now)
                .await
            {
                for acquired in locked {
                    self.release_organization_lock(acquired, operation_id).await;
                }
                return Err(error);
            }
            locked.push(organization_id);
        }
        if disabled_at.is_some() {
            for organization_id in &locked {
                let owners = self
                    .database
                    .collection::<Document>("organization_memberships")
                    .count_documents(doc! {
                        "organization_id": organization_i64(*organization_id)?,
                        "role": "owner",
                    })
                    .await
                    .map_err(unavailable)?;
                if owners <= 1 {
                    for acquired in locked {
                        self.release_organization_lock(acquired, operation_id).await;
                    }
                    return Err(AuthStoreError::FinalOwner);
                }
            }
        }
        let update = if let Some(timestamp) = disabled_at {
            doc! { "$set": { "disabled_at": date(timestamp) } }
        } else {
            doc! { "$unset": { "disabled_at": "" } }
        };
        let result = self
            .database
            .collection::<Document>("users")
            .update_one(doc! { "_id": user_i64(user_id)? }, update)
            .await
            .map_err(unavailable)
            .and_then(|result| matched(result.matched_count));
        for organization_id in locked {
            self.release_organization_lock(organization_id, operation_id)
                .await;
        }
        result
    }

    async fn create_session_inner(&self, session: WebSession) -> Result<(), AuthStoreError> {
        self.database
            .collection::<Document>("web_sessions")
            .insert_one(encode_session(&session)?)
            .await
            .map(|_| ())
            .map_err(classify_duplicate)
    }

    async fn load_session_inner(&self, digest: SecretDigest) -> Result<WebSession, AuthStoreError> {
        let document = self
            .database
            .collection::<Document>("web_sessions")
            .find_one(doc! { "digest": digest_binary(digest) })
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::NotFound)?;
        decode_session(&document)
    }

    async fn create_api_token_inner(&self, token: ApiToken) -> Result<(), AuthStoreError> {
        self.database
            .collection::<Document>("api_tokens")
            .insert_one(encode_api_token(&token)?)
            .await
            .map(|_| ())
            .map_err(classify_duplicate)
    }

    async fn load_api_token_inner(&self, digest: SecretDigest) -> Result<ApiToken, AuthStoreError> {
        let document = self
            .database
            .collection::<Document>("api_tokens")
            .find_one(doc! { "digest": digest_binary(digest) })
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::NotFound)?;
        decode_api_token(&document)
    }

    async fn list_api_tokens_inner(
        &self,
        user_id: UserId,
        organization_id: OrganizationId,
        limit: usize,
    ) -> Result<Vec<ApiTokenView>, AuthStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(AuthStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("api_tokens")
            .find(doc! {
                "user_id": user_i64(user_id)?,
                "organization_id": organization_i64(organization_id)?,
                "revoked_at": { "$exists": false },
            })
            .sort(doc! { "created_at": -1, "_id": -1 })
            .limit(i64::try_from(limit).unwrap_or(100))
            .await
            .map_err(unavailable)?;
        let mut values = Vec::with_capacity(limit);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let token = decode_api_token(&document)?;
            values.push(ApiTokenView {
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
            });
        }
        Ok(values)
    }

    async fn load_organization_inner(
        &self,
        organization_id: OrganizationId,
    ) -> Result<OrganizationIdentity, AuthStoreError> {
        let document = self
            .database
            .collection::<Document>("organizations")
            .find_one(doc! { "_id": organization_i64(organization_id)? })
            .await
            .map_err(unavailable)?
            .ok_or(AuthStoreError::NotFound)?;
        Ok(OrganizationIdentity {
            id: organization_id,
            slug: Slug::new(
                document
                    .get_str("slug")
                    .map_err(|_| AuthStoreError::InvalidData)?,
            )
            .map_err(|_| AuthStoreError::InvalidData)?,
            display_name: DisplayName::new(
                document
                    .get_str("display_name")
                    .map_err(|_| AuthStoreError::InvalidData)?,
            )
            .map_err(|_| AuthStoreError::InvalidData)?,
            created_at: required_date(&document, "created_at")?,
        })
    }

    async fn list_organization_members_inner(
        &self,
        organization_id: OrganizationId,
        limit: usize,
    ) -> Result<Vec<OrganizationMemberView>, AuthStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(AuthStoreError::InvalidData);
        }
        let mut membership_cursor = self
            .database
            .collection::<Document>("organization_memberships")
            .find(doc! { "organization_id": organization_i64(organization_id)? })
            .sort(doc! { "created_at": 1, "_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(100))
            .await
            .map_err(unavailable)?;
        let mut memberships = Vec::with_capacity(limit);
        let mut user_ids = Vec::with_capacity(limit);
        while let Some(document) = membership_cursor.try_next().await.map_err(unavailable)? {
            let membership = decode_membership(&document)?;
            user_ids.push(user_i64(membership.user_id)?);
            memberships.push(membership);
        }
        if memberships.is_empty() {
            return Ok(Vec::new());
        }
        let mut user_cursor = self
            .database
            .collection::<Document>("users")
            .find(doc! { "_id": { "$in": user_ids } })
            .await
            .map_err(unavailable)?;
        let mut users = HashMap::with_capacity(memberships.len());
        while let Some(document) = user_cursor.try_next().await.map_err(unavailable)? {
            let user = decode_user(&document)?;
            users.insert(user.id, user);
        }
        memberships
            .into_iter()
            .map(|membership| {
                let user = users
                    .remove(&membership.user_id)
                    .ok_or(AuthStoreError::InvalidData)?;
                Ok(OrganizationMemberView {
                    user_id: user.id,
                    email: user.email.display().into(),
                    display_name: user.display_name.as_str().into(),
                    role: membership.role,
                    disabled_at: user.disabled_at,
                    joined_at: membership.created_at,
                })
            })
            .collect()
    }

    async fn list_audit_log_inner(
        &self,
        organization_id: OrganizationId,
        limit: usize,
    ) -> Result<Vec<AuditLogView>, AuthStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(AuthStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("audit_log")
            .find(doc! { "organization_id": organization_i64(organization_id)? })
            .sort(doc! { "timestamp": -1, "_id": -1 })
            .limit(i64::try_from(limit).unwrap_or(100))
            .await
            .map_err(unavailable)?;
        let mut values = Vec::with_capacity(limit);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let metadata = document
                .get_document("metadata")
                .map_err(|_| AuthStoreError::InvalidData)?
                .iter()
                .map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.as_str().into(), value.into()))
                        .ok_or(AuthStoreError::InvalidData)
                })
                .collect::<Result<Vec<_>, _>>()?;
            values.push(AuditLogView {
                request_id: document
                    .get_str("_id")
                    .map_err(|_| AuthStoreError::InvalidData)?
                    .into(),
                actor: document
                    .get_str("actor")
                    .map_err(|_| AuthStoreError::InvalidData)?
                    .into(),
                actor_user_id: parse_user_id(
                    document
                        .get_i64("actor_user_id")
                        .map_err(|_| AuthStoreError::InvalidData)?,
                )?,
                action: document
                    .get_str("action")
                    .map_err(|_| AuthStoreError::InvalidData)?
                    .into(),
                target_kind: document
                    .get_str("target_kind")
                    .map_err(|_| AuthStoreError::InvalidData)?
                    .into(),
                target_id: document
                    .get_str("target_id")
                    .map_err(|_| AuthStoreError::InvalidData)?
                    .into(),
                timestamp: required_date(&document, "timestamp")?,
                metadata,
            });
        }
        Ok(values)
    }

    async fn acquire_organization_lock(
        &self,
        organization_id: OrganizationId,
        operation_id: CredentialId,
        now: Timestamp,
    ) -> Result<(), AuthStoreError> {
        let expires_at = Timestamp::from_unix_millis(
            now.unix_millis()
                .saturating_add(i64::try_from(AUTH_LOCK_DURATION.as_millis()).unwrap_or(i64::MAX)),
        )
        .map_err(|_| AuthStoreError::InvalidData)?;
        let document = self
            .database
            .collection::<Document>("organizations")
            .find_one_and_update(
                doc! {
                    "_id": organization_i64(organization_id)?,
                    "$or": [
                        { "auth_lock": { "$exists": false } },
                        { "auth_lock.expires_at": { "$lte": date(now) } },
                        { "auth_lock.operation_id": id_i64(operation_id)? },
                    ],
                },
                doc! { "$set": { "auth_lock": {
                    "operation_id": id_i64(operation_id)?,
                    "expires_at": date(expires_at),
                }}},
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(unavailable)?;
        document
            .is_some()
            .then_some(())
            .ok_or(AuthStoreError::Unavailable)
    }

    async fn release_organization_lock(
        &self,
        organization_id: OrganizationId,
        operation_id: CredentialId,
    ) {
        let Ok(organization_id) = organization_i64(organization_id) else {
            return;
        };
        let Ok(operation_id) = id_i64(operation_id) else {
            return;
        };
        let _ = self
            .database
            .collection::<Document>("organizations")
            .update_one(
                doc! {
                    "_id": organization_id,
                    "auth_lock.operation_id": operation_id,
                },
                doc! { "$unset": { "auth_lock": "" } },
            )
            .await;
    }
}

impl AuthStore for MongoAuthStore {
    fn install_bootstrap_token(
        &self,
        token: SetupToken,
    ) -> PortFuture<'_, Result<BootstrapTokenInstall, AuthStoreError>> {
        Box::pin(self.install_bootstrap_token_inner(token))
    }

    fn consume_bootstrap(
        &self,
        identity: BootstrapIdentity,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(self.consume_bootstrap_inner(identity))
    }

    fn create_owned_organization(
        &self,
        organization: OrganizationIdentity,
        membership: OrganizationMembership,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(self.create_owned_organization_inner(organization, membership))
    }

    fn create_invited_user(
        &self,
        user: UserAccount,
        membership: OrganizationMembership,
        setup_token: SetupToken,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(self.create_invited_user_inner(user, membership, setup_token))
    }

    fn create_password_setup_token(
        &self,
        token: SetupToken,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let user_id = token.user_id.ok_or(AuthStoreError::InvalidData)?;
            let purpose = setup_purpose_name(token.purpose);
            if self
                .database
                .collection::<Document>("users")
                .find_one(doc! { "_id": user_i64(user_id)? })
                .projection(doc! { "_id": 1 })
                .await
                .map_err(unavailable)?
                .is_none()
            {
                return Err(AuthStoreError::NotFound);
            }
            let tokens = self
                .database
                .collection::<Document>("password_setup_tokens");
            if token.purpose == SetupPurpose::PasswordSetup {
                let replacement = tokens
                    .update_one(
                        doc! {
                            "user_id": user_i64(user_id)?,
                            "purpose": purpose,
                            "consumed_at": { "$exists": false },
                        },
                        doc! {
                            "$set": {
                                "digest": digest_binary(token.digest),
                                "created_at": date(token.created_at),
                                "expires_at": date(token.expires_at),
                            },
                            "$unset": {
                                "operation_id": "",
                                "operation_state": "",
                            },
                        },
                    )
                    .await
                    .map_err(classify_duplicate)?;
                if replacement.matched_count == 1 {
                    return Ok(());
                }
            }
            tokens
                .insert_one(encode_setup_token(&token)?)
                .await
                .map_err(classify_duplicate)?;
            Ok(())
        })
    }

    fn consume_password_setup(
        &self,
        digest: SecretDigest,
        now: Timestamp,
        password_hash: PasswordHash,
    ) -> PortFuture<'_, Result<UserId, AuthStoreError>> {
        Box::pin(self.consume_password_setup_inner(digest, now, password_hash))
    }

    fn load_user_by_email<'a>(
        &'a self,
        email: &'a EmailAddress,
    ) -> PortFuture<'a, Result<UserAccount, AuthStoreError>> {
        Box::pin(self.load_user_by_email_inner(email))
    }

    fn load_user(&self, user_id: UserId) -> PortFuture<'_, Result<UserAccount, AuthStoreError>> {
        Box::pin(self.load_user_inner(user_id))
    }

    fn update_password_hash(
        &self,
        user_id: UserId,
        password_hash: PasswordHash,
        changed_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(self.update_password_hash_inner(user_id, password_hash, changed_at))
    }

    fn load_membership(
        &self,
        user_id: UserId,
        organization_id: OrganizationId,
    ) -> PortFuture<'_, Result<OrganizationMembership, AuthStoreError>> {
        Box::pin(self.load_membership_inner(user_id, organization_id))
    }

    fn list_user_memberships(
        &self,
        user_id: UserId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<OrganizationMembership>, AuthStoreError>> {
        Box::pin(self.list_user_memberships_inner(user_id, limit))
    }

    fn mutate_membership(
        &self,
        mutation: MembershipMutation,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(self.mutate_membership_inner(mutation))
    }

    fn set_user_disabled(
        &self,
        user_id: UserId,
        disabled_at: Option<Timestamp>,
        operation_id: CredentialId,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(self.set_user_disabled_inner(user_id, disabled_at, operation_id))
    }

    fn create_session(&self, session: WebSession) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(self.create_session_inner(session))
    }

    fn load_session(
        &self,
        digest: SecretDigest,
    ) -> PortFuture<'_, Result<WebSession, AuthStoreError>> {
        Box::pin(self.load_session_inner(digest))
    }

    fn touch_session(
        &self,
        session_id: CredentialId,
        last_seen_at: Timestamp,
        idle_expires_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let result = self
                .database
                .collection::<Document>("web_sessions")
                .update_one(
                    doc! { "_id": id_i64(session_id)? },
                    doc! { "$set": {
                        "last_seen_at": date(last_seen_at),
                        "idle_expires_at": date(idle_expires_at),
                    }},
                )
                .await
                .map_err(unavailable)?;
            matched(result.matched_count)
        })
    }

    fn revoke_session(
        &self,
        digest: SecretDigest,
        revoked_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let result = self
                .database
                .collection::<Document>("web_sessions")
                .update_one(
                    doc! { "digest": digest_binary(digest), "revoked_at": { "$exists": false } },
                    doc! { "$set": { "revoked_at": date(revoked_at) } },
                )
                .await
                .map_err(unavailable)?;
            matched(result.matched_count)
        })
    }

    fn revoke_user_sessions(
        &self,
        user_id: UserId,
        revoked_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            self.database
                .collection::<Document>("web_sessions")
                .update_many(
                    doc! { "user_id": user_i64(user_id)?, "revoked_at": { "$exists": false } },
                    doc! { "$set": { "revoked_at": date(revoked_at) } },
                )
                .await
                .map_err(unavailable)?;
            Ok(())
        })
    }

    fn create_api_token(&self, token: ApiToken) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(self.create_api_token_inner(token))
    }

    fn load_api_token(
        &self,
        digest: SecretDigest,
    ) -> PortFuture<'_, Result<ApiToken, AuthStoreError>> {
        Box::pin(self.load_api_token_inner(digest))
    }

    fn touch_api_token(
        &self,
        token_id: CredentialId,
        last_used_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let result = self
                .database
                .collection::<Document>("api_tokens")
                .update_one(
                    doc! { "_id": id_i64(token_id)? },
                    doc! { "$set": { "last_used_at": date(last_used_at) } },
                )
                .await
                .map_err(unavailable)?;
            matched(result.matched_count)
        })
    }

    fn revoke_api_token(
        &self,
        token_id: CredentialId,
        user_id: UserId,
        organization_id: OrganizationId,
        revoked_at: Timestamp,
    ) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            let result = self
                .database
                .collection::<Document>("api_tokens")
                .update_one(
                    doc! {
                        "_id": id_i64(token_id)?,
                        "user_id": user_i64(user_id)?,
                        "organization_id": organization_i64(organization_id)?,
                        "revoked_at": { "$exists": false },
                    },
                    doc! { "$set": { "revoked_at": date(revoked_at) } },
                )
                .await
                .map_err(unavailable)?;
            matched(result.matched_count)
        })
    }

    fn project_access(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<(OrganizationId, ProjectAcceptanceState), AuthStoreError>> {
        Box::pin(async move {
            let document = self
                .database
                .collection::<Document>("projects")
                .find_one(doc! { "_id": project_id.get() })
                .projection(doc! { "organization_id": 1, "state": 1 })
                .await
                .map_err(unavailable)?
                .ok_or(AuthStoreError::NotFound)?;
            let organization_id = parse_organization_id(
                document
                    .get_i64("organization_id")
                    .map_err(|_| AuthStoreError::InvalidData)?,
            )?;
            let state = match document
                .get_str("state")
                .map_err(|_| AuthStoreError::InvalidData)?
            {
                "active" => ProjectAcceptanceState::Active,
                "disabled" => ProjectAcceptanceState::Disabled,
                "pending_delete" => ProjectAcceptanceState::PendingDelete,
                "purging" => ProjectAcceptanceState::Purging,
                "deleted" => ProjectAcceptanceState::Deleted,
                _ => return Err(AuthStoreError::InvalidData),
            };
            Ok((organization_id, state))
        })
    }

    fn append_audit(&self, record: AuditRecord) -> PortFuture<'_, Result<(), AuthStoreError>> {
        Box::pin(async move {
            self.database
                .collection::<Document>("audit_log")
                .update_one(
                    doc! { "_id": record.request_id.as_str() },
                    doc! { "$setOnInsert": encode_audit_fields(&record)? },
                )
                .upsert(true)
                .await
                .map_err(classify_duplicate)?;
            Ok(())
        })
    }

    fn list_api_tokens(
        &self,
        user_id: UserId,
        organization_id: OrganizationId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ApiTokenView>, AuthStoreError>> {
        Box::pin(self.list_api_tokens_inner(user_id, organization_id, limit))
    }

    fn load_organization(
        &self,
        organization_id: OrganizationId,
    ) -> PortFuture<'_, Result<OrganizationIdentity, AuthStoreError>> {
        Box::pin(self.load_organization_inner(organization_id))
    }

    fn list_organization_members(
        &self,
        organization_id: OrganizationId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<OrganizationMemberView>, AuthStoreError>> {
        Box::pin(self.list_organization_members_inner(organization_id, limit))
    }

    fn list_audit_log(
        &self,
        organization_id: OrganizationId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<AuditLogView>, AuthStoreError>> {
        Box::pin(self.list_audit_log_inner(organization_id, limit))
    }
}

pub(crate) async fn create_auth_collections(
    database: &Database,
) -> Result<(), crate::MongoBootstrapError> {
    for (name, validator) in [
        ("users", user_validator()),
        ("organization_memberships", membership_validator()),
        ("web_sessions", session_validator()),
        ("api_tokens", api_token_validator()),
        ("password_setup_tokens", setup_token_validator()),
        ("audit_log", audit_validator()),
    ] {
        database
            .run_command(doc! {
                "create": name,
                "validator": validator,
                "validationLevel": "strict",
                "validationAction": "error",
            })
            .await?;
    }
    Ok(())
}

pub(crate) async fn create_auth_indexes(
    database: &Database,
) -> Result<(), crate::MongoBootstrapError> {
    database
        .collection::<Document>("users")
        .create_index(index(
            doc! { "canonical_email": 1 },
            "user_canonical_email_unique",
            true,
        ))
        .await?;
    let memberships = database.collection::<Document>("organization_memberships");
    memberships
        .create_index(index(
            doc! { "organization_id": 1, "user_id": 1 },
            "membership_org_user_unique",
            true,
        ))
        .await?;
    memberships
        .create_index(index(
            doc! { "user_id": 1, "organization_id": 1 },
            "membership_user_org",
            false,
        ))
        .await?;
    memberships
        .create_index(index(
            doc! { "organization_id": 1, "role": 1 },
            "membership_org_role",
            false,
        ))
        .await?;
    let sessions = database.collection::<Document>("web_sessions");
    sessions
        .create_index(index(doc! { "digest": 1 }, "session_digest_unique", true))
        .await?;
    sessions
        .create_index(ttl_index("absolute_expires_at", "session_absolute_ttl"))
        .await?;
    sessions
        .create_index(index(
            doc! { "user_id": 1, "revoked_at": 1 },
            "session_user_active",
            false,
        ))
        .await?;
    let tokens = database.collection::<Document>("api_tokens");
    tokens
        .create_index(index(doc! { "digest": 1 }, "api_token_digest_unique", true))
        .await?;
    tokens
        .create_index(ttl_index("expires_at", "api_token_expiry_ttl"))
        .await?;
    tokens
        .create_index(index(
            doc! { "user_id": 1, "organization_id": 1, "revoked_at": 1 },
            "api_token_user_org",
            false,
        ))
        .await?;
    let setup = database.collection::<Document>("password_setup_tokens");
    setup
        .create_index(index(doc! { "digest": 1 }, "setup_digest_unique", true))
        .await?;
    setup
        .create_index(ttl_index("expires_at", "setup_expiry_ttl"))
        .await?;
    setup
        .create_index(
            IndexModel::builder()
                .keys(doc! { "purpose": 1 })
                .options(
                    IndexOptions::builder()
                        .name("bootstrap_singleton".to_owned())
                        .unique(true)
                        .partial_filter_expression(doc! { "purpose": "bootstrap" })
                        .build(),
                )
                .build(),
        )
        .await?;
    database
        .collection::<Document>("audit_log")
        .create_index(index(
            doc! { "organization_id": 1, "timestamp": -1 },
            "audit_org_time",
            false,
        ))
        .await?;
    Ok(())
}

pub(crate) async fn validate_auth_indexes(
    database: &Database,
) -> Result<bool, crate::MongoBootstrapError> {
    for collection in [
        "users",
        "organization_memberships",
        "web_sessions",
        "api_tokens",
        "password_setup_tokens",
        "audit_log",
    ] {
        let names = database
            .collection::<Document>(collection)
            .list_index_names()
            .await?;
        let actual = names.iter().map(String::as_str).collect::<BTreeSet<_>>();
        if actual != auth_index_names(collection) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn auth_index_names(collection: &str) -> BTreeSet<&'static str> {
    match collection {
        "users" => BTreeSet::from(["_id_", "user_canonical_email_unique"]),
        "organization_memberships" => BTreeSet::from([
            "_id_",
            "membership_org_role",
            "membership_org_user_unique",
            "membership_user_org",
        ]),
        "web_sessions" => BTreeSet::from([
            "_id_",
            "session_absolute_ttl",
            "session_digest_unique",
            "session_user_active",
        ]),
        "api_tokens" => BTreeSet::from([
            "_id_",
            "api_token_digest_unique",
            "api_token_expiry_ttl",
            "api_token_user_org",
        ]),
        "password_setup_tokens" => BTreeSet::from([
            "_id_",
            "bootstrap_singleton",
            "setup_digest_unique",
            "setup_expiry_ttl",
        ]),
        "audit_log" => BTreeSet::from(["_id_", "audit_org_time"]),
        _ => BTreeSet::new(),
    }
}

pub(crate) fn user_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "email", "canonical_email", "display_name", "created_at"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "long", "minimum": 1 },
            "email": { "bsonType": "string", "minLength": 3, "maxLength": 254 },
            "canonical_email": { "bsonType": "string", "minLength": 3, "maxLength": 254 },
            "display_name": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
            "password_hash": { "bsonType": "string", "minLength": 1, "maxLength": 512 },
            "password_changed_at": { "bsonType": "date" },
            "disabled_at": { "bsonType": "date" },
            "created_at": { "bsonType": "date" },
        }
    }}
}

pub(crate) fn membership_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "organization_id", "user_id", "role", "created_at", "created_by"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "organization_id": { "bsonType": "long", "minimum": 1 },
            "user_id": { "bsonType": "long", "minimum": 1 },
            "role": { "enum": ["owner", "admin", "member", "viewer"] },
            "created_at": { "bsonType": "date" },
            "created_by": { "bsonType": "long", "minimum": 1 },
        }
    }}
}

pub(crate) fn session_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "digest", "csrf_digest", "user_id", "created_at", "last_seen_at", "idle_expires_at", "absolute_expires_at"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "long", "minimum": 1 },
            "digest": { "bsonType": "binData" },
            "csrf_digest": { "bsonType": "binData" },
            "user_id": { "bsonType": "long", "minimum": 1 },
            "created_at": { "bsonType": "date" },
            "last_seen_at": { "bsonType": "date" },
            "idle_expires_at": { "bsonType": "date" },
            "absolute_expires_at": { "bsonType": "date" },
            "revoked_at": { "bsonType": "date" },
        }
    }}
}

pub(crate) fn api_token_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "digest", "user_id", "organization_id", "name", "scopes", "created_at", "expires_at"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "long", "minimum": 1 },
            "digest": { "bsonType": "binData" },
            "user_id": { "bsonType": "long", "minimum": 1 },
            "organization_id": { "bsonType": "long", "minimum": 1 },
            "name": { "bsonType": "string", "minLength": 1, "maxLength": 64 },
            "scopes": {
                "bsonType": "array",
                "maxItems": 14,
                "uniqueItems": true,
                "items": { "bsonType": "string", "minLength": 1, "maxLength": 64 },
            },
            "created_at": { "bsonType": "date" },
            "expires_at": { "bsonType": "date" },
            "last_used_at": { "bsonType": "date" },
            "revoked_at": { "bsonType": "date" },
        }
    }}
}

pub(crate) fn setup_token_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "digest", "purpose", "created_at", "expires_at"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "long", "minimum": 1 },
            "digest": { "bsonType": "binData" },
            "purpose": { "enum": ["bootstrap", "password_setup", "password_reset"] },
            "user_id": { "bsonType": "long", "minimum": 1 },
            "created_at": { "bsonType": "date" },
            "expires_at": { "bsonType": "date" },
            "consumed_at": { "bsonType": "date" },
            "operation_id": { "bsonType": "long", "minimum": 1 },
            "operation_state": { "enum": ["applying", "complete"] },
        }
    }}
}

pub(crate) fn audit_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "organization_id", "actor", "actor_user_id", "action", "target_kind", "target_id", "timestamp", "metadata"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "string", "minLength": 1, "maxLength": 64 },
            "organization_id": { "bsonType": "long", "minimum": 1 },
            "actor": { "enum": ["web_session", "personal_api_token", "bootstrap"] },
            "actor_user_id": { "bsonType": "long", "minimum": 1 },
            "action": { "bsonType": "string", "minLength": 1, "maxLength": 64 },
            "target_kind": { "enum": ["user", "api_token", "project", "project_key", "project_deletion", "incident_capsule", "notification_destination", "alert_rule", "replay"] },
            "target_id": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
            "timestamp": { "bsonType": "date" },
            "metadata": {
                "bsonType": "object",
                "additionalProperties": false,
                "properties": {
                    "role": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
                    "credential_kind": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
                    "outcome": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
                    "project_id": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
                    "selected_event_count": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
                    "result_size_class": { "enum": ["small", "medium", "large"] },
                },
            },
        }
    }}
}

fn encode_user(user: &UserAccount) -> Result<Document, AuthStoreError> {
    let mut document = doc! { "_id": user_i64(user.id)? };
    document.extend(encode_user_fields(user)?);
    Ok(document)
}

fn encode_user_fields(user: &UserAccount) -> Result<Document, AuthStoreError> {
    let mut document = doc! {
        "email": user.email.display(),
        "canonical_email": user.email.canonical(),
        "display_name": user.display_name.as_str(),
        "created_at": date(user.created_at),
    };
    if let Some(hash) = &user.password_hash {
        document.insert("password_hash", hash.expose());
    }
    if let Some(disabled_at) = user.disabled_at {
        document.insert("disabled_at", date(disabled_at));
    }
    Ok(document)
}

fn decode_user(document: &Document) -> Result<UserAccount, AuthStoreError> {
    let id = parse_user_id(
        document
            .get_i64("_id")
            .map_err(|_| AuthStoreError::InvalidData)?,
    )?;
    let email = EmailAddress::parse(
        document
            .get_str("email")
            .map_err(|_| AuthStoreError::InvalidData)?,
    )
    .map_err(|_| AuthStoreError::InvalidData)?;
    if email.canonical()
        != document
            .get_str("canonical_email")
            .map_err(|_| AuthStoreError::InvalidData)?
    {
        return Err(AuthStoreError::InvalidData);
    }
    Ok(UserAccount {
        id,
        email,
        display_name: UserDisplayName::new(
            document
                .get_str("display_name")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )
        .map_err(|_| AuthStoreError::InvalidData)?,
        password_hash: document
            .get_str("password_hash")
            .ok()
            .map(PasswordHash::new)
            .transpose()
            .map_err(|_| AuthStoreError::InvalidData)?,
        disabled_at: optional_date(document, "disabled_at")?,
        created_at: required_date(document, "created_at")?,
    })
}

fn encode_membership(membership: &OrganizationMembership) -> Result<Document, AuthStoreError> {
    let mut document = doc! {
        "_id": membership_id(membership.organization_id, membership.user_id)?,
    };
    document.extend(encode_membership_fields(membership)?);
    Ok(document)
}

fn encode_membership_fields(
    membership: &OrganizationMembership,
) -> Result<Document, AuthStoreError> {
    Ok(doc! {
        "organization_id": organization_i64(membership.organization_id)?,
        "user_id": user_i64(membership.user_id)?,
        "role": role_name(membership.role),
        "created_at": date(membership.created_at),
        "created_by": user_i64(membership.created_by)?,
    })
}

fn decode_membership(document: &Document) -> Result<OrganizationMembership, AuthStoreError> {
    Ok(OrganizationMembership {
        organization_id: parse_organization_id(
            document
                .get_i64("organization_id")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )?,
        user_id: parse_user_id(
            document
                .get_i64("user_id")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )?,
        role: parse_role(
            document
                .get_str("role")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )?,
        created_at: required_date(document, "created_at")?,
        created_by: parse_user_id(
            document
                .get_i64("created_by")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )?,
    })
}

fn encode_setup_token(token: &SetupToken) -> Result<Document, AuthStoreError> {
    let mut document = doc! {
        "_id": id_i64(token.id)?,
        "digest": digest_binary(token.digest),
        "purpose": setup_purpose_name(token.purpose),
        "created_at": date(token.created_at),
        "expires_at": date(token.expires_at),
    };
    if let Some(user_id) = token.user_id {
        document.insert("user_id", user_i64(user_id)?);
    }
    if let Some(consumed_at) = token.consumed_at {
        document.insert("consumed_at", date(consumed_at));
    }
    Ok(document)
}

fn encode_session(session: &WebSession) -> Result<Document, AuthStoreError> {
    let mut document = doc! {
        "_id": id_i64(session.id)?,
        "digest": digest_binary(session.digest),
        "csrf_digest": digest_binary(session.csrf_digest),
        "user_id": user_i64(session.user_id)?,
        "created_at": date(session.created_at),
        "last_seen_at": date(session.last_seen_at),
        "idle_expires_at": date(session.idle_expires_at),
        "absolute_expires_at": date(session.absolute_expires_at),
    };
    if let Some(revoked_at) = session.revoked_at {
        document.insert("revoked_at", date(revoked_at));
    }
    Ok(document)
}

fn decode_session(document: &Document) -> Result<WebSession, AuthStoreError> {
    Ok(WebSession {
        id: parse_credential_id(
            document
                .get_i64("_id")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )?,
        digest: parse_digest(document, "digest")?,
        csrf_digest: parse_digest(document, "csrf_digest")?,
        user_id: parse_user_id(
            document
                .get_i64("user_id")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )?,
        created_at: required_date(document, "created_at")?,
        last_seen_at: required_date(document, "last_seen_at")?,
        idle_expires_at: required_date(document, "idle_expires_at")?,
        absolute_expires_at: required_date(document, "absolute_expires_at")?,
        revoked_at: optional_date(document, "revoked_at")?,
    })
}

fn encode_api_token(token: &ApiToken) -> Result<Document, AuthStoreError> {
    let mut document = doc! {
        "_id": id_i64(token.id)?,
        "digest": digest_binary(token.digest),
        "user_id": user_i64(token.user_id)?,
        "organization_id": organization_i64(token.organization_id)?,
        "name": token.name.as_str(),
        "scopes": token.scopes.iter().map(|scope| scope.scope()).collect::<Vec<_>>(),
        "created_at": date(token.created_at),
        "expires_at": date(token.expires_at),
    };
    if let Some(last_used_at) = token.last_used_at {
        document.insert("last_used_at", date(last_used_at));
    }
    if let Some(revoked_at) = token.revoked_at {
        document.insert("revoked_at", date(revoked_at));
    }
    Ok(document)
}

fn decode_api_token(document: &Document) -> Result<ApiToken, AuthStoreError> {
    let scopes = document
        .get_array("scopes")
        .map_err(|_| AuthStoreError::InvalidData)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or(AuthStoreError::InvalidData)
                .and_then(|value| {
                    Permission::parse_scope(value).map_err(|_| AuthStoreError::InvalidData)
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ApiToken {
        id: parse_credential_id(
            document
                .get_i64("_id")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )?,
        digest: parse_digest(document, "digest")?,
        user_id: parse_user_id(
            document
                .get_i64("user_id")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )?,
        organization_id: parse_organization_id(
            document
                .get_i64("organization_id")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )?,
        name: TokenName::new(
            document
                .get_str("name")
                .map_err(|_| AuthStoreError::InvalidData)?,
        )
        .map_err(|_| AuthStoreError::InvalidData)?,
        scopes: PermissionSet::from_permissions(scopes),
        created_at: required_date(document, "created_at")?,
        expires_at: required_date(document, "expires_at")?,
        last_used_at: optional_date(document, "last_used_at")?,
        revoked_at: optional_date(document, "revoked_at")?,
    })
}

fn encode_audit_fields(record: &AuditRecord) -> Result<Document, AuthStoreError> {
    let metadata = record
        .metadata
        .values()
        .iter()
        .map(|(key, value)| {
            (
                key.name().to_owned(),
                Bson::String(value.as_str().to_owned()),
            )
        })
        .collect::<Document>();
    Ok(doc! {
        "organization_id": organization_i64(record.organization_id)?,
        "actor": actor_name(record.actor),
        "actor_user_id": user_i64(record.actor_user_id)?,
        "action": record.action.name(),
        "target_kind": record.target_kind,
        "target_id": record.target_id.as_str(),
        "timestamp": date(record.timestamp),
        "metadata": metadata,
    })
}

fn index(keys: Document, name: &str, unique: bool) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .unique(unique)
                .build(),
        )
        .build()
}

fn ttl_index(field: &str, name: &str) -> IndexModel {
    IndexModel::builder()
        .keys(doc! { field: 1 })
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .expire_after(Duration::ZERO)
                .build(),
        )
        .build()
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn required_date(document: &Document, field: &str) -> Result<Timestamp, AuthStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(field)
            .map_err(|_| AuthStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| AuthStoreError::InvalidData)
}

fn optional_date(document: &Document, field: &str) -> Result<Option<Timestamp>, AuthStoreError> {
    match document.get(field) {
        None => Ok(None),
        Some(Bson::DateTime(value)) => Timestamp::from_unix_millis(value.timestamp_millis())
            .map(Some)
            .map_err(|_| AuthStoreError::InvalidData),
        Some(_) => Err(AuthStoreError::InvalidData),
    }
}

fn digest_binary(digest: SecretDigest) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: digest.expose().to_vec(),
    }
}

fn parse_digest(document: &Document, field: &str) -> Result<SecretDigest, AuthStoreError> {
    let bytes: [u8; 32] = document
        .get_binary_generic(field)
        .map_err(|_| AuthStoreError::InvalidData)?
        .as_slice()
        .try_into()
        .map_err(|_| AuthStoreError::InvalidData)?;
    Ok(SecretDigest::new(bytes))
}

fn membership_id(
    organization_id: OrganizationId,
    user_id: UserId,
) -> Result<Binary, AuthStoreError> {
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&organization_id.get().to_be_bytes());
    bytes[8..].copy_from_slice(&user_id.get().to_be_bytes());
    Ok(Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    })
}

fn organization_i64(value: OrganizationId) -> Result<i64, AuthStoreError> {
    i64::try_from(value.get()).map_err(|_| AuthStoreError::InvalidData)
}

fn user_i64(value: UserId) -> Result<i64, AuthStoreError> {
    i64::try_from(value.get()).map_err(|_| AuthStoreError::InvalidData)
}

fn id_i64(value: CredentialId) -> Result<i64, AuthStoreError> {
    i64::try_from(value.get()).map_err(|_| AuthStoreError::InvalidData)
}

fn parse_organization_id(value: i64) -> Result<OrganizationId, AuthStoreError> {
    OrganizationId::new(u64::try_from(value).map_err(|_| AuthStoreError::InvalidData)?)
        .map_err(|_| AuthStoreError::InvalidData)
}

fn parse_user_id(value: i64) -> Result<UserId, AuthStoreError> {
    UserId::new(u64::try_from(value).map_err(|_| AuthStoreError::InvalidData)?)
        .map_err(|_| AuthStoreError::InvalidData)
}

fn parse_credential_id(value: i64) -> Result<CredentialId, AuthStoreError> {
    CredentialId::new(u64::try_from(value).map_err(|_| AuthStoreError::InvalidData)?)
        .map_err(|_| AuthStoreError::InvalidData)
}

fn role_name(role: OrganizationRole) -> &'static str {
    role.name()
}

fn parse_role(value: &str) -> Result<OrganizationRole, AuthStoreError> {
    match value {
        "owner" => Ok(OrganizationRole::Owner),
        "admin" => Ok(OrganizationRole::Admin),
        "member" => Ok(OrganizationRole::Member),
        "viewer" => Ok(OrganizationRole::Viewer),
        _ => Err(AuthStoreError::InvalidData),
    }
}

fn setup_purpose_name(purpose: SetupPurpose) -> &'static str {
    match purpose {
        SetupPurpose::Bootstrap => "bootstrap",
        SetupPurpose::PasswordSetup => "password_setup",
        SetupPurpose::PasswordReset => "password_reset",
    }
}

fn actor_name(actor: Actor) -> &'static str {
    match actor {
        Actor::WebSession => "web_session",
        Actor::PersonalApiToken => "personal_api_token",
        Actor::Bootstrap => "bootstrap",
    }
}

fn matched(count: u64) -> Result<(), AuthStoreError> {
    (count == 1).then_some(()).ok_or(AuthStoreError::NotFound)
}

fn unavailable(_: MongoError) -> AuthStoreError {
    AuthStoreError::Unavailable
}

fn classify_duplicate(error: MongoError) -> AuthStoreError {
    if duplicate_write(&error) {
        AuthStoreError::IdentityCollision
    } else {
        AuthStoreError::Unavailable
    }
}

fn classify_user_duplicate(error: MongoError) -> AuthStoreError {
    if duplicate_message(&error)
        .is_some_and(|message| message.contains("user_canonical_email_unique"))
    {
        AuthStoreError::AlreadyExists
    } else {
        classify_duplicate(error)
    }
}

fn classify_organization_duplicate(error: MongoError) -> AuthStoreError {
    if duplicate_message(&error).is_some_and(|message| message.contains("organization_slug_unique"))
    {
        AuthStoreError::AlreadyExists
    } else {
        classify_duplicate(error)
    }
}

fn classify_membership_duplicate(error: MongoError) -> AuthStoreError {
    if duplicate_write(&error) {
        AuthStoreError::AlreadyExists
    } else {
        AuthStoreError::Unavailable
    }
}

fn duplicate_write(error: &MongoError) -> bool {
    duplicate_message(error).is_some()
}

fn duplicate_message(error: &MongoError) -> Option<&str> {
    match error.kind.as_ref() {
        ErrorKind::Write(WriteFailure::WriteError(write)) if write.code == 11000 => {
            Some(&write.message)
        }
        _ => None,
    }
}
