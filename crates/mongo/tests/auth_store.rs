use std::error::Error;

use metric_domain::{
    DisplayName, IpScrubPolicy, ItemCapabilities, OrganizationId, ProjectAcceptanceState,
    ProjectId, ProjectIdentity, ProjectIngestLimits, SecretBytes, Slug, Timestamp,
    auth::{
        Actor, ApiToken, AuditAction, AuditMetadata, AuditRecord, AuditTargetId, BootstrapIdentity,
        CredentialId, EmailAddress, MembershipMutation, MembershipMutationKind,
        OrganizationMembership, OrganizationRole, PasswordHash, Permission, PermissionSet,
        RequestCorrelationId, SecretDigest, SetupPurpose, SetupToken, TokenName, UserAccount,
        UserDisplayName, UserId, WebSession,
    },
};
use metric_mongo::MongoProjectStore;
use metric_ports::{AuthStore, AuthStoreError, BootstrapTokenInstall, ProjectStore};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_auth_bootstrap_revocation_final_owner_and_scope() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let store = control.auth_store();
    let now = timestamp(1_750_000_000_000);
    let later = timestamp(1_750_000_060_000);
    let owner_id = UserId::new(11)?;
    let organization_id = OrganizationId::new(22)?;
    let setup = setup_token(1, [1; 32], SetupPurpose::Bootstrap, None, now);
    assert_eq!(
        store.install_bootstrap_token(setup.clone()).await?,
        BootstrapTokenInstall::Created
    );
    assert_eq!(
        store.install_bootstrap_token(setup).await?,
        BootstrapTokenInstall::AlreadyInstalled
    );

    let bootstrap = BootstrapIdentity {
        operation_id: CredentialId::new(2)?,
        token_digest: SecretDigest::new([1; 32]),
        organization_id,
        organization_slug: Slug::new("acme")?,
        organization_name: DisplayName::new("Acme")?,
        user: user(owner_id, "owner@example.com", now, true),
        membership: membership(
            organization_id,
            owner_id,
            OrganizationRole::Owner,
            owner_id,
            now,
        ),
        timestamp: now,
    };
    store.consume_bootstrap(bootstrap.clone()).await?;
    store
        .consume_bootstrap(bootstrap.clone())
        .await
        .expect("same guarded bootstrap operation is retryable");
    let mut conflicting = bootstrap;
    conflicting.organization_slug = Slug::new("different")?;
    assert_eq!(
        store.consume_bootstrap(conflicting).await,
        Err(AuthStoreError::InvalidCredential)
    );
    assert_eq!(
        store
            .install_bootstrap_token(setup_token(3, [3; 32], SetupPurpose::Bootstrap, None, now,))
            .await?,
        BootstrapTokenInstall::Closed
    );

    let loaded = store
        .load_user_by_email(&EmailAddress::parse("OWNER@example.com")?)
        .await?;
    assert_eq!(loaded.id, owner_id);
    assert_eq!(
        store.load_membership(owner_id, organization_id).await?.role,
        OrganizationRole::Owner
    );

    assert_eq!(
        store
            .mutate_membership(MembershipMutation {
                organization_id,
                user_id: owner_id,
                actor_user_id: owner_id,
                operation_id: CredentialId::new(4)?,
                kind: MembershipMutationKind::ChangeRole(OrganizationRole::Admin),
                timestamp: now,
            })
            .await,
        Err(AuthStoreError::FinalOwner)
    );
    assert_eq!(
        store
            .set_user_disabled(owner_id, Some(now), CredentialId::new(5)?)
            .await,
        Err(AuthStoreError::FinalOwner)
    );

    let second_owner = UserId::new(33)?;
    store
        .create_invited_user(
            user(second_owner, "second@example.com", now, false),
            membership(
                organization_id,
                second_owner,
                OrganizationRole::Owner,
                owner_id,
                now,
            ),
            setup_token(
                6,
                [6; 32],
                SetupPurpose::PasswordSetup,
                Some(second_owner),
                now,
            ),
        )
        .await?;
    store
        .create_password_setup_token(setup_token(
            66,
            [66; 32],
            SetupPurpose::PasswordSetup,
            Some(second_owner),
            now,
        ))
        .await?;
    assert_eq!(
        store
            .consume_password_setup(
                SecretDigest::new([6; 32]),
                later,
                PasswordHash::new("$argon2id$v=19$m=19456,t=2,p=1$old$hash")?,
            )
            .await,
        Err(AuthStoreError::InvalidCredential)
    );
    assert_eq!(
        store
            .consume_password_setup(
                SecretDigest::new([66; 32]),
                later,
                PasswordHash::new("$argon2id$v=19$m=19456,t=2,p=1$new$hash")?,
            )
            .await?,
        second_owner
    );
    store
        .mutate_membership(MembershipMutation {
            organization_id,
            user_id: owner_id,
            actor_user_id: second_owner,
            operation_id: CredentialId::new(7)?,
            kind: MembershipMutationKind::ChangeRole(OrganizationRole::Admin),
            timestamp: now,
        })
        .await?;
    let organization = store.load_organization(organization_id).await?;
    assert_eq!(organization.slug.as_str(), "acme");
    assert_eq!(organization.display_name.as_str(), "Acme");
    let members = store
        .list_organization_members(organization_id, 100)
        .await?;
    assert_eq!(members.len(), 2);
    assert!(
        members
            .iter()
            .any(|member| member.user_id == owner_id && member.role == OrganizationRole::Admin)
    );
    assert!(members.iter().any(|member| {
        member.user_id == second_owner && member.role == OrganizationRole::Owner
    }));
    store
        .append_audit(AuditRecord {
            request_id: RequestCorrelationId::new("request-organization-view")?,
            organization_id,
            actor: Actor::WebSession,
            actor_user_id: owner_id,
            action: AuditAction::MembershipRoleChanged,
            target_kind: "user",
            target_id: AuditTargetId::new(second_owner.get().to_string())?,
            timestamp: later,
            metadata: AuditMetadata::new([])?,
        })
        .await?;
    let audit = store.list_audit_log(organization_id, 100).await?;
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action.as_ref(), "membership.role_changed");
    assert_eq!(
        store
            .set_user_disabled(second_owner, Some(now), CredentialId::new(8)?)
            .await,
        Err(AuthStoreError::FinalOwner)
    );

    let session = WebSession {
        id: CredentialId::new(9)?,
        digest: SecretDigest::new([9; 32]),
        csrf_digest: SecretDigest::new([10; 32]),
        user_id: owner_id,
        created_at: now,
        last_seen_at: now,
        idle_expires_at: later,
        absolute_expires_at: timestamp(1_750_086_400_000),
        revoked_at: None,
    };
    store.create_session(session.clone()).await?;
    assert_eq!(
        store.load_session(session.digest).await?.csrf_digest,
        session.csrf_digest
    );
    store.revoke_session(session.digest, later).await?;
    assert_eq!(
        store.load_session(session.digest).await?.revoked_at,
        Some(later)
    );

    let token = ApiToken {
        id: CredentialId::new(10)?,
        digest: SecretDigest::new([11; 32]),
        user_id: owner_id,
        organization_id,
        name: TokenName::new("automation")?,
        scopes: PermissionSet::from_permissions([Permission::IssueRead, Permission::IssueWrite]),
        created_at: now,
        expires_at: timestamp(1_760_000_000_000),
        last_used_at: None,
        revoked_at: None,
    };
    store.create_api_token(token.clone()).await?;
    assert_eq!(
        store.load_api_token(token.digest).await?.scopes,
        token.scopes
    );
    store
        .revoke_api_token(token.id, owner_id, organization_id, later)
        .await?;
    assert_eq!(
        store.load_api_token(token.digest).await?.revoked_at,
        Some(later)
    );

    control
        .insert_project(ProjectIdentity {
            id: ProjectId::new(44)?,
            organization_id,
            slug: Slug::new("backend")?,
            display_name: DisplayName::new("Backend")?,
            state: ProjectAcceptanceState::Active,
            policy_revision: 1,
            ip_policy: IpScrubPolicy::Hmac,
            items: ItemCapabilities {
                error: true,
                client_report: true,
                log: true,
                transaction: true,
                span: true,
                feedback: true,
            },
            limits: ProjectIngestLimits::default(),
            grouping_revision: 1,
            created_at: now,
        })
        .await?;
    assert_eq!(
        store.project_organization(ProjectId::new(44)?).await?,
        organization_id
    );

    let invalid = database
        .collection::<mongodb::bson::Document>("api_tokens")
        .insert_one(doc! {
            "_id": 999_i64,
            "digest": "plaintext-must-not-be-stored",
        })
        .await;
    assert!(invalid.is_err());
    for (collection, expected) in [
        ("users", 2_usize),
        ("organization_memberships", 4),
        ("web_sessions", 4),
        ("api_tokens", 4),
        ("password_setup_tokens", 4),
        ("audit_log", 2),
    ] {
        assert_eq!(
            database
                .collection::<mongodb::bson::Document>(collection)
                .list_index_names()
                .await?
                .len(),
            expected,
            "{collection}"
        );
    }
    Ok(())
}

fn user(id: UserId, email: &str, now: Timestamp, password: bool) -> UserAccount {
    UserAccount {
        id,
        email: EmailAddress::parse(email).unwrap(),
        display_name: UserDisplayName::new("User").unwrap(),
        password_hash: password
            .then(|| PasswordHash::new("$argon2id$v=19$m=19456,t=2,p=1$abc$hash").unwrap()),
        disabled_at: None,
        created_at: now,
    }
}

fn membership(
    organization_id: OrganizationId,
    user_id: UserId,
    role: OrganizationRole,
    created_by: UserId,
    now: Timestamp,
) -> OrganizationMembership {
    OrganizationMembership {
        organization_id,
        user_id,
        role,
        created_at: now,
        created_by,
    }
}

fn setup_token(
    id: u64,
    digest: [u8; 32],
    purpose: SetupPurpose,
    user_id: Option<UserId>,
    now: Timestamp,
) -> SetupToken {
    SetupToken {
        id: CredentialId::new(id).unwrap(),
        digest: SecretDigest::new(digest),
        purpose,
        user_id,
        created_at: now,
        expires_at: timestamp(now.unix_millis() + 86_400_000),
        consumed_at: None,
    }
}

fn timestamp(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "metric_phase11_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
