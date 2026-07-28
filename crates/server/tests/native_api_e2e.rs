use std::{
    error::Error,
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use metric_application::{
    auth::{
        AuthConfig, BootstrapRequest, CreateApiTokenRequest, IdentityService, LoginRequest,
        PasswordConfig, PasswordInput,
    },
    dispatcher::{Dispatcher, DispatcherConfig},
    finalizer::{Finalizer, FinalizerConfig},
    incident_capsule::{IncidentCapsuleAccess, IncidentCapsuleConfig, IncidentCapsuleService},
    ingest::{AttachmentIngestConfig, IngestService},
    native_api::NativeApiService,
    normalizer::{Normalizer, NormalizerLimits},
    processor::{
        FinalizerBatchConfig, FinalizerBatcher, GrouperStage, IssuePreparerStage, Processor,
        ProcessorConfig,
    },
    projects::{CreateProject, ProjectCacheConfig, ProjectService},
    search::{SearchConfig, SearchService},
    shutdown::ShutdownRoot,
    symbolication::BaselineSymbolicationService,
    writer::{MongoWriter, MongoWriterConfig},
};
use metric_blob::{LocalBlobConfig, LocalBlobStore};
use metric_domain::{
    AcceptedEvent, DisplayName, EventId, EventKey, IpScrubPolicy, ItemCapabilities, OrganizationId,
    OrganizationIdentity, ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits,
    ScrubbedEventPayload, SecretBytes, Slug, Timestamp,
    api::IssueListQuery,
    auth::{
        EmailAddress, Permission, PermissionSet, RequestCorrelationId, SecretDigest, TokenName,
        UserDisplayName,
    },
    event::{EventLevel, EventPlatform},
    finalization::{FinalizeEvent, ProcessedEventPayload, SearchToken},
    grouping::{
        GroupingComponent, GroupingComponentKind, GroupingExplanation, GroupingKey,
        GroupingStrategy, derive_issue_id,
    },
    issue::{
        ActorRef, IssueCommand, IssueCommandAction, IssueGroupingDetail, IssueOccurrence,
        IssueRelease, IssueStatus, IssueTitle,
    },
};
use metric_mongo::{EventCodecConfig, IssueCodecConfig, MongoProjectStore};
use metric_ports::{
    Clock, EventBacklog, EventStore, EventWriteStatus, InvestigationStore, IssueStore,
    ProjectResolver, ProjectStore, RandomError, RandomSource,
};
use metric_server::{config::IngestConfig, http, ingest_http, native_http};
use metric_testkit::FakeOutcomeSink;
use mongodb::{
    Client, Database,
    bson::{Binary, doc, spec::BinarySubtype},
};
use tower::ServiceExt;

const SDK_EVENT: &str = include_str!("fixtures/python-2.32.0-error-event-v1.json");

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_native_query_pagination_search_verification_and_explains() {
    let database = test_database().await.unwrap();
    let result = exercise_queries(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn cumulative_create_project_sdk_event_issue_query_and_lifecycle() {
    let database = test_database().await.unwrap();
    let result = exercise_cumulative_e2e(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Phase 12 performance baseline requires MongoDB 8.0.12"]
async fn performance_native_event_query_rps_p95_p99() {
    let database = test_database().await.unwrap();
    let result = measure_queries(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise_queries(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = setup(database).await?;
    for index in 1..=3 {
        finalize_event(&control, index, index, i64::from(index) * 1_000, "staging").await?;
    }
    let queries =
        control.investigation_store(EventCodecConfig::default(), IssueCodecConfig::default());
    let first = queries
        .list_issues(
            ProjectId::new(42)?,
            IssueListQuery {
                status: None,
                from: None,
                until: None,
                before: None,
                limit: 2,
            },
        )
        .await?;
    assert_eq!(first.items.len(), 2);
    let first_ids = first
        .items
        .iter()
        .map(|issue| issue.issue_id)
        .collect::<Vec<_>>();
    let anchor = first.next.expect("first page has a cursor");

    finalize_event(&control, 4, 4, 4_000, "staging").await?;
    let second = queries
        .list_issues(
            ProjectId::new(42)?,
            IssueListQuery {
                status: None,
                from: None,
                until: None,
                before: Some(anchor),
                limit: 2,
            },
        )
        .await?;
    assert_eq!(second.items.len(), 1);
    assert!(!first_ids.contains(&second.items[0].issue_id));
    assert_eq!(second.items[0].last_seen.unix_millis(), 1_000);

    let event_first = queries
        .list_events(
            ProjectId::new(42)?,
            None,
            Timestamp::from_unix_millis(0)?,
            Timestamp::from_unix_millis(10_000)?,
            None,
            2,
        )
        .await?;
    assert_eq!(event_first.items.len(), 2);
    let event_anchor = event_first.next.expect("event page has cursor");
    finalize_event(&control, 5, 5, 5_000, "staging").await?;
    let event_second = queries
        .list_events(
            ProjectId::new(42)?,
            None,
            Timestamp::from_unix_millis(0)?,
            Timestamp::from_unix_millis(10_000)?,
            Some(event_anchor),
            2,
        )
        .await?;
    assert_eq!(
        event_second
            .items
            .iter()
            .map(|event| event.occurred_at.unix_millis())
            .collect::<Vec<_>>(),
        vec![2_000, 1_000]
    );

    let search = SearchService::new(
        Arc::new(queries.clone()),
        Arc::new(FixedClock(Timestamp::from_unix_millis(6_000)?)),
        SearchConfig::default(),
    )?;
    let staging = search
        .search(ProjectId::new(42)?, "environment:staging", None, Some(10))
        .await?;
    assert_eq!(staging.items.len(), 5);

    let collision_key = EventKey::new(ProjectId::new(42)?, EventId::from_bytes(event_bytes(1)));
    database
        .collection::<mongodb::bson::Document>("error_events")
        .update_one(
            doc! { "_id": binary(collision_key.as_bytes()) },
            doc! { "$set": { "k": [SearchToken::environment("production").stored()] } },
        )
        .await?;
    let production = search
        .search(
            ProjectId::new(42)?,
            "environment:production",
            None,
            Some(10),
        )
        .await?;
    assert!(
        production.items.is_empty(),
        "token candidates must be exactly post-verified against the body"
    );

    let issue = staging.items[0].issue_id;
    let issue_store = control.issue_store(IssueCodecConfig::default());
    issue_store
        .apply_command(IssueCommand {
            project_id: ProjectId::new(42)?,
            issue_id: issue,
            idempotency_key: [8; 16],
            actor: ActorRef::system(),
            at: Timestamp::from_unix_millis(7_000)?,
            action: IssueCommandAction::Resolve,
        })
        .await?;
    let activity = queries
        .issue_activity(ProjectId::new(42)?, issue, None, 10)
        .await?;
    assert_eq!(activity.items.len(), 1);
    assert_eq!(
        issue_store.load(ProjectId::new(42)?, issue).await?.status,
        IssueStatus::Resolved
    );

    assert_eq!(
        queries
            .list_environments(ProjectId::new(42)?, None, 10)
            .await?
            .items
            .len(),
        1
    );
    assert_eq!(
        queries
            .list_releases(OrganizationId::new(7)?, ProjectId::new(42)?, None, 10)
            .await?
            .items
            .len(),
        1
    );

    let token_explain = database
        .run_command(doc! {
            "explain": {
                "find": "error_events",
                "filter": {
                    "p": 42_i32,
                    "k": SearchToken::environment("staging").stored(),
                    "k.0": { "$exists": true },
                    "o": {
                        "$gte": mongodb::bson::DateTime::from_millis(0),
                        "$lt": mongodb::bson::DateTime::from_millis(10_000),
                    },
                },
                "sort": { "o": -1, "_id": -1 },
                "limit": 50_i64,
            },
            "verbosity": "executionStats",
        })
        .await?;
    assert!(format!("{token_explain:?}").contains("event_search_tokens"));
    let issue_explain = database
        .run_command(doc! {
            "explain": {
                "find": "issues",
                "filter": { "p": 42_i32, "s": { "$exists": false } },
                "sort": { "l": -1, "_id": -1 },
                "limit": 50_i64,
            },
            "verbosity": "executionStats",
        })
        .await?;
    assert!(format!("{issue_explain:?}").contains("issue_status_timeline"));
    Ok(())
}

async fn exercise_cumulative_e2e(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let now = Timestamp::from_unix_millis(2_000)?;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
    let random: Arc<dyn RandomSource> = Arc::new(CounterRandom(AtomicU64::new(0)));
    let blob_directory =
        std::env::temp_dir().join(format!("metric-native-e2e-{}", uuid_like_suffix()));
    let blob = LocalBlobStore::new(
        &blob_directory,
        LocalBlobConfig {
            capacity_bytes: 1024 * 1024 + 128,
            reserve_bytes: 128,
            max_object_bytes: 1024 * 1024,
        },
    )
    .await?;
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
        .expect("empty database has setup token");
    let owner = identity
        .bootstrap(BootstrapRequest {
            setup_secret: setup_token,
            email: EmailAddress::parse("owner@example.com")?,
            user_display_name: UserDisplayName::new("Owner")?,
            password: PasswordInput::new("correct horse battery staple")?,
            organization_slug: Slug::new("acme")?,
            organization_name: DisplayName::new("Acme")?,
            request_id: RequestCorrelationId::new("phase12-bootstrap")?,
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
    let issue_service = Arc::new(metric_application::issues::IssueService::new(Arc::new(
        control.issue_store(IssueCodecConfig::default()),
    )));
    let investigation: Arc<dyn InvestigationStore> = Arc::new(
        control.investigation_store(EventCodecConfig::default(), IssueCodecConfig::default()),
    );
    let search = Arc::new(SearchService::new(
        Arc::clone(&investigation),
        Arc::clone(&clock),
        SearchConfig::default(),
    )?);
    let capsule_shutdown = ShutdownRoot::new();
    let capsule_access: Arc<dyn IncidentCapsuleAccess> = identity.clone();
    let capsule = Arc::new(IncidentCapsuleService::new(
        capsule_access,
        Arc::clone(&issue_service),
        Arc::clone(&investigation),
        Arc::clone(&clock),
        IncidentCapsuleConfig::default(),
        capsule_shutdown.signal(),
    )?);
    let native = Arc::new(
        NativeApiService::new(
            Arc::clone(&identity),
            Arc::clone(&projects),
            Arc::clone(&issue_service),
            Arc::clone(&investigation),
            search,
            Arc::clone(&clock),
        )
        .with_blob_store(Arc::new(blob.clone())),
    );
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
                    log: true,
                    transaction: true,
                    span: true,
                    feedback: true,
                    check_in: true,
                    metric: true,
                    replay: true,
                },
                limits: ProjectIngestLimits::default(),
            },
            RequestCorrelationId::new("phase12-project-create")?,
        )
        .await?;

    let root = ShutdownRoot::new();
    let codec = EventCodecConfig::default();
    let event_store = Arc::new(control.event_store(codec));
    let (batcher, batch_task) = FinalizerBatcher::start(
        Arc::new(Finalizer::new(
            Arc::new(control.finalization_store(codec, IssueCodecConfig::default())),
            FinalizerConfig::default(),
        )?),
        FinalizerBatchConfig {
            max_wait: Duration::from_millis(1),
            shutdown_drain: Duration::from_secs(2),
            ..FinalizerBatchConfig::default()
        },
    )?;
    let processor = Arc::new(Processor::new(
        event_store.clone(),
        event_store.clone(),
        Arc::new(Normalizer::new(NormalizerLimits::default())?),
        Arc::new(BaselineSymbolicationService),
        Arc::new(GrouperStage),
        Arc::new(IssuePreparerStage),
        batcher.clone(),
        Arc::clone(&clock),
        ProcessorConfig {
            stage_timeout: Duration::from_secs(2),
            total_timeout: Duration::from_secs(5),
            state_timeout: Duration::from_secs(2),
            ..ProcessorConfig::default()
        },
    )?);
    let backlog: Arc<dyn EventBacklog> = event_store.clone();
    let (dispatcher, dispatcher_task) = Dispatcher::start(
        backlog,
        processor,
        Arc::clone(&clock),
        DispatcherConfig {
            queue_capacity: 32,
            worker_concurrency: 2,
            low_watermark: 4,
            refill_target: 24,
            refill_batch_size: 24,
            poll_interval: Duration::from_millis(5),
            metrics_interval: Duration::from_secs(1),
            source_timeout: Duration::from_secs(2),
            shutdown_drain: Duration::from_secs(2),
            max_pending_events: None,
            max_oldest_pending_age: Some(Duration::from_secs(3_600)),
        },
        root.signal(),
    )
    .await?;
    let (writer, writer_task) = MongoWriter::start(
        event_store,
        dispatcher,
        MongoWriterConfig {
            channel_capacity: 32,
            max_wait: Duration::from_millis(1),
            max_documents: 100,
            max_bytes: 8 * 1024 * 1024,
            operation_timeout: Duration::from_secs(2),
            shutdown_drain: Duration::from_secs(2),
        },
        root.signal(),
    )?;
    let resolver: Arc<dyn ProjectResolver> = projects;
    let ingest = Arc::new(
        IngestService::new(
            resolver,
            writer,
            Arc::new(FakeOutcomeSink::default()),
            Arc::clone(&clock),
            random,
            32,
            root.signal(),
        )
        .with_blob_store(Arc::new(blob.clone()), AttachmentIngestConfig::default()),
    );
    let app = http::router(
        root.signal(),
        metric_application::observability::Metrics,
        ingest_http::router(ingest, ingest_config(), root.signal()),
    );
    assert_eq!(
        app.oneshot(sdk_request(created.project_id, created.dsn_key))
            .await?
            .status(),
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if database
                .collection::<mongodb::bson::Document>("error_events")
                .find_one(doc! { "p": created.project_id.get(), "q": { "$exists": false } })
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let issues = native
        .list_issues(
            &owner,
            created.project_id,
            metric_application::native_api::IssueListRequest {
                status: None,
                from: None,
                until: None,
                cursor: None,
                limit: Some(10),
            },
        )
        .await?;
    assert_eq!(issues.items.len(), 1);
    let sdk_event_id = EventId::parse("aa40a14691564910ae6eb2affdba35f9")?;
    let attachments = native
        .event_attachments(&owner, created.project_id, sdk_event_id)
        .await?;
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].filename.as_ref(), "sdk-context.json");
    let (metadata, mut reader) = native
        .open_event_attachment(
            &owner,
            created.project_id,
            sdk_event_id,
            attachments[0].attachment_id,
        )
        .await?;
    assert_eq!(metadata.content_type.as_ref(), "application/json");
    let bytes = reader
        .read_chunk(1024)
        .await?
        .expect("SDK attachment has bytes");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?,
        serde_json::json!({"source": "native-e2e"})
    );
    drop(reader);
    let issue_id = issues.items[0].issue_id;
    let resolved = native
        .issue_command(
            &owner,
            created.project_id,
            issue_id,
            [9; 16],
            IssueCommandAction::Resolve,
        )
        .await?;
    assert!(resolved.applied);
    assert_eq!(resolved.issue.status, IssueStatus::Resolved);
    let session = identity
        .login(LoginRequest {
            email: "owner@example.com".into(),
            password: "correct horse battery staple".into(),
            organization_id: owner.organization_id,
            client_network_digest: SecretDigest::new([19; 32]),
            request_id: RequestCorrelationId::new("phase19-login")?,
        })
        .await?;
    let web_owner = identity
        .authenticate_session(
            &session.session,
            Some(&session.csrf),
            true,
            owner.organization_id,
        )
        .await?;
    let token = identity
        .create_api_token(
            &web_owner,
            CreateApiTokenRequest {
                name: TokenName::new("capsule-e2e")?,
                scopes: PermissionSet::from_permissions([
                    Permission::IssueRead,
                    Permission::EventRead,
                    Permission::IncidentExport,
                ]),
                expires_at: Timestamp::from_unix_millis(now.unix_millis() + 60_000)?,
                request_id: RequestCorrelationId::new("phase19-token")?,
            },
        )
        .await?;
    let limited_token = identity
        .create_api_token(
            &web_owner,
            CreateApiTokenRequest {
                name: TokenName::new("capsule-without-export")?,
                scopes: PermissionSet::from_permissions([
                    Permission::IssueRead,
                    Permission::EventRead,
                ]),
                expires_at: Timestamp::from_unix_millis(now.unix_millis() + 60_000)?,
                request_id: RequestCorrelationId::new("phase19-limited-token")?,
            },
        )
        .await?;
    let capsule_app = http::router(
        capsule_shutdown.signal(),
        metric_application::observability::Metrics,
        native_http::router(
            Some(Arc::clone(&identity)),
            Some(Arc::clone(&native)),
            false,
            true,
            native_http::NativeHttpModules {
                incident_capsule: Some(Arc::clone(&capsule)),
                ..native_http::NativeHttpModules::default()
            },
        ),
    );
    let capsule_uri = format!(
        "/api/v1/projects/{}/issues/{issue_id}/capsule",
        created.project_id.get()
    );
    assert_eq!(
        capsule_app
            .clone()
            .oneshot(
                Request::post(&capsule_uri)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))?,
            )
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        capsule_app
            .clone()
            .oneshot(
                Request::post(&capsule_uri)
                    .header(
                        "authorization",
                        format!("Bearer {}", limited_token.secret.encode_hex()),
                    )
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))?,
            )
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        capsule_app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/projects/{}/issues/{}/capsule",
                    created.project_id.get(),
                    hex::encode([0xff; 16])
                ))
                .header(
                    "authorization",
                    format!("Bearer {}", token.secret.encode_hex()),
                )
                .header("content-type", "application/json")
                .body(Body::from("{}"))?,
            )
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    let response = capsule_app
        .oneshot(
            Request::post(capsule_uri)
                .header(
                    "authorization",
                    format!("Bearer {}", token.secret.encode_hex()),
                )
                .header("content-type", "application/json")
                .body(Body::from("{}"))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.incident-capsule+zip; version=1")
    );
    let capsule_bytes = axum::body::to_bytes(response.into_body(), 100 * 1024 * 1024)
        .await?
        .to_vec();
    let validated = metric_testkit::incident_capsule::validate(&capsule_bytes)?;
    assert!(validated.entries.contains_key("issue.json"));
    assert!(
        validated
            .entries
            .contains_key(&format!("events/{sdk_event_id}.json"))
    );
    assert!(validated.entries.contains_key("activity.json"));
    assert_eq!(validated.manifest["version"], 1);
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("audit_log")
            .count_documents(doc! { "action": "incident_capsule.exported" })
            .await?,
        1
    );
    capsule_shutdown.begin();

    root.begin();
    writer_task.wait().await;
    dispatcher_task.wait().await;
    batcher.close();
    batch_task.wait().await;
    std::fs::remove_dir_all(blob_directory)?;
    Ok(())
}

