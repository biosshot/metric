//! Authoritative identity, credential, and authorization application service.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{
        PasswordHash as ParsedPasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    },
};
use faultkeep_domain::{
    DisplayName, OrganizationId, OrganizationIdentity, ProjectId, Slug, Timestamp,
    api::{ApiTokenView, AuditLogView, OrganizationMemberView},
    auth::{
        Actor, ApiToken, AuditAction, AuditMetadata, AuditMetadataKey, AuditMetadataValue,
        AuditRecord, AuditTargetId, AuthContext, BootstrapIdentity, CredentialId, EmailAddress,
        MembershipMutation, MembershipMutationKind, OrganizationMembership, OrganizationRole,
        PasswordHash, Permission, PermissionSet, PlainSecret, RequestCorrelationId, SecretDigest,
        SetupPurpose, SetupToken, TokenName, UserAccount, UserDisplayName, UserId, WebSession,
    },
    issue::{ActorKind, ActorRef},
};
use faultkeep_ports::{AuthStore, AuthStoreError, BootstrapTokenInstall, Clock, RandomSource};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::time::timeout;

const MIN_PASSWORD_BYTES: usize = 12;
const MAX_PASSWORD_BYTES: usize = 1_024;
const MIN_ARGON2_MEMORY_KIB: u32 = 19 * 1_024;
const MAX_ARGON2_MEMORY_KIB: u32 = 1024 * 1_024;
const MIN_ARGON2_ITERATIONS: u32 = 2;
const MAX_ARGON2_ITERATIONS: u32 = 20;
const MAX_ARGON2_PARALLELISM: u32 = 16;

#[derive(Debug, Clone, Copy)]
pub struct PasswordConfig {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub max_concurrency: usize,
}

