//! Bounded identity, credential, authorization, and audit values.

use std::{collections::BTreeSet, fmt, num::NonZeroU64};

use thiserror::Error;

use crate::{BoundedId, OrganizationId, Slug, Timestamp};

pub const MAX_AUDIT_METADATA: usize = 12;

pub type UserDisplayName = BoundedId<128>;
pub type TokenName = BoundedId<64>;
pub type RequestCorrelationId = BoundedId<64>;
pub type AuditTargetId = BoundedId<128>;
pub type AuditMetadataValue = BoundedId<128>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthValueError {
    #[error("identifier must be a positive u63 value")]
    InvalidIdentifier,
    #[error("email address is invalid or exceeds 254 bytes")]
    InvalidEmail,
    #[error("permission scope is unknown")]
    UnknownScope,
    #[error("audit metadata is duplicated or exceeds its bound")]
    InvalidAuditMetadata,
}

macro_rules! u63_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(NonZeroU64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, AuthValueError> {
                NonZeroU64::new(value)
                    .filter(|value| value.get() <= i64::MAX as u64)
                    .map(Self)
                    .ok_or(AuthValueError::InvalidIdentifier)
            }

            #[must_use]
            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }
    };
}

u63_id!(UserId);
u63_id!(CredentialId);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EmailAddress {
    display: Box<str>,
    canonical: Box<str>,
}

impl EmailAddress {
    pub fn parse(value: impl Into<Box<str>>) -> Result<Self, AuthValueError> {
        let display = value.into();
        let valid = !display.is_empty()
            && display.len() <= 254
            && display.is_ascii()
            && !display
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace());
        if !valid {
            return Err(AuthValueError::InvalidEmail);
        }
        let Some((local, domain)) = display.rsplit_once('@') else {
            return Err(AuthValueError::InvalidEmail);
        };
        if local.is_empty()
            || domain.is_empty()
            || local.len() > 64
            || domain.starts_with('.')
            || domain.ends_with('.')
            || !domain.contains('.')
        {
            return Err(AuthValueError::InvalidEmail);
        }
        let canonical = display.to_ascii_lowercase().into_boxed_str();
        Ok(Self { display, canonical })
    }

    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    #[must_use]
    pub fn canonical(&self) -> &str {
        &self.canonical
    }
}

