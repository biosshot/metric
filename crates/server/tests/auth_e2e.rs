use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use metric_application::auth::{
    AuthConfig, AuthError, BootstrapRequest, CreateApiTokenRequest, IdentityService,
    InviteUserRequest, LoginRequest, PasswordConfig, PasswordInput,
};
use metric_domain::{
    BoundedId, DisplayName, IpScrubPolicy, ItemCapabilities, OrganizationId, OrganizationIdentity,
    ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits, SecretBytes, Slug,
    Timestamp,
    auth::{
        EmailAddress, OrganizationRole, Permission, PermissionSet, SecretDigest, TokenName,
        UserDisplayName,
    },
};
use metric_mongo::MongoProjectStore;
use metric_ports::{Clock, ProjectStore, RandomError, RandomSource};
use metric_testkit::FixedClock;
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_identity_service_login_session_token_and_tenant_authorization() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let now = Timestamp::from_unix_millis(1_750_000_000_000)?;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
    let random: Arc<dyn RandomSource> = Arc::new(CounterRandom(AtomicU64::new(0)));
    let service = IdentityService::new(
        Arc::new(control.auth_store()),
        Arc::clone(&clock),
        random,
        AuthConfig {
            password: PasswordConfig {
                max_concurrency: 1,
                ..PasswordConfig::default()
            },
            store_timeout: Duration::from_secs(10),
            ..AuthConfig::default()
        },
    )?;

    let setup = service
        .ensure_bootstrap_token()
        .await?
        .expect("empty database emits one bootstrap token");
    let bootstrap = service
        .bootstrap(BootstrapRequest {
            setup_secret: setup,
            email: EmailAddress::parse("owner@example.com")?,
            user_display_name: UserDisplayName::new("Owner")?,
            password: PasswordInput::new("correct horse battery staple")?,
            organization_slug: Slug::new("acme")?,
            organization_name: DisplayName::new("Acme")?,
            request_id: BoundedId::new("bootstrap-e2e")?,
        })
        .await?;
    assert!(service.ensure_bootstrap_token().await.is_err());

    assert_eq!(
        service
            .login(LoginRequest {
                email: "owner@example.com".into(),
                password: "incorrect password".into(),
                organization_id: bootstrap.organization_id,
                client_network_digest: SecretDigest::new([90; 32]),
                request_id: BoundedId::new("bad-login-e2e")?,
            })
            .await,
        Err(AuthError::InvalidCredentials)
    );
    let session = service
        .login(LoginRequest {
            email: "OWNER@example.com".into(),
            password: "correct horse battery staple".into(),
            organization_id: bootstrap.organization_id,
            client_network_digest: SecretDigest::new([91; 32]),
            request_id: BoundedId::new("login-e2e")?,
        })
        .await?;
    let context = service
        .authenticate_session(
            &session.session,
            Some(&session.csrf),
            true,
            bootstrap.organization_id,
        )
        .await?;
    assert!(context.permissions.contains(Permission::OrganizationOwner));

    control
        .insert_project(project(
            100,
            bootstrap.organization_id,
            "authorized-project",
            now,
        ))
        .await?;
    let foreign_organization = OrganizationId::new(9_999)?;
    control
        .insert_organization(OrganizationIdentity {
            id: foreign_organization,
            slug: Slug::new("foreign")?,
            display_name: DisplayName::new("Foreign")?,
            created_at: now,
        })
        .await?;
    control
        .insert_project(project(101, foreign_organization, "foreign-project", now))
        .await?;
    service
        .authorize_project(&context, ProjectId::new(100)?, Permission::IssueRead)
        .await?;
    assert_eq!(
        service
            .authorize_project(&context, ProjectId::new(101)?, Permission::IssueRead)
            .await,
        Err(AuthError::Forbidden)
    );

    let token = service
        .create_api_token(
            &context,
            CreateApiTokenRequest {
                name: TokenName::new("automation")?,
                scopes: PermissionSet::from_permissions([
                    Permission::IssueRead,
                    Permission::IssueWrite,
                ]),
                expires_at: Timestamp::from_unix_millis(
                    now.unix_millis() + 30 * 24 * 60 * 60 * 1_000,
                )?,
                request_id: BoundedId::new("token-create-e2e")?,
            },
        )
        .await?;
    let token_context = service.authenticate_api_token(&token.secret).await?;
    assert!(token_context.permissions.contains(Permission::IssueRead));
    assert!(!token_context.permissions.contains(Permission::ProjectAdmin));
    service
        .revoke_api_token(&context, token.id, BoundedId::new("token-revoke-e2e")?)
        .await?;
    assert_eq!(
        service.authenticate_api_token(&token.secret).await,
        Err(AuthError::InvalidCredential)
    );

    let second_setup = service
        .invite_user(
            &context,
            InviteUserRequest {
                email: EmailAddress::parse("second@example.com")?,
                display_name: UserDisplayName::new("Second Owner")?,
                role: OrganizationRole::Owner,
                request_id: BoundedId::new("invite-e2e")?,
            },
        )
        .await?;
    service
        .setup_password(
            &second_setup,
            PasswordInput::new("another correct horse password")?,
            bootstrap.organization_id,
            BoundedId::new("setup-e2e")?,
        )
        .await?;
    service
        .set_user_disabled(
            &context,
            context.user_id,
            true,
            BoundedId::new("disable-e2e")?,
        )
        .await?;
    assert_eq!(
        service
            .authenticate_session(&session.session, None, false, bootstrap.organization_id,)
            .await,
        Err(AuthError::InvalidCredential)
    );

    let audits = database
        .collection::<mongodb::bson::Document>("audit_log")
        .count_documents(doc! {})
        .await?;
    assert!(audits >= 7);
    let leaked = database
        .collection::<mongodb::bson::Document>("audit_log")
        .find_one(doc! {
            "$or": [
                { "metadata.password": { "$exists": true } },
                { "metadata.token": { "$exists": true } },
                { "metadata.digest": { "$exists": true } },
            ]
        })
        .await?;
    assert!(leaked.is_none());
    Ok(())
}

fn project(
    id: i32,
    organization_id: OrganizationId,
    slug: &str,
    now: Timestamp,
) -> ProjectIdentity {
    ProjectIdentity {
        id: ProjectId::new(id).unwrap(),
        organization_id,
        slug: Slug::new(slug).unwrap(),
        display_name: DisplayName::new(slug).unwrap(),
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
    }
}

struct CounterRandom(AtomicU64);

impl RandomSource for CounterRandom {
    fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
        let sequence = self.0.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        for (index, byte) in output.iter_mut().enumerate() {
            *byte = sequence.wrapping_add(index as u64) as u8;
        }
        Ok(())
    }
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
        "metric_phase11_e2e_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