impl Default for PasswordConfig {
    fn default() -> Self {
        Self {
            memory_kib: MIN_ARGON2_MEMORY_KIB,
            iterations: MIN_ARGON2_ITERATIONS,
            parallelism: 1,
            max_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LoginRateLimitConfig {
    pub max_attempts: u32,
    pub window: Duration,
    pub capacity: usize,
}

impl Default for LoginRateLimitConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            window: Duration::from_secs(60),
            capacity: 10_000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AuthConfig {
    pub identity_collision_retries: usize,
    pub session_idle_timeout: Duration,
    pub session_absolute_timeout: Duration,
    pub activity_touch_interval: Duration,
    pub setup_token_timeout: Duration,
    pub max_api_token_lifetime: Duration,
    pub store_timeout: Duration,
    pub password: PasswordConfig,
    pub login_rate_limit: LoginRateLimitConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            identity_collision_retries: 16,
            session_idle_timeout: Duration::from_secs(7 * 24 * 60 * 60),
            session_absolute_timeout: Duration::from_secs(30 * 24 * 60 * 60),
            activity_touch_interval: Duration::from_secs(5 * 60),
            setup_token_timeout: Duration::from_secs(24 * 60 * 60),
            max_api_token_lifetime: Duration::from_secs(365 * 24 * 60 * 60),
            store_timeout: Duration::from_secs(5),
            password: PasswordConfig::default(),
            login_rate_limit: LoginRateLimitConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthError {
    #[error("authentication configuration is invalid")]
    InvalidConfiguration,
    #[error("authentication failed")]
    InvalidCredentials,
    #[error("login is temporarily rate limited")]
    RateLimited,
    #[error("authorization denied")]
    Forbidden,
    #[error("authentication credential is invalid or expired")]
    InvalidCredential,
    #[error("identity already exists")]
    AlreadyExists,
    #[error("generated identity collides with an existing record")]
    IdentityCollision,
    #[error("identity collision retry limit was exhausted")]
    CollisionExhausted,
    #[error("identity does not exist")]
    NotFound,
    #[error("the final organization owner cannot be removed, disabled, or demoted")]
    FinalOwner,
    #[error("bootstrap is unavailable")]
    BootstrapClosed,
    #[error("password does not satisfy the bounded policy")]
    InvalidPassword,
    #[error("requested token scopes or lifetime are invalid")]
    InvalidTokenPolicy,
    #[error("cryptographic randomness is unavailable")]
    RandomUnavailable,
    #[error("authentication operation is temporarily unavailable")]
    Unavailable,
}

impl AuthError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "auth_invalid_configuration",
            Self::InvalidCredentials => "auth_invalid_credentials",
            Self::RateLimited => "auth_rate_limited",
            Self::Forbidden => "auth_forbidden",
            Self::InvalidCredential => "auth_invalid_credential",
            Self::AlreadyExists => "auth_identity_exists",
            Self::IdentityCollision => "auth_identity_collision",
            Self::CollisionExhausted => "auth_identity_collision_exhausted",
            Self::NotFound => "auth_identity_not_found",
            Self::FinalOwner => "auth_final_owner",
            Self::BootstrapClosed => "auth_bootstrap_closed",
            Self::InvalidPassword => "auth_invalid_password",
            Self::InvalidTokenPolicy => "auth_invalid_token_policy",
            Self::RandomUnavailable => "auth_random_unavailable",
            Self::Unavailable => "auth_unavailable",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PasswordInput(Box<str>);

impl PasswordInput {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, AuthError> {
        let value = value.into();
        if value.len() < MIN_PASSWORD_BYTES
            || value.len() > MAX_PASSWORD_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(AuthError::InvalidPassword);
        }
        Ok(Self(value))
    }

    fn login(value: impl Into<Box<str>>) -> Result<Self, AuthError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_PASSWORD_BYTES {
            return Err(AuthError::InvalidCredentials);
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for PasswordInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PasswordInput(<redacted>)")
    }
}

#[derive(Debug, Clone)]
pub struct BootstrapRequest {
    pub setup_secret: PlainSecret,
    pub email: EmailAddress,
    pub user_display_name: UserDisplayName,
    pub password: PasswordInput,
    pub organization_slug: Slug,
    pub organization_name: DisplayName,
    pub request_id: RequestCorrelationId,
}

#[derive(Debug, Clone)]
pub struct LoginRequest {
    pub email: Box<str>,
    pub password: Box<str>,
    pub organization_id: OrganizationId,
    pub client_network_digest: SecretDigest,
    pub request_id: RequestCorrelationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedWebSession {
    pub session: PlainSecret,
    pub csrf: PlainSecret,
    pub absolute_expires_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedApiToken {
    pub id: CredentialId,
    pub secret: PlainSecret,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct InviteUserRequest {
    pub email: EmailAddress,
    pub display_name: UserDisplayName,
    pub role: OrganizationRole,
    pub request_id: RequestCorrelationId,
}

#[derive(Debug, Clone)]
pub struct CreateApiTokenRequest {
    pub name: TokenName,
    pub scopes: PermissionSet,
    pub expires_at: Timestamp,
    pub request_id: RequestCorrelationId,
}

pub struct IdentityService {
    store: Arc<dyn AuthStore>,
    clock: Arc<dyn Clock>,
    random: Arc<dyn RandomSource>,
    config: AuthConfig,
    passwords: PasswordEngine,
    login_limiter: LoginRateLimiter,
}

impl IdentityService {
    pub fn new(
        store: Arc<dyn AuthStore>,
        clock: Arc<dyn Clock>,
        random: Arc<dyn RandomSource>,
        config: AuthConfig,
    ) -> Result<Self, AuthError> {
        validate_config(config)?;
        let passwords = PasswordEngine::new(config.password, Arc::clone(&random))?;
        Ok(Self {
            store,
            clock,
            random,
            passwords,
            login_limiter: LoginRateLimiter::new(config.login_rate_limit)?,
            config,
        })
    }

    pub async fn ensure_bootstrap_token(&self) -> Result<Option<PlainSecret>, AuthError> {
        for _ in 0..self.config.identity_collision_retries {
            let id = self.random_id()?;
            let secret = self.random_secret()?;
            let now = self.clock.now();
            let token = SetupToken {
                id,
                digest: digest(secret.expose()),
                purpose: SetupPurpose::Bootstrap,
                user_id: None,
                created_at: now,
                expires_at: add_duration(now, self.config.setup_token_timeout)?,
                consumed_at: None,
            };
            match self.call(self.store.install_bootstrap_token(token)).await {
                Ok(BootstrapTokenInstall::Created) => return Ok(Some(secret)),
                Ok(BootstrapTokenInstall::AlreadyInstalled) => return Ok(None),
                Ok(BootstrapTokenInstall::Closed) => return Err(AuthError::BootstrapClosed),
                Err(AuthError::IdentityCollision) => {}
                Err(error) => return Err(error),
            }
        }
        Err(AuthError::CollisionExhausted)
    }

    pub async fn bootstrap(&self, request: BootstrapRequest) -> Result<AuthContext, AuthError> {
        let token_digest = digest(request.setup_secret.expose());
        let operation_id = derived_id(&token_digest, 0)?;
        let organization_id = OrganizationId::new(derived_u63(&token_digest, 8))
            .map_err(|_| AuthError::Unavailable)?;
        let user_id =
            UserId::new(derived_u63(&token_digest, 16)).map_err(|_| AuthError::Unavailable)?;
        let now = self.clock.now();
        let password_hash = self.passwords.hash(request.password).await?;
        let user = UserAccount {
            id: user_id,
            email: request.email,
            display_name: request.user_display_name,
            password_hash: Some(password_hash),
            disabled_at: None,
            created_at: now,
        };
        let membership = OrganizationMembership {
            organization_id,
            user_id,
            role: OrganizationRole::Owner,
            created_at: now,
            created_by: user_id,
        };
        self.call(self.store.consume_bootstrap(BootstrapIdentity {
            operation_id,
            token_digest,
            organization_id,
            organization_slug: request.organization_slug,
            organization_name: request.organization_name,
            user,
            membership,
            timestamp: now,
        }))
        .await?;
        let context = AuthContext {
            actor: Actor::Bootstrap,
            user_id,
            organization_id,
            role: OrganizationRole::Owner,
            permissions: PermissionSet::from_role(OrganizationRole::Owner),
            credential_id: operation_id,
        };
        self.audit(
            &context,
            request.request_id,
            AuditAction::PasswordSetup,
            "user",
            user_id.get().to_string(),
            AuditMetadata::new([(
                AuditMetadataKey::Role,
                AuditMetadataValue::new("owner").expect("static role is bounded"),
            )])
            .expect("static audit metadata is valid"),
        )
        .await?;
        Ok(context)
    }

    pub async fn login(&self, request: LoginRequest) -> Result<IssuedWebSession, AuthError> {
        let now = self.clock.now();
        let account_key = account_rate_key(&request.email);
        if !self
            .login_limiter
            .check(account_key, request.client_network_digest, now)
        {
            metrics::counter!(
                "faultkeep_auth_login_total",
                "outcome" => "rate_limited"
            )
            .increment(1);
            return Err(AuthError::RateLimited);
        }

        let email = EmailAddress::parse(request.email).map_err(|_| AuthError::InvalidCredentials);
        let user = match email {
            Ok(ref email) => match self.call(self.store.load_user_by_email(email)).await {
                Ok(user) => Some(user),
                Err(AuthError::NotFound) => None,
                Err(error) => return Err(error),
            },
            Err(_) => None,
        };
        let password = PasswordInput::login(request.password)?;
        let upgrade_password = password.clone();
        let encoded = user
            .as_ref()
            .and_then(|user| user.password_hash.as_ref())
            .unwrap_or(self.passwords.dummy_hash());
        let needs_upgrade = self.passwords.needs_upgrade(encoded);
        let password_valid = self.passwords.verify(password, encoded.clone()).await?;
        let valid_user = user.filter(|user| password_valid && user.disabled_at.is_none());
        let Some(user) = valid_user else {
            metrics::counter!(
                "faultkeep_auth_login_total",
                "outcome" => "invalid"
            )
            .increment(1);
            return Err(AuthError::InvalidCredentials);
        };
        let membership = match self
            .call(self.store.load_membership(user.id, request.organization_id))
            .await
        {
            Ok(membership) => membership,
            Err(AuthError::NotFound) => return Err(AuthError::InvalidCredentials),
            Err(error) => return Err(error),
        };
        if needs_upgrade && let Ok(upgraded) = self.passwords.hash(upgrade_password).await {
            let _ = self
                .call(self.store.update_password_hash(user.id, upgraded, now))
                .await;
        }

        self.call(self.store.revoke_user_sessions(user.id, now))
            .await?;
        let issued = self.create_session(user.id, now).await?;
        let context = context_for_membership(
            Actor::WebSession,
            issued.0.id,
            &membership,
            PermissionSet::from_role(membership.role),
        );
        self.audit(
            &context,
            request.request_id,
            AuditAction::LoginSucceeded,
            "user",
            user.id.get().to_string(),
            AuditMetadata::new([(
                AuditMetadataKey::CredentialKind,
                AuditMetadataValue::new("web_session").expect("static value is bounded"),
            )])
            .expect("static audit metadata is valid"),
        )
        .await?;
        self.login_limiter.success(account_key);
        metrics::counter!(
            "faultkeep_auth_login_total",
            "outcome" => "ok"
        )
        .increment(1);
        Ok(issued.1)
    }

    pub async fn authenticate_session(
        &self,
        session_secret: &PlainSecret,
        csrf: Option<&PlainSecret>,
        state_changing: bool,
        organization_id: OrganizationId,
    ) -> Result<AuthContext, AuthError> {
        let now = self.clock.now();
        let session = self
            .call(self.store.load_session(digest(session_secret.expose())))
            .await
            .map_err(credential_error)?;
        if session.revoked_at.is_some()
            || now >= session.idle_expires_at
            || now >= session.absolute_expires_at
            || (state_changing
                && !csrf.is_some_and(|secret| {
                    constant_digest_eq(digest(secret.expose()), session.csrf_digest)
                }))
        {
            return Err(AuthError::InvalidCredential);
        }
        let (user, membership) = self
            .authoritative_identity(session.user_id, organization_id)
            .await?;
        if user.disabled_at.is_some() {
            return Err(AuthError::InvalidCredential);
        }
        if elapsed_at_least(
            session.last_seen_at,
            now,
            self.config.activity_touch_interval,
        ) {
            let idle = add_duration(now, self.config.session_idle_timeout)?
                .min(session.absolute_expires_at);
            let _ = self
                .call(self.store.touch_session(session.id, now, idle))
                .await;
        }
        Ok(context_for_membership(
            Actor::WebSession,
            session.id,
            &membership,
            PermissionSet::from_role(membership.role),
        ))
    }

    pub async fn logout(&self, session_secret: &PlainSecret) -> Result<(), AuthError> {
        self.call(
            self.store
                .revoke_session(digest(session_secret.expose()), self.clock.now()),
        )
        .await
        .map_err(credential_error)
    }

    pub async fn invite_user(
        &self,
        context: &AuthContext,
        request: InviteUserRequest,
    ) -> Result<PlainSecret, AuthError> {
        require(context, Permission::OrganizationAdmin)?;
        if request.role == OrganizationRole::Owner {
            require(context, Permission::OrganizationOwner)?;
        }
        for _ in 0..self.config.identity_collision_retries {
            let user_id =
                UserId::new(self.random_u63()?).map_err(|_| AuthError::RandomUnavailable)?;
            let token_id = self.random_id()?;
            let secret = self.random_secret()?;
            let now = self.clock.now();
            let user = UserAccount {
                id: user_id,
                email: request.email.clone(),
                display_name: request.display_name.clone(),
                password_hash: None,
                disabled_at: None,
                created_at: now,
            };
            let membership = OrganizationMembership {
                organization_id: context.organization_id,
                user_id,
                role: request.role,
                created_at: now,
                created_by: context.user_id,
            };
            let setup = SetupToken {
                id: token_id,
                digest: digest(secret.expose()),
                purpose: SetupPurpose::PasswordSetup,
                user_id: Some(user_id),
                created_at: now,
                expires_at: add_duration(now, self.config.setup_token_timeout)?,
                consumed_at: None,
            };
            match self
                .call(self.store.create_invited_user(user, membership, setup))
                .await
            {
                Ok(()) => {
                    self.audit(
                        context,
                        request.request_id,
                        AuditAction::MembershipCreated,
                        "user",
                        user_id.get().to_string(),
                        role_metadata(request.role),
                    )
                    .await?;
                    return Ok(secret);
                }
                Err(AuthError::IdentityCollision) => {}
                Err(AuthError::AlreadyExists) => return Err(AuthError::AlreadyExists),
                Err(error) => return Err(error),
            }
        }
        Err(AuthError::CollisionExhausted)
    }

    pub async fn setup_password(
        &self,
        secret: &PlainSecret,
        password: PasswordInput,
        organization_id: OrganizationId,
        request_id: RequestCorrelationId,
    ) -> Result<(), AuthError> {
        let hash = self.passwords.hash(password).await?;
        let now = self.clock.now();
        let user_id = self
            .call(
                self.store
                    .consume_password_setup(digest(secret.expose()), now, hash),
            )
            .await
            .map_err(credential_error)?;
        self.call(self.store.revoke_user_sessions(user_id, now))
            .await?;
        let membership = self
            .call(self.store.load_membership(user_id, organization_id))
            .await?;
        let context = context_for_membership(
            Actor::Bootstrap,
            CredentialId::new(user_id.get()).map_err(|_| AuthError::Unavailable)?,
            &membership,
            PermissionSet::from_role(membership.role),
        );
        self.audit(
            &context,
            request_id,
            AuditAction::PasswordSetup,
            "user",
            user_id.get().to_string(),
            AuditMetadata::new([]).expect("empty audit metadata is valid"),
        )
        .await
    }

    pub async fn create_password_reset(
        &self,
        context: &AuthContext,
        target_user_id: UserId,
        request_id: RequestCorrelationId,
    ) -> Result<PlainSecret, AuthError> {
        require(context, Permission::OrganizationAdmin)?;
        let target_membership = self
            .call(
                self.store
                    .load_membership(target_user_id, context.organization_id),
            )
            .await?;
        if target_membership.role == OrganizationRole::Owner {
            require(context, Permission::OrganizationOwner)?;
        }
        for _ in 0..self.config.identity_collision_retries {
            let secret = self.random_secret()?;
            let now = self.clock.now();
            let token = SetupToken {
                id: self.random_id()?,
                digest: digest(secret.expose()),
                purpose: SetupPurpose::PasswordReset,
                user_id: Some(target_user_id),
                created_at: now,
                expires_at: add_duration(now, self.config.setup_token_timeout)?,
                consumed_at: None,
            };
            match self
                .call(self.store.create_password_setup_token(token))
                .await
            {
                Ok(()) => {
                    self.audit(
                        context,
                        request_id,
                        AuditAction::PasswordReset,
                        "user",
                        target_user_id.get().to_string(),
                        AuditMetadata::new([]).expect("empty audit metadata is valid"),
                    )
                    .await?;
                    return Ok(secret);
                }
                Err(AuthError::IdentityCollision) => {}
                Err(error) => return Err(error),
            }
        }
        Err(AuthError::CollisionExhausted)
    }

    pub async fn change_password(
        &self,
        context: &AuthContext,
        current: PasswordInput,
        replacement: PasswordInput,
        request_id: RequestCorrelationId,
    ) -> Result<(), AuthError> {
        if context.actor != Actor::WebSession {
            return Err(AuthError::Forbidden);
        }
        let (user, membership) = self
            .authoritative_identity(context.user_id, context.organization_id)
            .await?;
        if user.disabled_at.is_some() || membership.role != context.role {
            return Err(AuthError::InvalidCredential);
        }
        let Some(encoded) = user.password_hash else {
            return Err(AuthError::InvalidCredentials);
        };
        if !self.passwords.verify(current, encoded).await? {
            return Err(AuthError::InvalidCredentials);
        }
        let now = self.clock.now();
        let replacement = self.passwords.hash(replacement).await?;
        self.call(
            self.store
                .update_password_hash(context.user_id, replacement, now),
        )
        .await?;
        self.call(self.store.revoke_user_sessions(context.user_id, now))
            .await?;
        self.audit(
            context,
            request_id,
            AuditAction::PasswordChanged,
            "user",
            context.user_id.get().to_string(),
            AuditMetadata::new([]).expect("empty audit metadata is valid"),
        )
        .await
    }

    pub async fn create_api_token(
        &self,
        context: &AuthContext,
        request: CreateApiTokenRequest,
    ) -> Result<IssuedApiToken, AuthError> {
        if context.actor == Actor::Bootstrap
            || !request.scopes.is_subset_of(context.permissions)
            || request.expires_at <= self.clock.now()
            || duration_between(self.clock.now(), request.expires_at)
                > self.config.max_api_token_lifetime
        {
            return Err(AuthError::InvalidTokenPolicy);
        }
        for _ in 0..self.config.identity_collision_retries {
            let id = self.random_id()?;
            let secret = self.random_secret()?;
            let now = self.clock.now();
            let token = ApiToken {
                id,
                digest: digest(secret.expose()),
                user_id: context.user_id,
                organization_id: context.organization_id,
                name: request.name.clone(),
                scopes: request.scopes,
                created_at: now,
                expires_at: request.expires_at,
                last_used_at: None,
                revoked_at: None,
            };
            match self.call(self.store.create_api_token(token)).await {
                Ok(()) => {
                    self.audit(
                        context,
                        request.request_id,
                        AuditAction::ApiTokenCreated,
                        "api_token",
                        id.get().to_string(),
                        AuditMetadata::new([]).expect("empty audit metadata is valid"),
                    )
                    .await?;
                    return Ok(IssuedApiToken {
                        id,
                        secret,
                        expires_at: request.expires_at,
                    });
                }
                Err(AuthError::IdentityCollision) => {}
                Err(error) => return Err(error),
            }
        }
        Err(AuthError::CollisionExhausted)
    }

    pub async fn authenticate_api_token(
        &self,
        secret: &PlainSecret,
    ) -> Result<AuthContext, AuthError> {
        let now = self.clock.now();
        let token = self
            .call(self.store.load_api_token(digest(secret.expose())))
            .await
            .map_err(credential_error)?;
        if token.revoked_at.is_some() || now >= token.expires_at {
            return Err(AuthError::InvalidCredential);
        }
        let (user, membership) = self
            .authoritative_identity(token.user_id, token.organization_id)
            .await?;
        if user.disabled_at.is_some() {
            return Err(AuthError::InvalidCredential);
        }
        if token
            .last_used_at
            .is_none_or(|last| elapsed_at_least(last, now, self.config.activity_touch_interval))
        {
            let _ = self.call(self.store.touch_api_token(token.id, now)).await;
        }
        Ok(context_for_membership(
            Actor::PersonalApiToken,
            token.id,
            &membership,
            PermissionSet::from_role(membership.role).intersect(token.scopes),
        ))
    }

    pub async fn revoke_api_token(
        &self,
        context: &AuthContext,
        token_id: CredentialId,
        request_id: RequestCorrelationId,
    ) -> Result<(), AuthError> {
        self.call(self.store.revoke_api_token(
            token_id,
            context.user_id,
            context.organization_id,
            self.clock.now(),
        ))
        .await?;
        self.audit(
            context,
            request_id,
            AuditAction::ApiTokenRevoked,
            "api_token",
            token_id.get().to_string(),
            AuditMetadata::new([]).expect("empty audit metadata is valid"),
        )
        .await
    }

    pub async fn list_api_tokens(
        &self,
        context: &AuthContext,
        limit: usize,
    ) -> Result<Vec<ApiTokenView>, AuthError> {
        if !(1..=100).contains(&limit) {
            return Err(AuthError::InvalidTokenPolicy);
        }
        self.call(
            self.store
                .list_api_tokens(context.user_id, context.organization_id, limit),
        )
        .await
    }

    pub async fn organization(
        &self,
        context: &AuthContext,
    ) -> Result<OrganizationIdentity, AuthError> {
        self.call(self.store.load_organization(context.organization_id))
            .await
    }

    pub async fn list_organization_members(
        &self,
        context: &AuthContext,
        limit: usize,
    ) -> Result<Vec<OrganizationMemberView>, AuthError> {
        require(context, Permission::OrganizationAdmin)?;
        if !(1..=100).contains(&limit) {
            return Err(AuthError::InvalidTokenPolicy);
        }
        self.call(
            self.store
                .list_organization_members(context.organization_id, limit),
        )
        .await
    }

    pub async fn list_audit_log(
        &self,
        context: &AuthContext,
        limit: usize,
    ) -> Result<Vec<AuditLogView>, AuthError> {
        require(context, Permission::OrganizationAdmin)?;
        if !(1..=100).contains(&limit) {
            return Err(AuthError::InvalidTokenPolicy);
        }
        self.call(self.store.list_audit_log(context.organization_id, limit))
            .await
    }

    pub async fn record_project_audit(
        &self,
        context: &AuthContext,
        request_id: RequestCorrelationId,
        action: AuditAction,
        target_kind: &'static str,
        target_id: String,
    ) -> Result<(), AuthError> {
        if !matches!(
            action,
            AuditAction::ProjectCreated
                | AuditAction::ProjectKeyCreated
                | AuditAction::ProjectKeyDisabled
                | AuditAction::ProjectPolicyChanged
                | AuditAction::ProjectDeletionRequested
                | AuditAction::ProjectDeletionCancelled
        ) || !matches!(target_kind, "project" | "project_key" | "project_deletion")
        {
            return Err(AuthError::Forbidden);
        }
        self.audit(
            context,
            request_id,
            action,
            target_kind,
            target_id,
            AuditMetadata::new([]).expect("empty audit metadata is valid"),
        )
        .await
    }

    pub async fn record_incident_capsule_audit(
        &self,
        context: &AuthContext,
        request_id: RequestCorrelationId,
        project_id: ProjectId,
        issue_id: faultkeep_domain::grouping::IssueId,
        selected_event_count: usize,
        result_size_class: &'static str,
    ) -> Result<(), AuthError> {
        if !context.permissions.contains(Permission::IncidentExport)
            || selected_event_count > 10
            || !matches!(result_size_class, "small" | "medium" | "large")
        {
            return Err(AuthError::Forbidden);
        }
        let metadata = AuditMetadata::new([
            (
                AuditMetadataKey::ProjectId,
                AuditMetadataValue::new(project_id.get().to_string())
                    .map_err(|_| AuthError::Forbidden)?,
            ),
            (
                AuditMetadataKey::SelectedEventCount,
                AuditMetadataValue::new(selected_event_count.to_string())
                    .map_err(|_| AuthError::Forbidden)?,
            ),
            (
                AuditMetadataKey::ResultSizeClass,
                AuditMetadataValue::new(result_size_class).map_err(|_| AuthError::Forbidden)?,
            ),
        ])
        .map_err(|_| AuthError::Forbidden)?;
        self.audit(
            context,
            request_id,
            AuditAction::IncidentCapsuleExported,
            "incident_capsule",
            issue_id.to_string(),
            metadata,
        )
        .await
    }

    pub async fn record_notification_audit(
        &self,
        context: &AuthContext,
        request_id: RequestCorrelationId,
        project_id: ProjectId,
        action: AuditAction,
        target_id: String,
    ) -> Result<(), AuthError> {
        let target_kind = match action {
            AuditAction::NotificationDestinationUpserted => "notification_destination",
            AuditAction::AlertRuleUpserted => "alert_rule",
            _ => return Err(AuthError::Forbidden),
        };
        if !context.permissions.contains(Permission::ProjectAdmin) {
            return Err(AuthError::Forbidden);
        }
        let metadata = AuditMetadata::new([(
            AuditMetadataKey::ProjectId,
            AuditMetadataValue::new(project_id.get().to_string())
                .map_err(|_| AuthError::Forbidden)?,
        )])
        .map_err(|_| AuthError::Forbidden)?;
        self.audit(
            context,
            request_id,
            action,
            target_kind,
            target_id,
            metadata,
        )
        .await
    }

    pub async fn validate_issue_assignee(
        &self,
        context: &AuthContext,
        assignee: ActorRef,
    ) -> Result<(), AuthError> {
        if assignee.kind() != ActorKind::User || assignee.id()[..8] != [0; 8] {
            return Err(AuthError::Forbidden);
        }
        let user_id = UserId::new(u64::from_be_bytes(
            assignee.id()[8..]
                .try_into()
                .map_err(|_| AuthError::Forbidden)?,
        ))
        .map_err(|_| AuthError::Forbidden)?;
        let (user, _) = self
            .authoritative_identity(user_id, context.organization_id)
            .await?;
        if user.disabled_at.is_some() {
            return Err(AuthError::Forbidden);
        }
        Ok(())
    }

    pub async fn mutate_membership(
        &self,
        context: &AuthContext,
        target_user_id: UserId,
        kind: MembershipMutationKind,
        request_id: RequestCorrelationId,
    ) -> Result<(), AuthError> {
        require(context, Permission::OrganizationAdmin)?;
        let touches_owner = matches!(
            kind,
            MembershipMutationKind::Create(OrganizationRole::Owner)
                | MembershipMutationKind::ChangeRole(OrganizationRole::Owner)
        ) || self
            .call(
                self.store
                    .load_membership(target_user_id, context.organization_id),
            )
            .await
            .is_ok_and(|membership| membership.role == OrganizationRole::Owner);
        if touches_owner {
            require(context, Permission::OrganizationOwner)?;
        }
        let operation_id = self.random_id()?;
        self.call(self.store.mutate_membership(MembershipMutation {
            organization_id: context.organization_id,
            user_id: target_user_id,
            actor_user_id: context.user_id,
            operation_id,
            kind,
            timestamp: self.clock.now(),
        }))
        .await?;
        let (action, metadata) = match kind {
            MembershipMutationKind::Create(role) => {
                (AuditAction::MembershipCreated, role_metadata(role))
            }
            MembershipMutationKind::ChangeRole(role) => {
                (AuditAction::MembershipRoleChanged, role_metadata(role))
            }
            MembershipMutationKind::Remove => (
                AuditAction::MembershipRemoved,
                AuditMetadata::new([]).expect("empty audit metadata is valid"),
            ),
        };
        self.audit(
            context,
            request_id,
            action,
            "user",
            target_user_id.get().to_string(),
            metadata,
        )
        .await
    }

    pub async fn set_user_disabled(
        &self,
        context: &AuthContext,
        target_user_id: UserId,
        disabled: bool,
        request_id: RequestCorrelationId,
    ) -> Result<(), AuthError> {
        require(context, Permission::OrganizationAdmin)?;
        if self
            .call(
                self.store
                    .load_membership(target_user_id, context.organization_id),
            )
            .await?
            .role
            == OrganizationRole::Owner
        {
            require(context, Permission::OrganizationOwner)?;
        }
        let now = self.clock.now();
        self.call(self.store.set_user_disabled(
            target_user_id,
            disabled.then_some(now),
            self.random_id()?,
        ))
        .await?;
        if disabled {
            self.call(self.store.revoke_user_sessions(target_user_id, now))
                .await?;
        }
        self.audit(
            context,
            request_id,
            if disabled {
                AuditAction::UserDisabled
            } else {
                AuditAction::UserEnabled
            },
            "user",
            target_user_id.get().to_string(),
            AuditMetadata::new([]).expect("empty audit metadata is valid"),
        )
        .await
    }

    pub async fn authorize_project(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        permission: Permission,
    ) -> Result<(), AuthError> {
        let organization_id = self
            .call(self.store.project_organization(project_id))
            .await?;
        if organization_id != context.organization_id || !context.permissions.contains(permission) {
            return Err(AuthError::Forbidden);
        }
        Ok(())
    }

    async fn create_session(
        &self,
        user_id: UserId,
        now: Timestamp,
    ) -> Result<(WebSession, IssuedWebSession), AuthError> {
        for _ in 0..self.config.identity_collision_retries {
            let id = self.random_id()?;
            let secret = self.random_secret()?;
            let csrf = self.random_secret()?;
            let absolute_expires_at = add_duration(now, self.config.session_absolute_timeout)?;
            let session = WebSession {
                id,
                digest: digest(secret.expose()),
                csrf_digest: digest(csrf.expose()),
                user_id,
                created_at: now,
                last_seen_at: now,
                idle_expires_at: add_duration(now, self.config.session_idle_timeout)?
                    .min(absolute_expires_at),
                absolute_expires_at,
                revoked_at: None,
            };
            match self.call(self.store.create_session(session.clone())).await {
                Ok(()) => {
                    return Ok((
                        session,
                        IssuedWebSession {
                            session: secret,
                            csrf,
                            absolute_expires_at,
                        },
                    ));
                }
                Err(AuthError::IdentityCollision) => {}
                Err(error) => return Err(error),
            }
        }
        Err(AuthError::CollisionExhausted)
    }

    async fn authoritative_identity(
        &self,
        user_id: UserId,
        organization_id: OrganizationId,
    ) -> Result<(UserAccount, OrganizationMembership), AuthError> {
        let user = self
            .call(self.store.load_user(user_id))
            .await
            .map_err(credential_error)?;
        let membership = self
            .call(self.store.load_membership(user_id, organization_id))
            .await
            .map_err(credential_error)?;
        Ok((user, membership))
    }

    async fn audit(
        &self,
        context: &AuthContext,
        request_id: RequestCorrelationId,
        action: AuditAction,
        target_kind: &'static str,
        target_id: String,
        metadata: AuditMetadata,
    ) -> Result<(), AuthError> {
        let target_id = AuditTargetId::new(target_id).map_err(|_| AuthError::Unavailable)?;
        self.call(self.store.append_audit(AuditRecord {
            request_id,
            organization_id: context.organization_id,
            actor: context.actor,
            actor_user_id: context.user_id,
            action,
            target_kind,
            target_id,
            timestamp: self.clock.now(),
            metadata,
        }))
        .await
    }

    async fn call<T>(
        &self,
        future: faultkeep_ports::PortFuture<'_, Result<T, AuthStoreError>>,
    ) -> Result<T, AuthError> {
        timeout(self.config.store_timeout, future)
            .await
            .map_err(|_| AuthError::Unavailable)?
            .map_err(map_store_error)
    }

    fn random_secret(&self) -> Result<PlainSecret, AuthError> {
        let mut bytes = [0_u8; 32];
        self.random
            .fill_bytes(&mut bytes)
            .map_err(|_| AuthError::RandomUnavailable)?;
        Ok(PlainSecret::new(bytes))
    }

    fn random_u63(&self) -> Result<u64, AuthError> {
        let mut bytes = [0_u8; 8];
        self.random
            .fill_bytes(&mut bytes)
            .map_err(|_| AuthError::RandomUnavailable)?;
        Ok((u64::from_be_bytes(bytes) & i64::MAX as u64).max(1))
    }

    fn random_id(&self) -> Result<CredentialId, AuthError> {
        CredentialId::new(self.random_u63()?).map_err(|_| AuthError::RandomUnavailable)
    }
}

struct PasswordEngine {
    config: PasswordConfig,
    semaphore: Arc<Semaphore>,
    dummy_hash: PasswordHash,
    random: Arc<dyn RandomSource>,
}

impl PasswordEngine {
    fn new(config: PasswordConfig, random: Arc<dyn RandomSource>) -> Result<Self, AuthError> {
        let params = argon_params(config)?;
        let salt = SaltString::encode_b64(&[0x42; 16]).map_err(|_| AuthError::Unavailable)?;
        let encoded = Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password(b"faultkeep-dummy-password", &salt)
            .map_err(|_| AuthError::Unavailable)?
            .to_string();
        Ok(Self {
            config,
            semaphore: Arc::new(Semaphore::new(config.max_concurrency)),
            dummy_hash: PasswordHash::new(encoded).map_err(|_| AuthError::Unavailable)?,
            random,
        })
    }

    fn dummy_hash(&self) -> &PasswordHash {
        &self.dummy_hash
    }

    fn needs_upgrade(&self, encoded: &PasswordHash) -> bool {
        let expected = format!(
            "$argon2id$v=19$m={},t={},p={}$",
            self.config.memory_kib, self.config.iterations, self.config.parallelism
        );
        !encoded.expose().starts_with(&expected)
    }

    async fn hash(&self, password: PasswordInput) -> Result<PasswordHash, AuthError> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        let config = self.config;
        let mut salt_bytes = [0_u8; 16];
        self.random
            .fill_bytes(&mut salt_bytes)
            .map_err(|_| AuthError::RandomUnavailable)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| AuthError::Unavailable)?;
            let hash = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params(config)?)
                .hash_password(password.0.as_bytes(), &salt)
                .map_err(|_| AuthError::Unavailable)?
                .to_string();
            PasswordHash::new(hash).map_err(|_| AuthError::Unavailable)
        })
        .await
        .map_err(|_| AuthError::Unavailable)?
    }

    async fn verify(
        &self,
        password: PasswordInput,
        encoded: PasswordHash,
    ) -> Result<bool, AuthError> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let parsed =
                ParsedPasswordHash::new(encoded.expose()).map_err(|_| AuthError::Unavailable)?;
            Ok(Argon2::default()
                .verify_password(password.0.as_bytes(), &parsed)
                .is_ok())
        })
        .await
        .map_err(|_| AuthError::Unavailable)?
    }
}