async fn measure_queries(database: &Database) -> Result<(), Box<dyn Error>> {
    const EVENTS: u32 = 2_000;
    const QUERIES: usize = 1_000;
    let control = setup(database).await?;
    for chunk_start in (0..EVENTS).step_by(100) {
        let mut finalizer_events = Vec::new();
        for index in chunk_start..(chunk_start + 100).min(EVENTS) {
            prepare_finalized(
                &control,
                index + 1,
                1,
                i64::from(index) * 10,
                "benchmark",
                &mut finalizer_events,
            )
            .await?;
        }
        Finalizer::new(
            Arc::new(
                control
                    .finalization_store(EventCodecConfig::default(), IssueCodecConfig::default()),
            ),
            FinalizerConfig {
                max_batch_events: 100,
                ..FinalizerConfig::default()
            },
        )?
        .finalize(finalizer_events)
        .await?;
    }
    let queries =
        control.investigation_store(EventCodecConfig::default(), IssueCodecConfig::default());
    let mut samples = Vec::with_capacity(QUERIES);
    let started = Instant::now();
    for _ in 0..QUERIES {
        let query_started = Instant::now();
        let page = queries
            .list_events(
                ProjectId::new(42)?,
                None,
                Timestamp::from_unix_millis(0)?,
                Timestamp::from_unix_millis(100_000)?,
                None,
                50,
            )
            .await?;
        assert_eq!(page.items.len(), 50);
        samples.push(query_started.elapsed());
    }
    let elapsed = started.elapsed();
    samples.sort_unstable();
    let p95 = samples[(samples.len() - 1) * 95 / 100];
    let p99 = samples[(samples.len() - 1) * 99 / 100];
    let rps = QUERIES as f64 / elapsed.as_secs_f64();
    eprintln!(
        "Phase12 Native API query: dataset_events={EVENTS},queries={QUERIES},page=50,rps={rps:.0},p95_ms={:.3},p99_ms={:.3},elapsed_ms={}",
        p95.as_secs_f64() * 1_000.0,
        p99.as_secs_f64() * 1_000.0,
        elapsed.as_millis()
    );
    assert!(rps > 100.0);
    assert!(p95 < Duration::from_millis(100));
    assert!(p99 < Duration::from_millis(250));
    Ok(())
}

