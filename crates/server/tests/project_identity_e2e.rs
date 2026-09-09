use std::{error::Error, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use metric_application::{
    ingest::IngestService,
    observability::Metrics,
    projects::{ProjectCacheConfig, ProjectService},
    shutdown::ShutdownRoot,
};
use metric_domain::{
    DisplayName, DsnKey, IpScrubPolicy, ItemCapabilities, OrganizationId, OrganizationIdentity,
    ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits, ProjectKeyIdentity,
    ProjectKeyLabel, ProjectKeyState, SecretBytes, Slug, Timestamp,
};
use metric_mongo::MongoProjectStore;
use metric_ports::{ProjectResolver, ProjectStore};
use metric_server::{config::IngestConfig, http, ingest_http};
use metric_testkit::{FakeEventSink, FakeOutcomeSink, FixedClock, FixedRandom};
use mongodb::{Client, Database, bson::doc};
use tower::ServiceExt;

const KEY: DsnKey = DsnKey::from_bytes([4; 16]);
const EVENT: &str = include_str!("fixtures/error-event-v1.json");

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_http_real_project_resolver_to_fake_event_sink() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let store = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    store.bootstrap_or_validate().await?;
    seed(&store).await?;

    let root = ShutdownRoot::new();
    let projects = Arc::new(ProjectService::new(
        Arc::new(store),
        Arc::new(FixedClock(Timestamp::from_unix_millis(2_000)?)),
        Arc::new(FixedRandom(9)),
        8,
        ProjectCacheConfig {
            capacity: 64,
            max_inflight: 16,
            positive_ttl: Duration::from_secs(60),
            negative_ttl: Duration::from_secs(5),
        },
    )?);
    let sink = FakeEventSink::accepting();
    let resolver: Arc<dyn ProjectResolver> = projects.clone();
    let service = Arc::new(IngestService::new(
        resolver,
        Arc::new(sink.clone()),
        Arc::new(FakeOutcomeSink::default()),
        Arc::new(FixedClock(Timestamp::from_unix_millis(2_000)?)),
        Arc::new(FixedRandom(9)),
        16,
        root.signal(),
    ));
    let app = app(service, &root);

    let accepted = app.clone().oneshot(request(42)).await?;
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(sink.events().len(), 1);

    let mismatch = app.clone().oneshot(request(43)).await?;
    assert_eq!(mismatch.status(), StatusCode::UNAUTHORIZED);

    let old_key = DsnKey::from_bytes([8; 16]);
    let old_id = 4500000000000001_u64;
    let old_dsn = metric_domain::ExistingDsn::parse(&format!(
        "https://{old_key}@sentry.example:8443/{old_id}"
    ))?;
    let imported_request = |path: u64, dsn_id: u64| {
        Request::post(format!("/api/{path}/envelope/"))
            .body(Body::from(format!("{{\"dsn\":\"https://{old_key}@sentry.example/{dsn_id}\"}}\n{{\"type\":\"event\",\"length\":{}}}\n{}", EVENT.len(), EVENT))).unwrap()
    };
    // Populate both negative caches before importing.
    assert_eq!(
        app.clone()
            .oneshot(imported_request(old_id, old_id))
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let target = projects
        .import_project_key(
            ProjectId::new(42)?,
            ProjectKeyLabel::new("old clients")?,
            old_dsn,
        )
        .await?;
    for _ in 0..2 {
        assert_eq!(
            app.clone()
                .oneshot(imported_request(old_id, old_id))
                .await?
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        sink.events().last().unwrap().project_id,
        ProjectId::new(42)?
    );
    assert_eq!(
        app.clone()
            .oneshot(imported_request(42, old_id))
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(imported_request(old_id, 42))
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let store_request = Request::post(format!("/api/{old_id}/store/?sentry_key={old_key}"))
        .body(Body::from(EVENT))?;
    assert_eq!(
        app.clone().oneshot(store_request).await?.status(),
        StatusCode::OK
    );
    let listed = projects.list_project_keys(ProjectId::new(42)?).await?;
    assert!(
        listed
            .iter()
            .any(|key| key.key == target && key.existing_dsn.is_some())
    );
    projects
        .set_project_acceptance(ProjectId::new(42)?, ProjectAcceptanceState::Disabled)
        .await?;
    assert_eq!(
        app.clone()
            .oneshot(imported_request(old_id, old_id))
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    projects
        .set_project_acceptance(ProjectId::new(42)?, ProjectAcceptanceState::Active)
        .await?;
    assert_eq!(
        app.clone()
            .oneshot(imported_request(old_id, old_id))
            .await?
            .status(),
        StatusCode::OK
    );
    projects
        .set_project_key_state(ProjectId::new(42)?, target, ProjectKeyState::Disabled)
        .await?;
    assert_eq!(
        app.clone()
            .oneshot(imported_request(old_id, old_id))
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone().oneshot(request(42)).await?.status(),
        StatusCode::OK
    );

    projects
        .set_key_state(KEY, ProjectKeyState::Disabled)
        .await?;
    let disabled = app.oneshot(request(42)).await?;
    assert_eq!(disabled.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

fn app(service: Arc<IngestService>, root: &ShutdownRoot) -> Router {
    let config = IngestConfig {
        max_compressed_request_bytes: 20 * 1024 * 1024,
        max_decompressed_request_bytes: 100 * 1024 * 1024,
        max_event_bytes: 1024 * 1024,
        max_envelope_items: 100,
        max_active_requests: 128,
        max_parsing_tasks: 2,
        max_waiting_for_storage: 128,
        request_timeout: "10s".parse().unwrap(),
        unsupported_backoff_seconds: 3600,
        project_cache: Default::default(),
        batch: Default::default(),
        event_codec: Default::default(),
        backlog: Default::default(),
        attachments: Default::default(),
        replay: Default::default(),
    };
    http::router(
        root.signal(),
        Metrics,
        ingest_http::router(service, config, root.signal()),
    )
}

fn request(project_id: i32) -> Request<Body> {
    let envelope = format!(
        "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{}",
        EVENT.len(),
        EVENT
    );
    Request::builder()
        .method("POST")
        .uri(format!("/api/{project_id}/envelope/"))
        .header(
            "x-sentry-auth",
            format!("Sentry sentry_version=7,sentry_key={KEY}"),
        )
        .body(Body::from(envelope))
        .unwrap()
}

async fn seed(store: &MongoProjectStore) -> Result<(), Box<dyn Error>> {
    store
        .insert_organization(OrganizationIdentity {
            id: OrganizationId::new(1)?,
            slug: Slug::new("acme")?,
            display_name: DisplayName::new("Acme")?,
            created_at: Timestamp::from_unix_millis(1_000)?,
        })
        .await?;
    store
        .insert_project(ProjectIdentity {
            id: ProjectId::new(42)?,
            organization_id: OrganizationId::new(1)?,
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
                metric: true,
                replay: true,
            },
            limits: ProjectIngestLimits::default(),
            grouping_revision: 1,
            created_at: Timestamp::from_unix_millis(1_000)?,
        })
        .await?;
    store
        .insert_project_key(ProjectKeyIdentity {
            key: KEY,
            project_id: ProjectId::new(42)?,
            state: ProjectKeyState::Active,
            label: ProjectKeyLabel::new("default")?,
            created_at: Timestamp::from_unix_millis(1_000)?,
        })
        .await?;
    Ok(())
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
        "metric_phase2_e2e_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