fn argon_params(config: PasswordConfig) -> Result<Params, AuthError> {
    Params::new(
        config.memory_kib,
        config.iterations,
        config.parallelism,
        None,
    )
    .map_err(|_| AuthError::InvalidConfiguration)
}

pub struct LoginRateLimiter {
    config: LoginRateLimitConfig,
    state: Mutex<RateState>,
}

impl LoginRateLimiter {
    pub fn new(config: LoginRateLimitConfig) -> Result<Self, AuthError> {
        if config.max_attempts == 0 || config.window.is_zero() || config.capacity < 2 {
            return Err(AuthError::InvalidConfiguration);
        }
        Ok(Self {
            config,
            state: Mutex::new(RateState::new(config.capacity)),
        })
    }

    #[must_use]
    pub fn check(&self, account: SecretDigest, network: SecretDigest, now: Timestamp) -> bool {
        let mut state = self.state.lock().expect("login limiter lock poisoned");
        let account_allowed = state.increment(account, now, self.config);
        let network_allowed = state.increment(network, now, self.config);
        account_allowed && network_allowed
    }

    pub fn success(&self, account: SecretDigest) {
        self.state
            .lock()
            .expect("login limiter lock poisoned")
            .entries
            .remove(&account);
    }

    #[must_use]
    pub fn entries(&self) -> usize {
        self.state
            .lock()
            .expect("login limiter lock poisoned")
            .entries
            .len()
    }
}

