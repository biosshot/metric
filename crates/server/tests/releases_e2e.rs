use std::{
    error::Error,
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use metric_application::{
    auth::{
        AuthConfig, BootstrapRequest, CreateApiTokenRequest, IdentityService, PasswordConfig,
        PasswordInput,
    },
    releases::ReleaseService,
};
use metric_domain::{
    BoundedId, DisplayName, IpScrubPolicy, ItemCapabilities, ProjectAcceptanceState, ProjectId,
    ProjectIdentity, ProjectIngestLimits, SecretBytes, Slug, Timestamp,
    auth::{
        Actor, AuthContext, EmailAddress, Permission, PermissionSet, TokenName, UserDisplayName,
    },
    finalization::derive_release_id,
};
use metric_mongo::{IssueCodecConfig, MongoProjectStore};
use metric_ports::{Clock, ProjectStore, RandomError, RandomSource, ReleaseStore};
use metric_server::release_http;
use metric_testkit::FixedClock;
use mongodb::{Client, Database, bson::doc};
use wait_timeout::ChildExt;

#[tokio::test]
#[ignore = "requires local MongoDB and globally installed sentry-cli"]
async fn real_sentry_cli_release_finalize_and_idempotent_deploy() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error + Send + Sync>> {
    let now = Timestamp::from_unix_millis(1_800_000_000_000)?;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
    let random: Arc<dyn RandomSource> = Arc::new(CounterRandom(AtomicU64::new(1)));
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let identity = Arc::new(IdentityService::new(
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
    )?);
    let setup = identity
        .ensure_bootstrap_token()
        .await?
        .expect("empty database emits a setup token");
    let bootstrap = identity
        .bootstrap(BootstrapRequest {
            setup_secret: setup,
            email: EmailAddress::parse("owner@example.com")?,
            user_display_name: UserDisplayName::new("Owner")?,
            password: PasswordInput::new("correct horse battery staple")?,
            organization_slug: Slug::new("acme")?,
            organization_name: DisplayName::new("Acme")?,
            request_id: BoundedId::new("phase29-bootstrap")?,
        })
        .await?;
    control
        .insert_project(ProjectIdentity {
            id: ProjectId::new(29)?,
            organization_id: bootstrap.organization_id,
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
                check_in: true,
            },
            limits: ProjectIngestLimits::default(),
            grouping_revision: 1,
            created_at: now,
        })
        .await?;
    let token = identity
        .create_api_token(
            &AuthContext {
                actor: Actor::WebSession,
                ..bootstrap.clone()
            },
            CreateApiTokenRequest {
                name: TokenName::new("sentry-cli-releases")?,
                scopes: PermissionSet::from_permissions([
                    Permission::ReleaseRead,
                    Permission::ReleaseWrite,
                ]),
                expires_at: Timestamp::from_unix_millis(now.unix_millis() + 86_400_000)?,
                request_id: BoundedId::new("phase29-token")?,
            },
        )
        .await?;
    let store: Arc<dyn ReleaseStore> = Arc::new(control.release_store(IssueCodecConfig::default()));
    let service = Arc::new(ReleaseService::new(Arc::clone(&store), Arc::clone(&clock)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = release_http::router(Some(Arc::clone(&identity)), Some(service));
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let url = format!("http://{address}/");
    let secret = token.secret.encode_hex();
    for arguments in [
        vec![
            "releases",
            "new",
            "--org",
            "acme",
            "--project",
            "backend",
            "backend@2.4.0",
        ],
        vec![
            "releases",
            "finalize",
            "--org",
            "acme",
            "--project",
            "backend",
            "--released",
            "1700000000",
            "backend@2.4.0",
        ],
        vec![
            "releases",
            "deploys",
            "new",
            "--org",
            "acme",
            "--project",
            "backend",
            "--release",
            "backend@2.4.0",
            "--env",
            "production",
            "--started",
            "1700000000",
            "--finished",
            "1700000060",
        ],
        vec![
            "releases",
            "deploys",
            "new",
            "--org",
            "acme",
            "--project",
            "backend",
            "--release",
            "backend@2.4.0",
            "--env",
            "production",
            "--started",
            "1700000000",
            "--finished",
            "1700000060",
        ],
    ] {
        let url = url.clone();
        let secret = secret.clone();
        let output =
            tokio::task::spawn_blocking(move || run_cli(&arguments, &url, &secret)).await??;
        assert!(
            output.status.success(),
            "sentry-cli failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    server.abort();
    let release_id = derive_release_id(bootstrap.organization_id, "backend@2.4.0");
    let release = store
        .load_release(bootstrap.organization_id, release_id)
        .await?;
    assert!(release.explicit);
    assert_eq!(
        release.released_at,
        Some(Timestamp::from_unix_millis(1_700_000_000_000)?)
    );
    let deploys = store
        .list_deploys(
            bootstrap.organization_id,
            ProjectId::new(29)?,
            release_id,
            None,
            10,
        )
        .await?;
    assert_eq!(deploys.len(), 1, "the retried deploy must be idempotent");
    assert_eq!(deploys[0].environment.as_ref(), "production");
    assert_eq!(
        deploys[0].finished_at,
        Some(Timestamp::from_unix_millis(1_700_000_060_000)?)
    );
    Ok(())
}

fn run_cli(
    arguments: &[&str],
    url: &str,
    secret: &str,
) -> Result<Output, Box<dyn Error + Send + Sync>> {
    let executable = if cfg!(windows) {
        "sentry-cli.cmd"
    } else {
        "sentry-cli"
    };
    let mut child = Command::new(executable)
        .args(arguments)
        .env("SENTRY_URL", url)
        .env("SENTRY_AUTH_TOKEN", secret)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if child.wait_timeout(Duration::from_secs(20))?.is_none() {
        child.kill()?;
    }
    Ok(child.wait_with_output()?)
}

async fn test_database() -> Result<Database, Box<dyn Error + Send + Sync>> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&retryWrites=false&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "metric_phase29_release_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}

struct CounterRandom(AtomicU64);

impl RandomSource for CounterRandom {
    fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
        let value = self.0.fetch_add(1, Ordering::Relaxed).to_be_bytes();
        for (index, byte) in output.iter_mut().enumerate() {
            *byte = value[index % value.len()];
        }
        Ok(())
    }
}
