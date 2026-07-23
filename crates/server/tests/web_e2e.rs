use std::{
    error::Error,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use faultkeep_application::{
    auth::{AuthConfig, BootstrapRequest, IdentityService, PasswordConfig, PasswordInput},
    native_api::NativeApiService,
    projects::{CreateProject, ProjectCacheConfig, ProjectService},
    search::{SearchConfig, SearchService},
    shutdown::ShutdownRoot,
};
use faultkeep_domain::{
    DisplayName, IpScrubPolicy, ItemCapabilities, ProjectIngestLimits, SecretBytes, Slug,
    Timestamp,
    auth::{EmailAddress, RequestCorrelationId, UserDisplayName},
};
use faultkeep_mongo::{EventCodecConfig, IssueCodecConfig, MongoProjectStore};
use faultkeep_ports::{Clock, InvestigationStore, RandomError, RandomSource};
use faultkeep_server::{http, native_http, web_http};
use mongodb::{Client, Database};
use tokio::net::TcpListener;

const EMAIL: &str = "phase13-owner@example.com";
const PASSWORD: &str = "correct horse battery staple";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires MongoDB 8.0.12, built web/dist, Node.js and Playwright Chromium"]
async fn infrastructure_browser_login_session_csrf_and_project_isolation() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    if !std::path::Path::new("web/dist/index.html").is_file() {
        return Err("web/dist is missing; run npm run build in web/".into());
    }
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(Timestamp::from_unix_millis(2_000)?));
    let random: Arc<dyn RandomSource> = Arc::new(CounterRandom(AtomicU64::new(0)));
    let identity = Arc::new(IdentityService::new(
        Arc::new(control.auth_store()),
        Arc::clone(&clock),
        Arc::clone(&random),
        AuthConfig {
            password: PasswordConfig {
                max_concurrency: 1,
                ..PasswordConfig::default()
            },
            store_timeout: Duration::from_secs(10),
            ..AuthConfig::default()
        },
    )?);
    let setup_token = identity
        .ensure_bootstrap_token()
        .await?
        .expect("empty test database has a bootstrap token");
    let owner = identity
        .bootstrap(BootstrapRequest {
            setup_secret: setup_token,
            email: EmailAddress::parse(EMAIL)?,
            user_display_name: UserDisplayName::new("Phase 13 Owner")?,
            password: PasswordInput::new(PASSWORD)?,
            organization_slug: Slug::new("phase13")?,
            organization_name: DisplayName::new("Phase 13")?,
            request_id: RequestCorrelationId::new("phase13-web-bootstrap")?,
        })
        .await?;
    let projects = Arc::new(ProjectService::new(
        Arc::new(control.clone()),
        Arc::clone(&clock),
        Arc::clone(&random),
        16,
        ProjectCacheConfig {
            capacity: 64,
            max_inflight: 16,
            positive_ttl: Duration::from_secs(60),
            negative_ttl: Duration::from_secs(5),
        },
    )?);
    let investigation: Arc<dyn InvestigationStore> = Arc::new(
        control.investigation_store(EventCodecConfig::default(), IssueCodecConfig::default()),
    );
    let search = Arc::new(SearchService::new(
        Arc::clone(&investigation),
        Arc::clone(&clock),
        SearchConfig::default(),
    )?);
    let native = Arc::new(NativeApiService::new(
        Arc::clone(&identity),
        Arc::clone(&projects),
        Arc::new(faultkeep_application::issues::IssueService::new(Arc::new(
            control.issue_store(IssueCodecConfig::default()),
        ))),
        investigation,
        search,
        Arc::clone(&clock),
    ));
    let created = native
        .create_project(
            &owner,
            CreateProject {
                organization_id: owner.organization_id,
                slug: Slug::new("backend")?,
                display_name: DisplayName::new("Backend")?,
                ip_policy: IpScrubPolicy::Hmac,
                items: ItemCapabilities {
                    error: true,
                    client_report: true,
                },
                limits: ProjectIngestLimits::default(),
            },
            RequestCorrelationId::new("phase13-web-project")?,
        )
        .await?;

    let root = ShutdownRoot::new();
    let app = http::router(
        root.signal(),
        faultkeep_application::observability::Metrics,
        native_http::router(Some(identity), Some(Arc::clone(&native)), false, true)
            .merge(web_http::router()),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(http::run(
        listener,
        root.signal(),
        Duration::from_secs(2),
        app,
    ));

    let script = std::env::current_dir()?.join("web/tests/e2e/real-session.mjs");
    let base_url = format!("http://{address}");
    let organization_id = owner.organization_id.get().to_string();
    let browser = tokio::task::spawn_blocking(move || {
        Command::new("node")
            .arg(script)
            .arg(base_url)
            .arg(EMAIL)
            .arg(PASSWORD)
            .arg(organization_id)
            .status()
    })
    .await??;
    if !browser.success() {
        return Err(format!("real browser script exited with {browser}").into());
    }

    let updated = native.project(&owner, created.project_id).await?;
    if updated.ip_policy != IpScrubPolicy::Remove {
        return Err("browser mutation did not reach the authoritative project service".into());
    }

    root.begin();
    server.await??;
    Ok(())
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("FAULTKEEP_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://faultkeep:faultkeep-local-only@127.0.0.1:27018/?authSource=admin&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(mongodb::bson::doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "faultkeep_phase13_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )))
}

struct FixedClock(Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
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