#[derive(Clone, Copy)]
struct RateEntry {
    attempts: u32,
    window_end: i64,
    generation: u64,
}

struct RateState {
    entries: HashMap<SecretDigest, RateEntry>,
    order: VecDeque<(SecretDigest, u64)>,
    generation: u64,
    capacity: usize,
}

impl RateState {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.min(4096)),
            order: VecDeque::with_capacity(capacity.min(4096)),
            generation: 1,
            capacity,
        }
    }

    fn increment(
        &mut self,
        key: SecretDigest,
        now: Timestamp,
        config: LoginRateLimitConfig,
    ) -> bool {
        let now = now.unix_millis();
        let window_millis = i64::try_from(config.window.as_millis()).unwrap_or(i64::MAX);
        let generation = self.generation;
        self.generation = self.generation.wrapping_add(1).max(1);
        let entry = self.entries.entry(key).or_insert(RateEntry {
            attempts: 0,
            window_end: now.saturating_add(window_millis),
            generation,
        });
        if now >= entry.window_end {
            entry.attempts = 0;
            entry.window_end = now.saturating_add(window_millis);
        }
        entry.attempts = entry.attempts.saturating_add(1);
        entry.generation = generation;
        let allowed = entry.attempts <= config.max_attempts;
        self.order.push_back((key, generation));
        while self.entries.len() > self.capacity {
            self.evict_oldest();
        }
        while self.order.len() > self.capacity.saturating_mul(2) {
            self.evict_oldest();
        }
        allowed
    }

    fn evict_oldest(&mut self) {
        if let Some((key, generation)) = self.order.pop_front()
            && self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.generation == generation)
        {
            self.entries.remove(&key);
        }
    }
}