impl fmt::Debug for EmailAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EmailAddress(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PasswordHash(Box<str>);

impl PasswordHash {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, AuthValueError> {
        let value = value.into();
        if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err(AuthValueError::InvalidIdentifier);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PasswordHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PasswordHash(<redacted>)")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SecretDigest([u8; 32]);

impl SecretDigest {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn expose(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for SecretDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretDigest(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PlainSecret([u8; 32]);

impl PlainSecret {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn encode_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for PlainSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlainSecret(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrganizationRole {
    Owner,
    Admin,
    Member,
    Viewer,
}

impl OrganizationRole {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Viewer => "viewer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Permission {
    EventRead,
    IssueRead,
    IssueWrite,
    ProjectRead,
    ProjectAdmin,
    DebugFileRead,
    DebugFileWrite,
    DebugFileDelete,
    ArtifactRead,
    ArtifactWrite,
    ArtifactDelete,
    OrganizationAdmin,
    OrganizationOwner,
    OrganizationDelete,
}

impl Permission {
    pub const ALL: [Self; 14] = [
        Self::EventRead,
        Self::IssueRead,
        Self::IssueWrite,
        Self::ProjectRead,
        Self::ProjectAdmin,
        Self::DebugFileRead,
        Self::DebugFileWrite,
        Self::DebugFileDelete,
        Self::ArtifactRead,
        Self::ArtifactWrite,
        Self::ArtifactDelete,
        Self::OrganizationAdmin,
        Self::OrganizationOwner,
        Self::OrganizationDelete,
    ];

    #[must_use]
    pub const fn scope(self) -> &'static str {
        match self {
            Self::EventRead => "event:read",
            Self::IssueRead => "issue:read",
            Self::IssueWrite => "issue:write",
            Self::ProjectRead => "project:read",
            Self::ProjectAdmin => "project:admin",
            Self::DebugFileRead => "debug_file:read",
            Self::DebugFileWrite => "debug_file:write",
            Self::DebugFileDelete => "debug_file:delete",
            Self::ArtifactRead => "artifact:read",
            Self::ArtifactWrite => "artifact:write",
            Self::ArtifactDelete => "artifact:delete",
            Self::OrganizationAdmin => "organization:admin",
            Self::OrganizationOwner => "organization:owner",
            Self::OrganizationDelete => "organization:delete",
        }
    }

    pub fn parse_scope(value: &str) -> Result<Self, AuthValueError> {
        Self::ALL
            .into_iter()
            .find(|permission| permission.scope() == value)
            .ok_or(AuthValueError::UnknownScope)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PermissionSet(u16);

impl PermissionSet {
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    pub fn from_role(role: OrganizationRole) -> Self {
        let mut set = Self::empty();
        let viewer = [
            Permission::EventRead,
            Permission::IssueRead,
            Permission::ProjectRead,
            Permission::DebugFileRead,
            Permission::ArtifactRead,
        ];
        for permission in viewer {
            set.insert(permission);
        }
        if matches!(
            role,
            OrganizationRole::Member | OrganizationRole::Admin | OrganizationRole::Owner
        ) {
            set.insert(Permission::IssueWrite);
        }
        if matches!(role, OrganizationRole::Admin | OrganizationRole::Owner) {
            for permission in [
                Permission::ProjectAdmin,
                Permission::DebugFileWrite,
                Permission::DebugFileDelete,
                Permission::ArtifactWrite,
                Permission::ArtifactDelete,
                Permission::OrganizationAdmin,
            ] {
                set.insert(permission);
            }
        }
        if role == OrganizationRole::Owner {
            set.insert(Permission::OrganizationOwner);
            set.insert(Permission::OrganizationDelete);
        }
        set
    }

    #[must_use]
    pub fn from_permissions(permissions: impl IntoIterator<Item = Permission>) -> Self {
        let mut set = Self::empty();
        for permission in permissions {
            set.insert(permission);
        }
        set
    }

    pub fn insert(&mut self, permission: Permission) {
        self.0 |= 1_u16 << permission as u8;
    }

    #[must_use]
    pub const fn contains(self, permission: Permission) -> bool {
        self.0 & (1_u16 << permission as u8) != 0
    }

    #[must_use]
    pub const fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }

    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub fn iter(self) -> impl Iterator<Item = Permission> {
        Permission::ALL
            .into_iter()
            .filter(move |permission| self.contains(*permission))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    WebSession,
    PersonalApiToken,
    Bootstrap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub actor: Actor,
    pub user_id: UserId,
    pub organization_id: OrganizationId,
    pub role: OrganizationRole,
    pub permissions: PermissionSet,
    pub credential_id: CredentialId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAccount {
    pub id: UserId,
    pub email: EmailAddress,
    pub display_name: UserDisplayName,
    pub password_hash: Option<PasswordHash>,
    pub disabled_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationMembership {
    pub organization_id: OrganizationId,
    pub user_id: UserId,
    pub role: OrganizationRole,
    pub created_at: Timestamp,
    pub created_by: UserId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSession {
    pub id: CredentialId,
    pub digest: SecretDigest,
    pub csrf_digest: SecretDigest,
    pub user_id: UserId,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub idle_expires_at: Timestamp,
    pub absolute_expires_at: Timestamp,
    pub revoked_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiToken {
    pub id: CredentialId,
    pub digest: SecretDigest,
    pub user_id: UserId,
    pub organization_id: OrganizationId,
    pub name: TokenName,
    pub scopes: PermissionSet,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupPurpose {
    Bootstrap,
    PasswordSetup,
    PasswordReset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupToken {
    pub id: CredentialId,
    pub digest: SecretDigest,
    pub purpose: SetupPurpose,
    pub user_id: Option<UserId>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub consumed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    LoginSucceeded,
    PasswordSetup,
    PasswordReset,
    PasswordChanged,
    MembershipCreated,
    MembershipRemoved,
    MembershipRoleChanged,
    UserDisabled,
    UserEnabled,
    ApiTokenCreated,
    ApiTokenRevoked,
    ProjectCreated,
    ProjectKeyCreated,
    ProjectKeyDisabled,
    ProjectPolicyChanged,
    ProjectDeletionRequested,
    ProjectDeletionCancelled,
}

impl AuditAction {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::LoginSucceeded => "auth.login_succeeded",
            Self::PasswordSetup => "auth.password_setup",
            Self::PasswordReset => "auth.password_reset",
            Self::PasswordChanged => "auth.password_changed",
            Self::MembershipCreated => "membership.created",
            Self::MembershipRemoved => "membership.removed",
            Self::MembershipRoleChanged => "membership.role_changed",
            Self::UserDisabled => "user.disabled",
            Self::UserEnabled => "user.enabled",
            Self::ApiTokenCreated => "api_token.created",
            Self::ApiTokenRevoked => "api_token.revoked",
            Self::ProjectCreated => "project.created",
            Self::ProjectKeyCreated => "project_key.created",
            Self::ProjectKeyDisabled => "project_key.disabled",
            Self::ProjectPolicyChanged => "project.policy_changed",
            Self::ProjectDeletionRequested => "project.deletion_requested",
            Self::ProjectDeletionCancelled => "project.deletion_cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuditMetadataKey {
    Role,
    CredentialKind,
    Outcome,
}

impl AuditMetadataKey {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Role => "role",
            Self::CredentialKind => "credential_kind",
            Self::Outcome => "outcome",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditMetadata(Vec<(AuditMetadataKey, AuditMetadataValue)>);

impl AuditMetadata {
    pub fn new(
        values: impl IntoIterator<Item = (AuditMetadataKey, AuditMetadataValue)>,
    ) -> Result<Self, AuthValueError> {
        let values = values.into_iter().collect::<Vec<_>>();
        let unique = values.iter().map(|(key, _)| *key).collect::<BTreeSet<_>>();
        if values.len() > MAX_AUDIT_METADATA || unique.len() != values.len() {
            return Err(AuthValueError::InvalidAuditMetadata);
        }
        Ok(Self(values))
    }

    #[must_use]
    pub fn values(&self) -> &[(AuditMetadataKey, AuditMetadataValue)] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    pub request_id: RequestCorrelationId,
    pub organization_id: OrganizationId,
    pub actor: Actor,
    pub actor_user_id: UserId,
    pub action: AuditAction,
    pub target_kind: &'static str,
    pub target_id: AuditTargetId,
    pub timestamp: Timestamp,
    pub metadata: AuditMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipMutationKind {
    Create(OrganizationRole),
    ChangeRole(OrganizationRole),
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipMutation {
    pub organization_id: OrganizationId,
    pub user_id: UserId,
    pub actor_user_id: UserId,
    pub operation_id: CredentialId,
    pub kind: MembershipMutationKind,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapIdentity {
    pub operation_id: CredentialId,
    pub token_digest: SecretDigest,
    pub organization_id: OrganizationId,
    pub organization_slug: Slug,
    pub organization_name: crate::DisplayName,
    pub user: UserAccount,
    pub membership: OrganizationMembership,
    pub timestamp: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_expansion_and_scope_intersection_are_deny_by_default() {
        let viewer = PermissionSet::from_role(OrganizationRole::Viewer);
        assert!(viewer.contains(Permission::IssueRead));
        assert!(!viewer.contains(Permission::IssueWrite));
        let admin = PermissionSet::from_role(OrganizationRole::Admin);
        assert!(admin.contains(Permission::OrganizationAdmin));
        assert!(!admin.contains(Permission::OrganizationOwner));
        let owner = PermissionSet::from_role(OrganizationRole::Owner);
        assert!(owner.contains(Permission::OrganizationDelete));
        assert_eq!(
            owner.intersect(PermissionSet::from_permissions([Permission::IssueRead])),
            PermissionSet::from_permissions([Permission::IssueRead])
        );
    }

    #[test]
    fn email_and_secret_debug_are_bounded_and_redacted() {
        let email = EmailAddress::parse("Owner@Example.COM").unwrap();
        assert_eq!(email.canonical(), "owner@example.com");
        assert_eq!(format!("{email:?}"), "EmailAddress(<redacted>)");
        assert_eq!(
            format!("{:?}", PlainSecret::new([7; 32])),
            "PlainSecret(<redacted>)"
        );
        assert_eq!(
            format!("{:?}", SecretDigest::new([7; 32])),
            "SecretDigest(<redacted>)"
        );
        assert!(EmailAddress::parse("bad email@example.com").is_err());
    }

    #[test]
    fn audit_metadata_uses_a_closed_bounded_allowlist() {
        let metadata = AuditMetadata::new([(
            AuditMetadataKey::Role,
            AuditMetadataValue::new("admin").unwrap(),
        )])
        .unwrap();
        assert_eq!(metadata.values()[0].0.name(), "role");
        assert!(
            AuditMetadata::new([
                (
                    AuditMetadataKey::Role,
                    AuditMetadataValue::new("owner").unwrap()
                ),
                (
                    AuditMetadataKey::Role,
                    AuditMetadataValue::new("admin").unwrap()
                ),
            ])
            .is_err()
        );
    }

    #[test]
    fn project_identifier_remains_separate_from_tenant_identity() {
        assert!(crate::ProjectId::new(1).is_ok());
        assert!(UserId::new(i64::MAX as u64).is_ok());
        assert!(CredentialId::new(0).is_err());
    }
}