async fn setup(database: &Database) -> Result<MongoProjectStore, Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let now = Timestamp::from_unix_millis(0)?;
    control
        .insert_organization(OrganizationIdentity {
            id: OrganizationId::new(7)?,
            slug: Slug::new("acme")?,
            display_name: DisplayName::new("Acme")?,
            created_at: now,
        })
        .await?;
    control
        .insert_project(ProjectIdentity {
            id: ProjectId::new(42)?,
            organization_id: OrganizationId::new(7)?,
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
            created_at: now,
        })
        .await?;
    Ok(control)
}

async fn finalize_event(
    control: &MongoProjectStore,
    event_index: u32,
    issue_index: u32,
    occurred_at: i64,
    environment: &str,
) -> Result<(), Box<dyn Error>> {
    let mut events = Vec::new();
    prepare_finalized(
        control,
        event_index,
        issue_index,
        occurred_at,
        environment,
        &mut events,
    )
    .await?;
    Finalizer::new(
        Arc::new(
            control.finalization_store(EventCodecConfig::default(), IssueCodecConfig::default()),
        ),
        FinalizerConfig::default(),
    )?
    .finalize(events)
    .await?;
    Ok(())
}

async fn prepare_finalized(
    control: &MongoProjectStore,
    event_index: u32,
    issue_index: u32,
    occurred_at: i64,
    environment: &str,
    output: &mut Vec<FinalizeEvent>,
) -> Result<(), Box<dyn Error>> {
    let project_id = ProjectId::new(42)?;
    let event_id = EventId::from_bytes(event_bytes(event_index));
    let body = format!(
        r#"{{"event_id":"{event_id}","environment":"{environment}","release":"backend@1.0","platform":"rust","level":"error","message":"fixture {event_index}"}}"#
    );
    let accepted = AcceptedEvent {
        project_id,
        event_id,
        received_at: Timestamp::from_unix_millis(occurred_at + 1)?,
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(body.as_bytes().to_vec()),
    };
    let event_store = control.event_store(EventCodecConfig::default());
    let prepared = event_store.prepare(accepted)?;
    assert_eq!(
        event_store.insert_batch(&[prepared]).await?,
        vec![EventWriteStatus::Inserted]
    );
    let mut key_bytes = [u8::try_from(issue_index % 251 + 1)?; 34];
    key_bytes[..2].copy_from_slice(&1_u16.to_be_bytes());
    let grouping_key = GroupingKey::parse(&key_bytes)?;
    let issue_id = derive_issue_id(project_id, grouping_key);
    let timestamp = Timestamp::from_unix_millis(occurred_at)?;
    let occurrence = IssueOccurrence {
        project_id,
        issue_id,
        grouping_key,
        event_id,
        occurred_at: timestamp,
        received_at: Timestamp::from_unix_millis(occurred_at + 1)?,
        release: Some(IssueRelease::new("backend@1.0")?),
        title: IssueTitle::new(format!("Fixture {issue_index}"))?,
        culprit: None,
        grouping: IssueGroupingDetail {
            strategy: GroupingStrategy::Message,
            explanation: GroupingExplanation {
                summary: "fixture grouping".into(),
                components: vec![GroupingComponent {
                    kind: GroupingComponentKind::Message,
                    value: format!("fixture {issue_index}").into(),
                }],
            },
        },
        increment: NonZeroU64::MIN,
    };
    output.push(FinalizeEvent {
        project_id,
        event_id,
        received_at: Timestamp::from_unix_millis(occurred_at + 1)?,
        occurred_at: timestamp,
        level: EventLevel::Error,
        platform: EventPlatform::Rust,
        issue: occurrence,
        environment: Some(environment.into()),
        search_tokens: vec![
            SearchToken::environment(environment),
            SearchToken::release("backend@1.0"),
        ],
        payload: ProcessedEventPayload::new(body.into_bytes()),
    });
    Ok(())
}

fn event_bytes(index: u32) -> [u8; 16] {
    u128::from(index).to_be_bytes()
}

fn binary(bytes: impl AsRef<[u8]>) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.as_ref().to_vec(),
    }
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

fn sdk_request(project_id: ProjectId, key: metric_domain::DsnKey) -> Request<Body> {
    let attachment = r#"{"source":"native-e2e"}"#;
    let envelope = format!(
        "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{}\n{{\"type\":\"attachment\",\"length\":{},\"filename\":\"sdk-context.json\",\"content_type\":\"application/json\"}}\n{}",
        SDK_EVENT.len(),
        SDK_EVENT,
        attachment.len(),
        attachment
    );
    Request::builder()
        .method("POST")
        .uri(format!("/api/{}/envelope/", project_id.get()))
        .header(
            "x-sentry-auth",
            format!("Sentry sentry_version=7,sentry_key={key}"),
        )
        .body(Body::from(envelope))
        .expect("SDK request is valid")
}

fn ingest_config() -> IngestConfig {
    IngestConfig {
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
    Ok(client.database(&format!("metric_phase12_{}", uuid_like_suffix())))
}

fn uuid_like_suffix() -> String {
    format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}