fn validate_config(config: AuthConfig) -> Result<(), AuthError> {
    let password = config.password;
    let valid = config.identity_collision_retries > 0
        && !config.session_idle_timeout.is_zero()
        && config.session_idle_timeout <= config.session_absolute_timeout
        && !config.activity_touch_interval.is_zero()
        && config.activity_touch_interval < config.session_idle_timeout
        && !config.setup_token_timeout.is_zero()
        && !config.max_api_token_lifetime.is_zero()
        && !config.store_timeout.is_zero()
        && password.memory_kib >= MIN_ARGON2_MEMORY_KIB
        && password.memory_kib <= MAX_ARGON2_MEMORY_KIB
        && password.iterations >= MIN_ARGON2_ITERATIONS
        && password.iterations <= MAX_ARGON2_ITERATIONS
        && password.parallelism > 0
        && password.parallelism <= MAX_ARGON2_PARALLELISM
        && password.max_concurrency > 0
        && password.max_concurrency <= 64;
    if !valid {
        return Err(AuthError::InvalidConfiguration);
    }
    LoginRateLimiter::new(config.login_rate_limit)?;
    Ok(())
}

fn require(context: &AuthContext, permission: Permission) -> Result<(), AuthError> {
    context
        .permissions
        .contains(permission)
        .then_some(())
        .ok_or(AuthError::Forbidden)
}

fn context_for_membership(
    actor: Actor,
    credential_id: CredentialId,
    membership: &OrganizationMembership,
    permissions: PermissionSet,
) -> AuthContext {
    AuthContext {
        actor,
        user_id: membership.user_id,
        organization_id: membership.organization_id,
        role: membership.role,
        permissions,
        credential_id,
    }
}

fn role_metadata(role: OrganizationRole) -> AuditMetadata {
    AuditMetadata::new([(
        AuditMetadataKey::Role,
        AuditMetadataValue::new(role.name()).expect("role name is bounded"),
    )])
    .expect("role metadata is valid")
}

fn map_store_error(error: AuthStoreError) -> AuthError {
    match error {
        AuthStoreError::NotFound => AuthError::NotFound,
        AuthStoreError::AlreadyExists => AuthError::AlreadyExists,
        AuthStoreError::IdentityCollision => AuthError::IdentityCollision,
        AuthStoreError::BootstrapClosed => AuthError::BootstrapClosed,
        AuthStoreError::FinalOwner => AuthError::FinalOwner,
        AuthStoreError::InvalidCredential => AuthError::InvalidCredential,
        AuthStoreError::InvalidData | AuthStoreError::Unavailable => AuthError::Unavailable,
    }
}

fn credential_error(error: AuthError) -> AuthError {
    match error {
        AuthError::NotFound | AuthError::InvalidCredential => AuthError::InvalidCredential,
        other => other,
    }
}

fn digest(bytes: &[u8]) -> SecretDigest {
    SecretDigest::new(Sha256::digest(bytes).into())
}

fn account_rate_key(email: &str) -> SecretDigest {
    digest(email.to_ascii_lowercase().as_bytes())
}

fn constant_digest_eq(left: SecretDigest, right: SecretDigest) -> bool {
    left.expose()
        .iter()
        .zip(right.expose())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn add_duration(timestamp: Timestamp, duration: Duration) -> Result<Timestamp, AuthError> {
    let millis =
        i64::try_from(duration.as_millis()).map_err(|_| AuthError::InvalidConfiguration)?;
    Timestamp::from_unix_millis(timestamp.unix_millis().saturating_add(millis))
        .map_err(|_| AuthError::InvalidConfiguration)
}

fn duration_between(start: Timestamp, end: Timestamp) -> Duration {
    Duration::from_millis(
        u64::try_from(end.unix_millis().saturating_sub(start.unix_millis())).unwrap_or(0),
    )
}

fn elapsed_at_least(start: Timestamp, end: Timestamp, duration: Duration) -> bool {
    duration_between(start, end) >= duration
}

fn derived_u63(digest: &SecretDigest, offset: usize) -> u64 {
    let bytes = digest.expose();
    let value = u64::from_be_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("digest segment has eight bytes"),
    ) & i64::MAX as u64;
    value.max(1)
}

fn derived_id(digest: &SecretDigest, offset: usize) -> Result<CredentialId, AuthError> {
    CredentialId::new(derived_u63(digest, offset)).map_err(|_| AuthError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use faultkeep_domain::{
        BoundedId, ProjectAcceptanceState,
        auth::{AuthValueError, SetupToken},
    };
    use faultkeep_ports::{PortFuture, RandomError};
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

    struct TestClock(AtomicI64);

    impl Clock for TestClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_unix_millis(self.0.load(Ordering::Relaxed)).unwrap()
        }
    }

    struct TestRandom(AtomicU64);

    impl RandomSource for TestRandom {
        fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
            let value = self.0.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            for (index, byte) in output.iter_mut().enumerate() {
                *byte = value.wrapping_add(index as u64) as u8;
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        state: Mutex<MemoryState>,
    }

    #[derive(Default)]
    struct MemoryState {
        bootstrap: Option<SetupToken>,
        users: HashMap<UserId, UserAccount>,
        emails: HashMap<String, UserId>,
        memberships: HashMap<(UserId, OrganizationId), OrganizationMembership>,
        sessions: HashMap<SecretDigest, WebSession>,
        tokens: HashMap<SecretDigest, ApiToken>,
        audits: Vec<AuditRecord>,
        project_organizations: HashMap<ProjectId, OrganizationId>,
    }

    impl AuthStore for MemoryStore {
        fn install_bootstrap_token(
            &self,
            token: SetupToken,
        ) -> PortFuture<'_, Result<BootstrapTokenInstall, AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                if !state.users.is_empty() {
                    return Ok(BootstrapTokenInstall::Closed);
                }
                if state.bootstrap.is_some() {
                    return Ok(BootstrapTokenInstall::AlreadyInstalled);
                }
                state.bootstrap = Some(token);
                Ok(BootstrapTokenInstall::Created)
            })
        }

        fn consume_bootstrap(
            &self,
            identity: BootstrapIdentity,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                let token = state
                    .bootstrap
                    .as_mut()
                    .ok_or(AuthStoreError::InvalidCredential)?;
                if token.digest != identity.token_digest
                    || token.consumed_at.is_some()
                    || identity.timestamp >= token.expires_at
                {
                    return Err(AuthStoreError::InvalidCredential);
                }
                token.consumed_at = Some(identity.timestamp);
                state
                    .emails
                    .insert(identity.user.email.canonical().to_owned(), identity.user.id);
                state.users.insert(identity.user.id, identity.user);
                state.memberships.insert(
                    (
                        identity.membership.user_id,
                        identity.membership.organization_id,
                    ),
                    identity.membership,
                );
                Ok(())
            })
        }

        fn create_invited_user(
            &self,
            user: UserAccount,
            membership: OrganizationMembership,
            setup_token: SetupToken,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                if state.emails.contains_key(user.email.canonical()) {
                    return Err(AuthStoreError::AlreadyExists);
                }
                if state.users.contains_key(&user.id) {
                    return Err(AuthStoreError::IdentityCollision);
                }
                state
                    .emails
                    .insert(user.email.canonical().to_owned(), user.id);
                state.users.insert(user.id, user);
                state
                    .memberships
                    .insert((membership.user_id, membership.organization_id), membership);
                state.bootstrap = Some(setup_token);
                Ok(())
            })
        }

        fn create_password_setup_token(
            &self,
            token: SetupToken,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                let user_id = token.user_id.ok_or(AuthStoreError::InvalidData)?;
                let mut state = self.state.lock().unwrap();
                if !state.users.contains_key(&user_id) {
                    return Err(AuthStoreError::NotFound);
                }
                state.bootstrap = Some(token);
                Ok(())
            })
        }

        fn consume_password_setup(
            &self,
            digest: SecretDigest,
            now: Timestamp,
            password_hash: PasswordHash,
        ) -> PortFuture<'_, Result<UserId, AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                let token = state
                    .bootstrap
                    .as_mut()
                    .filter(|token| token.digest == digest && now < token.expires_at)
                    .ok_or(AuthStoreError::InvalidCredential)?;
                let user_id = token.user_id.ok_or(AuthStoreError::InvalidData)?;
                token.consumed_at = Some(now);
                state
                    .users
                    .get_mut(&user_id)
                    .ok_or(AuthStoreError::NotFound)?
                    .password_hash = Some(password_hash);
                Ok(user_id)
            })
        }

        fn load_user_by_email<'a>(
            &'a self,
            email: &'a EmailAddress,
        ) -> PortFuture<'a, Result<UserAccount, AuthStoreError>> {
            let email = email.canonical().to_owned();
            Box::pin(async move {
                let state = self.state.lock().unwrap();
                let id = state.emails.get(&email).ok_or(AuthStoreError::NotFound)?;
                state.users.get(id).cloned().ok_or(AuthStoreError::NotFound)
            })
        }

        fn load_user(
            &self,
            user_id: UserId,
        ) -> PortFuture<'_, Result<UserAccount, AuthStoreError>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .unwrap()
                    .users
                    .get(&user_id)
                    .cloned()
                    .ok_or(AuthStoreError::NotFound)
            })
        }

        fn update_password_hash(
            &self,
            user_id: UserId,
            password_hash: PasswordHash,
            _changed_at: Timestamp,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .unwrap()
                    .users
                    .get_mut(&user_id)
                    .ok_or(AuthStoreError::NotFound)?
                    .password_hash = Some(password_hash);
                Ok(())
            })
        }

        fn load_membership(
            &self,
            user_id: UserId,
            organization_id: OrganizationId,
        ) -> PortFuture<'_, Result<OrganizationMembership, AuthStoreError>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .unwrap()
                    .memberships
                    .get(&(user_id, organization_id))
                    .cloned()
                    .ok_or(AuthStoreError::NotFound)
            })
        }

        fn mutate_membership(
            &self,
            mutation: MembershipMutation,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                let key = (mutation.user_id, mutation.organization_id);
                let owner_count = state
                    .memberships
                    .values()
                    .filter(|membership| {
                        membership.organization_id == mutation.organization_id
                            && membership.role == OrganizationRole::Owner
                    })
                    .count();
                let was_owner = state
                    .memberships
                    .get(&key)
                    .is_some_and(|membership| membership.role == OrganizationRole::Owner);
                if was_owner
                    && owner_count == 1
                    && !matches!(
                        mutation.kind,
                        MembershipMutationKind::ChangeRole(OrganizationRole::Owner)
                    )
                {
                    return Err(AuthStoreError::FinalOwner);
                }
                match mutation.kind {
                    MembershipMutationKind::Create(role) => {
                        if state.memberships.contains_key(&key) {
                            return Err(AuthStoreError::AlreadyExists);
                        }
                        state.memberships.insert(
                            key,
                            OrganizationMembership {
                                organization_id: mutation.organization_id,
                                user_id: mutation.user_id,
                                role,
                                created_at: mutation.timestamp,
                                created_by: mutation.actor_user_id,
                            },
                        );
                    }
                    MembershipMutationKind::ChangeRole(role) => {
                        state
                            .memberships
                            .get_mut(&key)
                            .ok_or(AuthStoreError::NotFound)?
                            .role = role;
                    }
                    MembershipMutationKind::Remove => {
                        state
                            .memberships
                            .remove(&key)
                            .ok_or(AuthStoreError::NotFound)?;
                    }
                }
                Ok(())
            })
        }

        fn set_user_disabled(
            &self,
            user_id: UserId,
            disabled_at: Option<Timestamp>,
            _operation_id: CredentialId,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                if disabled_at.is_some() {
                    let final_owner = state.memberships.values().any(|membership| {
                        membership.user_id == user_id
                            && membership.role == OrganizationRole::Owner
                            && state
                                .memberships
                                .values()
                                .filter(|other| {
                                    other.organization_id == membership.organization_id
                                        && other.role == OrganizationRole::Owner
                                })
                                .count()
                                == 1
                    });
                    if final_owner {
                        return Err(AuthStoreError::FinalOwner);
                    }
                }
                state
                    .users
                    .get_mut(&user_id)
                    .ok_or(AuthStoreError::NotFound)?
                    .disabled_at = disabled_at;
                Ok(())
            })
        }

        fn create_session(
            &self,
            session: WebSession,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                if state.sessions.insert(session.digest, session).is_some() {
                    return Err(AuthStoreError::IdentityCollision);
                }
                Ok(())
            })
        }

        fn load_session(
            &self,
            digest: SecretDigest,
        ) -> PortFuture<'_, Result<WebSession, AuthStoreError>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .unwrap()
                    .sessions
                    .get(&digest)
                    .cloned()
                    .ok_or(AuthStoreError::NotFound)
            })
        }

        fn touch_session(
            &self,
            session_id: CredentialId,
            last_seen_at: Timestamp,
            idle_expires_at: Timestamp,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                let session = state
                    .sessions
                    .values_mut()
                    .find(|session| session.id == session_id)
                    .ok_or(AuthStoreError::NotFound)?;
                session.last_seen_at = last_seen_at;
                session.idle_expires_at = idle_expires_at;
                Ok(())
            })
        }

        fn revoke_session(
            &self,
            digest: SecretDigest,
            revoked_at: Timestamp,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .unwrap()
                    .sessions
                    .get_mut(&digest)
                    .ok_or(AuthStoreError::NotFound)?
                    .revoked_at = Some(revoked_at);
                Ok(())
            })
        }

        fn revoke_user_sessions(
            &self,
            user_id: UserId,
            revoked_at: Timestamp,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                for session in self
                    .state
                    .lock()
                    .unwrap()
                    .sessions
                    .values_mut()
                    .filter(|session| session.user_id == user_id)
                {
                    session.revoked_at = Some(revoked_at);
                }
                Ok(())
            })
        }

        fn create_api_token(&self, token: ApiToken) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                if state.tokens.insert(token.digest, token).is_some() {
                    return Err(AuthStoreError::IdentityCollision);
                }
                Ok(())
            })
        }

        fn load_api_token(
            &self,
            digest: SecretDigest,
        ) -> PortFuture<'_, Result<ApiToken, AuthStoreError>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .unwrap()
                    .tokens
                    .get(&digest)
                    .cloned()
                    .ok_or(AuthStoreError::NotFound)
            })
        }

        fn touch_api_token(
            &self,
            token_id: CredentialId,
            last_used_at: Timestamp,
        ) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .unwrap()
                    .tokens
                    .values_mut()
                    .find(|token| token.id == token_id)
                    .ok_or(AuthStoreError::NotFound)?
                    .last_used_at = Some(last_used_at);
                Ok(())
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
                self.state
                    .lock()
                    .unwrap()
                    .tokens
                    .values_mut()
                    .find(|token| {
                        token.id == token_id
                            && token.user_id == user_id
                            && token.organization_id == organization_id
                    })
                    .ok_or(AuthStoreError::NotFound)?
                    .revoked_at = Some(revoked_at);
                Ok(())
            })
        }

        fn project_organization(
            &self,
            project_id: ProjectId,
        ) -> PortFuture<'_, Result<OrganizationId, AuthStoreError>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .unwrap()
                    .project_organizations
                    .get(&project_id)
                    .copied()
                    .ok_or(AuthStoreError::NotFound)
            })
        }

        fn append_audit(&self, record: AuditRecord) -> PortFuture<'_, Result<(), AuthStoreError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                if state
                    .audits
                    .iter()
                    .any(|existing| existing.request_id == record.request_id)
                {
                    return Ok(());
                }
                state.audits.push(record);
                Ok(())
            })
        }
    }

    fn service() -> (IdentityService, Arc<MemoryStore>, Arc<TestClock>) {
        let store = Arc::new(MemoryStore::default());
        let clock = Arc::new(TestClock(AtomicI64::new(1_750_000_000_000)));
        let service = IdentityService::new(
            store.clone(),
            clock.clone(),
            Arc::new(TestRandom(AtomicU64::new(0))),
            AuthConfig {
                password: PasswordConfig {
                    memory_kib: MIN_ARGON2_MEMORY_KIB,
                    iterations: MIN_ARGON2_ITERATIONS,
                    parallelism: 1,
                    max_concurrency: 1,
                },
                ..AuthConfig::default()
            },
        )
        .unwrap();
        (service, store, clock)
    }

    async fn bootstrapped() -> (
        IdentityService,
        Arc<MemoryStore>,
        Arc<TestClock>,
        AuthContext,
        IssuedWebSession,
    ) {
        let (service, store, clock) = service();
        let setup = service.ensure_bootstrap_token().await.unwrap().unwrap();
        let context = service
            .bootstrap(BootstrapRequest {
                setup_secret: setup,
                email: EmailAddress::parse("owner@example.com").unwrap(),
                user_display_name: UserDisplayName::new("Owner").unwrap(),
                password: PasswordInput::new("correct horse battery staple").unwrap(),
                organization_slug: Slug::new("acme").unwrap(),
                organization_name: DisplayName::new("Acme").unwrap(),
                request_id: BoundedId::new("bootstrap-1").unwrap(),
            })
            .await
            .unwrap();
        let login = service
            .login(LoginRequest {
                email: "owner@example.com".into(),
                password: "correct horse battery staple".into(),
                organization_id: context.organization_id,
                client_network_digest: SecretDigest::new([9; 32]),
                request_id: BoundedId::new("login-1").unwrap(),
            })
            .await
            .unwrap();
        (service, store, clock, context, login)
    }

    #[tokio::test]
    async fn bootstrap_login_session_csrf_rotation_and_generic_failure() {
        let (service, _store, clock, context, session) = bootstrapped().await;
        assert_eq!(
            service
                .login(LoginRequest {
                    email: "missing@example.com".into(),
                    password: "not the password".into(),
                    organization_id: context.organization_id,
                    client_network_digest: SecretDigest::new([8; 32]),
                    request_id: BoundedId::new("login-bad").unwrap(),
                })
                .await,
            Err(AuthError::InvalidCredentials)
        );
        assert_eq!(
            service
                .authenticate_session(&session.session, None, true, context.organization_id)
                .await,
            Err(AuthError::InvalidCredential)
        );
        let authenticated = service
            .authenticate_session(
                &session.session,
                Some(&session.csrf),
                true,
                context.organization_id,
            )
            .await
            .unwrap();
        assert_eq!(context.user_id, authenticated.user_id);
        assert!(
            authenticated
                .permissions
                .contains(Permission::OrganizationOwner)
        );
        let rotated = service
            .login(LoginRequest {
                email: "owner@example.com".into(),
                password: "correct horse battery staple".into(),
                organization_id: context.organization_id,
                client_network_digest: SecretDigest::new([7; 32]),
                request_id: BoundedId::new("login-rotated").unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(
            service
                .authenticate_session(&session.session, None, false, context.organization_id,)
                .await,
            Err(AuthError::InvalidCredential)
        );
        clock
            .0
            .store(rotated.absolute_expires_at.unix_millis(), Ordering::Relaxed);
        assert_eq!(
            service
                .authenticate_session(&rotated.session, None, false, context.organization_id,)
                .await,
            Err(AuthError::InvalidCredential)
        );
        clock.0.store(1_750_000_000_000, Ordering::Relaxed);
        service.logout(&rotated.session).await.unwrap();
        assert_eq!(
            service
                .authenticate_session(&rotated.session, None, false, context.organization_id,)
                .await,
            Err(AuthError::InvalidCredential)
        );
    }

    #[tokio::test]
    async fn final_owner_disabled_user_and_scope_intersection_fail_closed() {
        let (service, store, _clock, owner, _session) = bootstrapped().await;
        assert_eq!(
            service
                .set_user_disabled(
                    &owner,
                    owner.user_id,
                    true,
                    BoundedId::new("disable-final").unwrap(),
                )
                .await,
            Err(AuthError::FinalOwner)
        );
        assert_eq!(
            service
                .mutate_membership(
                    &owner,
                    owner.user_id,
                    MembershipMutationKind::ChangeRole(OrganizationRole::Admin),
                    BoundedId::new("demote-final").unwrap(),
                )
                .await,
            Err(AuthError::FinalOwner)
        );
        let token_secret = PlainSecret::new([77; 32]);
        let token_id = CredentialId::new(77).unwrap();
        store.state.lock().unwrap().tokens.insert(
            digest(token_secret.expose()),
            ApiToken {
                id: token_id,
                digest: digest(token_secret.expose()),
                user_id: owner.user_id,
                organization_id: owner.organization_id,
                name: TokenName::new("read").unwrap(),
                scopes: PermissionSet::from_permissions([
                    Permission::IssueRead,
                    Permission::OrganizationDelete,
                ]),
                created_at: Timestamp::from_unix_millis(1_750_000_000_000).unwrap(),
                expires_at: Timestamp::from_unix_millis(1_760_000_000_000).unwrap(),
                last_used_at: None,
                revoked_at: None,
            },
        );
        store
            .state
            .lock()
            .unwrap()
            .memberships
            .get_mut(&(owner.user_id, owner.organization_id))
            .unwrap()
            .role = OrganizationRole::Viewer;
        let token_context = service.authenticate_api_token(&token_secret).await.unwrap();
        assert!(token_context.permissions.contains(Permission::IssueRead));
        assert!(
            !token_context
                .permissions
                .contains(Permission::OrganizationDelete)
        );
        assert_eq!(
            service
                .create_api_token(
                    &token_context,
                    CreateApiTokenRequest {
                        name: TokenName::new("escalation").unwrap(),
                        scopes: PermissionSet::from_permissions([Permission::IssueWrite]),
                        expires_at: Timestamp::from_unix_millis(1_751_000_000_000).unwrap(),
                        request_id: BoundedId::new("token-escalation").unwrap(),
                    },
                )
                .await,
            Err(AuthError::InvalidTokenPolicy)
        );
        assert_eq!(
            service
                .validate_issue_assignee(&token_context, ActorRef::system())
                .await,
            Err(AuthError::Forbidden)
        );
        let own_project = ProjectId::new(41).unwrap();
        let foreign_project = ProjectId::new(42).unwrap();
        let foreign_organization = OrganizationId::new(999).unwrap();
        {
            let mut state = store.state.lock().unwrap();
            state
                .project_organizations
                .insert(own_project, owner.organization_id);
            state
                .project_organizations
                .insert(foreign_project, foreign_organization);
        }
        assert!(
            service
                .authorize_project(&token_context, own_project, Permission::IssueRead)
                .await
                .is_ok()
        );
        assert_eq!(
            service
                .authorize_project(&token_context, own_project, Permission::IssueWrite)
                .await,
            Err(AuthError::Forbidden)
        );
        assert_eq!(
            service
                .authorize_project(&token_context, foreign_project, Permission::IssueRead)
                .await,
            Err(AuthError::Forbidden)
        );
    }

    #[test]
    fn login_limiter_is_bounded_and_recovers_after_window() {
        let limiter = LoginRateLimiter::new(LoginRateLimitConfig {
            max_attempts: 2,
            window: Duration::from_secs(10),
            capacity: 4,
        })
        .unwrap();
        let now = Timestamp::from_unix_millis(0).unwrap();
        assert!(limiter.check(SecretDigest::new([1; 32]), SecretDigest::new([2; 32]), now));
        assert!(limiter.check(SecretDigest::new([1; 32]), SecretDigest::new([2; 32]), now));
        assert!(!limiter.check(SecretDigest::new([1; 32]), SecretDigest::new([2; 32]), now));
        for byte in 3..20 {
            let _ = limiter.check(
                SecretDigest::new([byte; 32]),
                SecretDigest::new([byte.wrapping_add(1); 32]),
                now,
            );
        }
        assert!(limiter.entries() <= 4);
        assert!(limiter.check(
            SecretDigest::new([1; 32]),
            SecretDigest::new([2; 32]),
            Timestamp::from_unix_millis(11_000).unwrap()
        ));
    }

    #[test]
    fn configuration_and_redaction_are_fail_closed() {
        assert_eq!(PasswordInput::new("short"), Err(AuthError::InvalidPassword));
        assert_eq!(
            format!("{:?}", PasswordInput::new("long enough password").unwrap()),
            "PasswordInput(<redacted>)"
        );
        assert_eq!(
            IdentityService::new(
                Arc::new(MemoryStore::default()),
                Arc::new(TestClock(AtomicI64::new(0))),
                Arc::new(TestRandom(AtomicU64::new(0))),
                AuthConfig {
                    password: PasswordConfig {
                        memory_kib: 1,
                        ..PasswordConfig::default()
                    },
                    ..AuthConfig::default()
                }
            )
            .err(),
            Some(AuthError::InvalidConfiguration)
        );
        let _ = ProjectAcceptanceState::Active;
        let _ = AuthValueError::UnknownScope;
    }

    #[test]
    #[ignore = "Phase 11 login rate-limit baseline runs in release mode"]
    fn performance_login_rate_limit_rps() {
        let limiter = LoginRateLimiter::new(LoginRateLimitConfig::default()).unwrap();
        let account = SecretDigest::new([1; 32]);
        let network = SecretDigest::new([2; 32]);
        let now = Timestamp::from_unix_millis(1_750_000_000_000).unwrap();
        for _ in 0..10 {
            let _ = limiter.check(account, network, now);
        }
        let iterations = 500_000_u64;
        let started = std::time::Instant::now();
        let mut rejected = 0_u64;
        for _ in 0..iterations {
            rejected += u64::from(!limiter.check(account, network, now));
        }
        let elapsed = started.elapsed();
        let rps = iterations as f64 / elapsed.as_secs_f64();
        eprintln!(
            "Auth Phase 11: rate_limit_rps={:.0},attempts={},rejected={},capacity={},elapsed_ms={}",
            rps,
            iterations,
            rejected,
            limiter.config.capacity,
            elapsed.as_millis()
        );
        assert_eq!(rejected, iterations);
        assert!(
            rps >= 100_000.0,
            "rate-limit baseline {rps:.0} RPS is below gate"
        );
    }
}
