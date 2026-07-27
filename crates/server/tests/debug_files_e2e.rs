use std::{
    error::Error,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use metric_application::{
    artifacts::{ArtifactConfig, ArtifactService, AssembleArtifact, AssembleArtifactState},
    auth::{AuthConfig, CreateApiTokenRequest, IdentityService, PasswordConfig},
    debug_files::{DebugFileConfig, DebugFileService},
    normalizer::{Normalizer, NormalizerLimits},
    symbolication::{SymbolicationConfig, SymbolicationService},
};
use metric_blob::{LocalBlobConfig, LocalBlobStore};
use metric_domain::{
    AcceptedEvent, DisplayName, EventId, IpScrubPolicy, ItemCapabilities, ProjectAcceptanceState,
    ProjectId, ProjectIdentity, ProjectIngestLimits, ScrubbedEventPayload, SecretBytes, Slug,
    Timestamp,
    artifacts::ArtifactBinding,
    auth::{
        Actor, AuthContext, CredentialId, OrganizationRole, Permission, PermissionSet, TokenName,
        UserId,
    },
    blob::{BlobKey, BlobKind},
    debug_files::{DebugFileId, DebugId, DebugUpload},
    finalization::derive_release_id,
    symbolication::{SymbolicationKind, SymbolicationRequest, SymbolicationStatus},
};
use metric_mongo::{ArtifactQuota, DebugFileQuota, MongoProjectStore};
use metric_ports::{
    ArtifactStore, BlobStore, BlobStoreError, Clock, DebugFileStore, ProjectStore, RandomError,
    RandomSource, SymbolicationBackend,
};
use metric_server::debug_http;
use metric_symbolication::{ExternalSymbolicator, ExternalSymbolicatorConfig, PrivateSourceSigner};
use metric_testkit::FixedClock;
use mongodb::{Client, Database, bson::doc};
use sha1::{Digest as _, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use url::Url;
use wait_timeout::ChildExt;

#[tokio::test]
#[ignore = "requires local MongoDB and npm ci in sdk-tests/sentry-cli"]
async fn real_pinned_sentry_cli_upload_private_isolation_and_exact_delete() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error + Send + Sync>> {
    let now = Timestamp::from_unix_millis(1_800_000_000_000)?;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
    let random: Arc<dyn RandomSource> = Arc::new(CounterRandom(AtomicU64::new(10)));
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
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
    let organization_id = metric_domain::OrganizationId::new(11)?;
    control
        .insert_organization(metric_domain::OrganizationIdentity {
            id: organization_id,
            slug: Slug::new("acme")?,
            display_name: DisplayName::new("Acme")?,
            created_at: now,
        })
        .await?;
    for (id, slug) in [(7, "native"), (8, "foreign")] {
        control
            .insert_project(ProjectIdentity {
                id: ProjectId::new(id)?,
                organization_id,
                slug: Slug::new(slug)?,
                display_name: DisplayName::new(slug)?,
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
    }
    let context = AuthContext {
        actor: Actor::Bootstrap,
        user_id: UserId::new(1)?,
        organization_id,
        role: OrganizationRole::Owner,
        permissions: PermissionSet::from_role(OrganizationRole::Owner),
        credential_id: CredentialId::new(1)?,
    };
    // The direct context needs an authoritative user/membership before token creation.
    let setup = identity
        .ensure_bootstrap_token()
        .await?
        .expect("new database emits setup token");
    let bootstrap = identity
        .bootstrap(metric_application::auth::BootstrapRequest {
            setup_secret: setup,
            email: metric_domain::auth::EmailAddress::parse("owner@example.com")?,
            user_display_name: metric_domain::auth::UserDisplayName::new("Owner")?,
            password: metric_application::auth::PasswordInput::new("correct horse battery staple")?,
            organization_slug: Slug::new("auth-org")?,
            organization_name: DisplayName::new("Auth Org")?,
            request_id: metric_domain::BoundedId::new("phase17-bootstrap")?,
        })
        .await?;
    // Debug projects belong to the bootstrapped organization used by the real API token.
    database
        .collection::<mongodb::bson::Document>("projects")
        .update_many(
            doc! { "_id": { "$in": [7_i32, 8_i32] } },
            doc! { "$set": { "organization_id": i64::try_from(bootstrap.organization_id.get())? } },
        )
        .await?;
    database
        .collection::<mongodb::bson::Document>("organizations")
        .delete_one(doc! { "_id": i64::try_from(context.organization_id.get())? })
        .await?;
    database
        .collection::<mongodb::bson::Document>("organizations")
        .update_one(
            doc! { "_id": i64::try_from(bootstrap.organization_id.get())? },
            doc! { "$set": { "slug": "acme" } },
        )
        .await?;
    let token_context = AuthContext {
        actor: Actor::WebSession,
        ..bootstrap.clone()
    };
    let token = identity
        .create_api_token(
            &token_context,
            CreateApiTokenRequest {
                name: TokenName::new("sentry-cli")?,
                scopes: PermissionSet::from_permissions([
                    Permission::DebugFileRead,
                    Permission::DebugFileWrite,
                    Permission::DebugFileDelete,
                    Permission::ArtifactRead,
                    Permission::ArtifactWrite,
                    Permission::ArtifactDelete,
                ]),
                expires_at: Timestamp::from_unix_millis(now.unix_millis() + 86_400_000)?,
                request_id: metric_domain::BoundedId::new("phase17-token")?,
            },
        )
        .await?;
    let root = std::env::temp_dir().join(format!("metric-debug-e2e-{}", uuid::Uuid::new_v4()));
    let blobs: Arc<dyn BlobStore> = Arc::new(
        LocalBlobStore::new(
            &root,
            LocalBlobConfig {
                capacity_bytes: 128 * 1024 * 1024,
                reserve_bytes: 8 * 1024 * 1024,
                max_object_bytes: 64 * 1024 * 1024,
            },
        )
        .await?,
    );
    let metadata: Arc<dyn DebugFileStore> =
        Arc::new(control.debug_file_store(DebugFileQuota::default()));
    let service = Arc::new(DebugFileService::new(
        Arc::clone(&metadata),
        Arc::clone(&blobs),
        Arc::clone(&clock),
        DebugFileConfig::default(),
    )?);
    let artifact_metadata: Arc<dyn ArtifactStore> =
        Arc::new(control.artifact_store(ArtifactQuota::default()));
    let artifact_service = Arc::new(ArtifactService::new(
        artifact_metadata,
        Arc::clone(&blobs),
        Arc::clone(&clock),
        Arc::clone(&random),
        ArtifactConfig::default(),
    )?);
    let api_context = identity.authenticate_api_token(&token.secret).await?;
    exercise_recovery_and_cleanup(
        &service,
        &metadata,
        &blobs,
        &api_context,
        bootstrap.organization_id,
        now,
    )
    .await?;
    let signer = PrivateSourceSigner::new(vec![7; 32], None)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = debug_http::router(
        Some(Arc::clone(&identity)),
        Some(Arc::clone(&service)),
        Some(Arc::clone(&artifact_service)),
        Some(signer.clone()),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let cli_root = repository_root().join("sdk-tests").join("sentry-cli");
    let fixture = cli_root.join("fixtures").join("metric.sym");
    for (package, executable) in [
        (
            "@sentry/cli",
            cli_root
                .join("node_modules")
                .join("@sentry")
                .join("cli-win32-x64")
                .join("bin")
                .join("sentry-cli.exe"),
        ),
        (
            "sentry-cli-v2",
            cli_root
                .join("node_modules")
                .join("sentry-cli-v2")
                .join("node_modules")
                .join("@sentry")
                .join("cli-win32-x64")
                .join("bin")
                .join("sentry-cli.exe"),
        ),
    ] {
        let fixture = fixture.clone();
        let url = format!("http://{address}/");
        let secret = token.secret.encode_hex();
        let output = tokio::task::spawn_blocking(move || run_cli(executable, fixture, url, secret))
            .await??;
        assert!(
            output.status.success(),
            "real {package} failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let sourcemap_fixture = cli_root.join("fixtures").join("sourcemaps");
    for (package, executable) in [
        (
            "@sentry/cli",
            cli_root
                .join("node_modules")
                .join("@sentry")
                .join("cli-win32-x64")
                .join("bin")
                .join("sentry-cli.exe"),
        ),
        (
            "sentry-cli-v2",
            cli_root
                .join("node_modules")
                .join("sentry-cli-v2")
                .join("node_modules")
                .join("@sentry")
                .join("cli-win32-x64")
                .join("bin")
                .join("sentry-cli.exe"),
        ),
    ] {
        let fixture = sourcemap_fixture.clone();
        let url = format!("http://{address}/");
        let secret = token.secret.encode_hex();
        let output = tokio::task::spawn_blocking(move || {
            run_sourcemaps_cli(executable, fixture, url, secret)
        })
        .await??;
        assert!(
            output.status.success(),
            "real {package} sourcemaps failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let modern_debug_id = DebugId::parse("67e9247c-814e-392b-a027-dbde6748fcbf")?;
    let modern = artifact_service
        .lookup(
            ProjectId::new(7)?,
            vec![modern_debug_id.clone()],
            None,
            None,
        )
        .await?;
    assert!(!modern.is_empty());
    let legacy = artifact_service
        .lookup(
            ProjectId::new(7)?,
            Vec::new(),
            Some("metric-phase18@1.0.0"),
            Some("windows".into()),
        )
        .await?;
    assert!(!legacy.is_empty());
    let explain = database
        .run_command(doc! {
            "explain": {
                "find": "artifact_bundles",
                "filter": { "b": { "$elemMatch": { "p": 7_i32, "d": "windows" } } },
                "hint": "artifact_legacy_binding",
            },
            "verbosity": "queryPlanner",
        })
        .await?;
    let explain_text = format!("{explain:?}");
    assert!(
        explain_text.contains("artifact_legacy_binding")
            || explain_text.contains("artifact_project_list"),
        "artifact lookup explain did not select an artifact binding index: {explain_text}"
    );
    let client = reqwest::Client::new();
    let private_index = client
        .get(format!(
            "http://{address}/internal/symbolicator/projects/7/artifact-lookup/?revision=1&debug_id={modern_debug_id}"
        ))
        .bearer_auth(signer.artifact_token(ProjectId::new(7)?))
        .send()
        .await?;
    assert_eq!(private_index.status(), reqwest::StatusCode::OK);
    let private_download = client
        .get(format!(
            "http://{address}/internal/symbolicator/projects/7/artifact-lookup/?revision=1&id={}",
            modern[0].bundle.id
        ))
        .bearer_auth(signer.artifact_token(ProjectId::new(7)?))
        .send()
        .await?;
    assert_eq!(private_download.status(), reqwest::StatusCode::OK);
    assert_eq!(
        private_download.bytes().await?.len() as u64,
        modern[0].bundle.size
    );
    let artifact_cross = client
        .get(format!(
            "http://{address}/internal/symbolicator/projects/8/artifact-lookup/?revision=1&debug_id={modern_debug_id}"
        ))
        .bearer_auth(signer.artifact_token(ProjectId::new(7)?))
        .send()
        .await?;
    assert_eq!(artifact_cross.status(), reqwest::StatusCode::UNAUTHORIZED);

    let release_id = derive_release_id(bootstrap.organization_id, "metric-phase18@1.0.0");
    let native_binding =
        ArtifactBinding::new(ProjectId::new(7)?, Some(release_id), Some("windows".into()))?;
    let foreign_binding =
        ArtifactBinding::new(ProjectId::new(8)?, Some(release_id), Some("windows".into()))?;
    let shared = artifact_service
        .assemble(
            &api_context,
            "acme",
            AssembleArtifact {
                sha1: modern[0].bundle.sha1,
                chunks: vec![modern[0].bundle.sha1],
                project_slugs: vec!["foreign".into()],
                release: Some("metric-phase18@1.0.0".into()),
                dist: Some("windows".into()),
            },
        )
        .await?;
    let AssembleArtifactState::Ok { bundle: shared } = shared else {
        panic!("shared artifact association was not published")
    };
    assert!(shared.bindings.contains(&native_binding));
    assert!(shared.bindings.contains(&foreign_binding));
    assert!(
        artifact_service
            .remove_binding(&api_context, shared.id, native_binding.clone())
            .await?
    );
    assert_eq!(artifact_service.gc_once().await?, 0);
    assert!(
        !artifact_service
            .lookup(
                ProjectId::new(8)?,
                vec![modern_debug_id.clone()],
                None,
                None,
            )
            .await?
            .is_empty()
    );
    assert!(
        artifact_service
            .remove_binding(&api_context, shared.id, foreign_binding)
            .await?
    );
    let rescued = artifact_service
        .assemble(
            &api_context,
            "acme",
            AssembleArtifact {
                sha1: shared.sha1,
                chunks: vec![shared.sha1],
                project_slugs: vec!["native".into()],
                release: Some("metric-phase18@1.0.0".into()),
                dist: Some("windows".into()),
            },
        )
        .await?;
    let AssembleArtifactState::Ok { bundle: rescued } = rescued else {
        panic!("orphan artifact was not rescued")
    };
    assert_eq!(rescued.generation, 0);
    assert!(
        artifact_service
            .remove_binding(&api_context, rescued.id, native_binding)
            .await?
    );
    database
        .collection::<mongodb::bson::Document>("artifact_bundles")
        .update_one(
            doc! { "_id": mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: rescued.id.as_bytes().to_vec() } },
            doc! { "$set": { "e": mongodb::bson::DateTime::from_millis(now.unix_millis()) } },
        )
        .await?;
    assert_eq!(artifact_service.gc_once().await?, 1);
    let republished = artifact_service
        .assemble(
            &api_context,
            "acme",
            AssembleArtifact {
                sha1: rescued.sha1,
                chunks: vec![rescued.sha1],
                project_slugs: vec!["native".into()],
                release: Some("metric-phase18@1.0.0".into()),
                dist: Some("windows".into()),
            },
        )
        .await?;
    let AssembleArtifactState::Ok {
        bundle: republished,
    } = republished
    else {
        panic!("collected artifact was not republished")
    };
    assert_eq!(republished.generation, 1);
    if std::env::var("METRIC_PHASE18_PERF").as_deref() == Ok("1") {
        let samples = 300_u64;
        let started = Instant::now();
        for _ in 0..samples {
            assert!(
                !artifact_service
                    .lookup(
                        ProjectId::new(7)?,
                        vec![modern_debug_id.clone()],
                        None,
                        None,
                    )
                    .await?
                    .is_empty()
            );
        }
        let modern_rps = samples as f64 / started.elapsed().as_secs_f64();
        let started = Instant::now();
        for _ in 0..samples {
            assert!(
                !artifact_service
                    .lookup(
                        ProjectId::new(7)?,
                        Vec::new(),
                        Some("metric-phase18@1.0.0"),
                        Some("windows".into()),
                    )
                    .await?
                    .is_empty()
            );
        }
        let legacy_rps = samples as f64 / started.elapsed().as_secs_f64();
        let missing_id = DebugId::parse("ffffffff-ffff-ffff-ffff-ffffffffffff")?;
        let started = Instant::now();
        for _ in 0..samples {
            assert!(
                artifact_service
                    .lookup(ProjectId::new(7)?, vec![missing_id.clone()], None, None,)
                    .await?
                    .is_empty()
            );
        }
        let miss_rps = samples as f64 / started.elapsed().as_secs_f64();
        let circuit_rps = circuit_open_rps(ProjectId::new(7)?).await?;
        eprintln!(
            "Phase18 Artifact lookup: samples={samples},modern_hit_rps={modern_rps:.0},legacy_hit_rps={legacy_rps:.0},miss_rps={miss_rps:.0},open_circuit_rps={circuit_rps:.0}"
        );
    }
    symbolicate_uploaded_javascript(address, signer.clone(), modern_debug_id, now).await?;

    let debug_id = DebugId::parse("67e9247c-814e-392b-a027-dbde6748fcbf-1")?;
    let files = service
        .find(ProjectId::new(7)?, Some(debug_id.clone()), None)
        .await?;
    assert_eq!(files.len(), 1);
    let file = files[0].clone();
    let own = client
        .get(format!(
            "http://{address}/internal/symbolicator/projects/7/debug-files/?revision=1&debug_id={debug_id}"
        ))
        .bearer_auth(signer.token(ProjectId::new(7)?))
        .send()
        .await?;
    assert_eq!(own.status(), reqwest::StatusCode::OK);
    let downloaded = client
        .get(format!(
            "http://{address}/internal/symbolicator/projects/7/debug-files/?revision=1&id={}",
            file.id
        ))
        .bearer_auth(signer.token(ProjectId::new(7)?))
        .send()
        .await?;
    assert_eq!(downloaded.status(), reqwest::StatusCode::OK);
    assert_eq!(
        downloaded.bytes().await?.as_ref(),
        tokio::fs::read(&fixture).await?
    );
    symbolicate_uploaded_native(address, signer.clone(), debug_id.clone(), now).await?;
    let cross = client
        .get(format!(
            "http://{address}/internal/symbolicator/projects/8/debug-files/?revision=1&debug_id={debug_id}"
        ))
        .bearer_auth(signer.token(ProjectId::new(7)?))
        .send()
        .await?;
    assert_eq!(cross.status(), reqwest::StatusCode::UNAUTHORIZED);
    if std::env::var("METRIC_PHASE17_PERF").as_deref() == Ok("1") {
        let samples = 500_u64;
        let started = Instant::now();
        for _ in 0..samples {
            assert_eq!(
                service
                    .find(ProjectId::new(7)?, Some(debug_id.clone()), None)
                    .await?
                    .len(),
                1
            );
        }
        let hit_rps = samples as f64 / started.elapsed().as_secs_f64();
        let missing_id = DebugId::parse("11111111-2222-3333-4444-555555555555")?;
        let started = Instant::now();
        for _ in 0..samples {
            assert!(
                service
                    .find(ProjectId::new(7)?, Some(missing_id.clone()), None)
                    .await?
                    .is_empty()
            );
        }
        let miss_rps = samples as f64 / started.elapsed().as_secs_f64();
        eprintln!(
            "Phase 17 private index baseline: hit_rps={hit_rps:.0},miss_rps={miss_rps:.0},samples={samples}"
        );
        assert!(hit_rps.is_finite() && hit_rps > 0.0);
        assert!(miss_rps.is_finite() && miss_rps > 0.0);
        let failure_rps = circuit_open_rps(ProjectId::new(7)?).await?;
        eprintln!("Phase 17 circuit-open baseline: failure_rps={failure_rps:.0},samples=10000");
        assert!(failure_rps.is_finite() && failure_rps > 0.0);
    }
    assert!(
        service
            .delete(&api_context, ProjectId::new(7)?, file.id)
            .await?
    );
    assert!(
        !service
            .delete(&api_context, ProjectId::new(7)?, file.id)
            .await?
    );
    assert!(
        service
            .find(ProjectId::new(7)?, Some(debug_id), None)
            .await?
            .is_empty()
    );

    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

async fn symbolicate_uploaded_native(
    metric_address: std::net::SocketAddr,
    signer: PrivateSourceSigner,
    debug_id: DebugId,
    now: Timestamp,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let expected_debug_id = debug_id.clone();
    let fake = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 32 * 1024];
        let count = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..count]);
        assert!(request.contains("revision=1"));
        assert!(request.contains(&expected_debug_id.to_string()));
        let body = r#"{"status":"complete","stacktraces":[{"frames":[{"status":"symbolicated","original_index":0,"function":"MetricSdkCompatibilityError","filename":"metric.c","module":"metric","lineno":42}]}],"missing_debug_ids":[]}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let adapter = Arc::new(ExternalSymbolicator::new(
        ExternalSymbolicatorConfig {
            endpoint: Url::parse(&format!("http://{address}/symbolicate"))?,
            callback_base_url: Url::parse(&format!("http://{metric_address}/"))?,
            request_timeout: Duration::from_secs(2),
            maximum_concurrency: 1,
            circuit_failure_threshold: 2,
            circuit_cooldown: Duration::from_secs(30),
            maximum_response_bytes: 4096,
        },
        signer,
    )?);
    let event = Normalizer::new(NormalizerLimits::default())?.normalize(&AcceptedEvent {
        project_id: ProjectId::new(7)?,
        event_id: EventId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?,
        received_at: now,
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            format!(
                r#"{{"platform":"native","stacktrace":{{"frames":[{{"instruction_addr":"0x10","package":"metric","filename":"metric.c"}}]}},"debug_meta":{{"images":[{{"type":"breakpad","debug_id":"{debug_id}","image_addr":"0x0","image_size":4096}}]}}}}"#
            )
            .into_bytes(),
        ),
    })?;
    let output = SymbolicationService::new(adapter, SymbolicationConfig::default())?
        .symbolicate_with_revision(&event, 1, &CancellationToken::new())
        .await;
    assert_eq!(output.status, SymbolicationStatus::Complete);
    assert_eq!(
        output.derived[0].frames[0].function.as_deref(),
        Some("MetricSdkCompatibilityError")
    );
    fake.await?;
    Ok(())
}

async fn symbolicate_uploaded_javascript(
    metric_address: std::net::SocketAddr,
    signer: PrivateSourceSigner,
    debug_id: DebugId,
    now: Timestamp,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let expected_debug_id = debug_id.clone();
    let fake = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 32 * 1024];
        let count = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..count]);
        assert!(request.starts_with("POST /symbolicate-js?scope=project-7 "));
        assert!(request.contains("artifact-lookup/?revision=1"));
        assert!(request.contains(&expected_debug_id.to_string()));
        let body_start = request.find("\r\n\r\n").unwrap() + 4;
        let payload: serde_json::Value = serde_json::from_str(&request[body_start..]).unwrap();
        let source = &payload["source"];
        let lookup = reqwest::Client::new()
            .get(format!(
                "{}&debug_id={expected_debug_id}",
                source["url"].as_str().unwrap()
            ))
            .bearer_auth(source["token"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(lookup.status(), reqwest::StatusCode::OK);
        let candidates: serde_json::Value = lookup.json().await.unwrap();
        assert!(
            candidates
                .as_array()
                .is_some_and(|values| !values.is_empty())
        );
        let pinned: serde_json::Value = serde_json::from_str(include_str!(
            "../../../sdk-tests/symbolicator/26.6.0-javascript-contract.json"
        ))
        .unwrap();
        let body = serde_json::to_string(&pinned["response"]).unwrap();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let adapter = Arc::new(ExternalSymbolicator::new(
        ExternalSymbolicatorConfig {
            endpoint: Url::parse(&format!("http://{address}/symbolicate"))?,
            callback_base_url: Url::parse(&format!("http://{metric_address}/"))?,
            request_timeout: Duration::from_secs(2),
            maximum_concurrency: 1,
            circuit_failure_threshold: 2,
            circuit_cooldown: Duration::from_secs(30),
            maximum_response_bytes: 4096,
        },
        signer,
    )?);
    let event = Normalizer::new(NormalizerLimits::default())?.normalize(&AcceptedEvent {
        project_id: ProjectId::new(7)?,
        event_id: EventId::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?,
        received_at: now,
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            format!(
                r#"{{"platform":"javascript","release":"metric-phase18@1.0.0","dist":"windows","stacktrace":{{"frames":[{{"function":"fail","filename":"app.min.js","abs_path":"https://example.invalid/static/app.min.js","lineno":1,"colno":50,"in_app":true}}]}},"debug_meta":{{"images":[{{"type":"sourcemap","debug_id":"{debug_id}","code_file":"https://example.invalid/static/app.min.js"}}]}}}}"#
            )
            .into_bytes(),
        ),
    })?;
    let output = SymbolicationService::new(adapter, SymbolicationConfig::default())?
        .symbolicate_with_revisions(&event, 0, 1, &CancellationToken::new())
        .await;
    assert_eq!(output.status, SymbolicationStatus::Complete);
    assert_eq!(output.raw[0].frames[0].line, Some(1));
    assert_eq!(output.raw[0].frames[0].column, Some(50));
    assert_eq!(
        output.derived[0].frames[0].function.as_deref(),
        Some("fail")
    );
    assert_eq!(output.derived[0].frames[0].line, Some(6));
    fake.await?;
    Ok(())
}

async fn exercise_recovery_and_cleanup(
    service: &DebugFileService,
    metadata: &Arc<dyn DebugFileStore>,
    blobs: &Arc<dyn BlobStore>,
    context: &AuthContext,
    organization_id: metric_domain::OrganizationId,
    now: Timestamp,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let project_id = ProjectId::new(7)?;
    let recovery_debug_id = DebugId::parse("11111111-1111-1111-1111-111111111111-1")?;
    let recovery_bytes = b"MODULE windows x86_64 111111111111111111111111111111111 recovery.pdb\n"
        .to_vec()
        .into_boxed_slice();
    let recovery_sha1: [u8; 20] = Sha1::digest(&recovery_bytes).into();
    service
        .upload_chunk(context, organization_id, recovery_sha1, recovery_bytes)
        .await?;
    metadata
        .upsert_upload(DebugUpload {
            id: upload_id(project_id, recovery_sha1),
            project_id,
            organization_id,
            sha1: recovery_sha1,
            name: "recovery.sym".into(),
            debug_id: Some(recovery_debug_id.clone()),
            code_id: None,
            chunks: vec![recovery_sha1],
            created_at: now,
            updated_at: now,
        })
        .await?;
    assert_eq!(service.recover(10).await?, 1);
    let recovered = service
        .find(project_id, Some(recovery_debug_id), None)
        .await?;
    assert_eq!(recovered.len(), 1);

    let stale_bytes = b"expired chunk".to_vec().into_boxed_slice();
    let stale_sha1: [u8; 20] = Sha1::digest(&stale_bytes).into();
    service
        .upload_chunk(context, organization_id, stale_sha1, stale_bytes)
        .await?;
    let orphan_id = DebugFileId::from_bytes([0x55; 16]);
    let mut writer = blobs.begin(BlobKind::DebugFile, now).await?;
    writer
        .write_chunk(b"orphan".to_vec().into_boxed_slice())
        .await?;
    writer
        .commit(BlobKey::debug_file(project_id, orphan_id))
        .await?;
    let (expired_chunks, orphan_files) = service.cleanup_once().await?;
    assert!(expired_chunks >= 1);
    assert!(orphan_files >= 1);
    assert!(matches!(
        blobs
            .open(&BlobKey::debug_chunk(organization_id, stale_sha1))
            .await,
        Err(BlobStoreError::NotFound)
    ));
    assert!(matches!(
        blobs
            .open(&BlobKey::debug_file(project_id, orphan_id))
            .await,
        Err(BlobStoreError::NotFound)
    ));
    assert!(service.delete(context, project_id, recovered[0].id).await?);
    Ok(())
}

async fn circuit_open_rps(project_id: ProjectId) -> Result<f64, Box<dyn Error + Send + Sync>> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    let adapter = ExternalSymbolicator::new(
        ExternalSymbolicatorConfig {
            endpoint: Url::parse(&format!("http://{address}/symbolicate"))?,
            callback_base_url: Url::parse("http://127.0.0.1:4001/")?,
            request_timeout: Duration::from_secs(1),
            maximum_concurrency: 1,
            circuit_failure_threshold: 1,
            circuit_cooldown: Duration::from_secs(30),
            maximum_response_bytes: 1024,
        },
        PrivateSourceSigner::new(vec![3; 32], None)?,
    )?;
    let request = SymbolicationRequest {
        project_id,
        debug_file_revision: 1,
        artifact_revision: 0,
        kind: SymbolicationKind::Native,
        traces: Vec::new(),
        modules: Vec::new(),
        release: None,
        dist: None,
    };
    assert!(adapter.symbolicate(request.clone()).await.is_err());
    let samples = 10_000_u64;
    let started = Instant::now();
    for _ in 0..samples {
        assert!(adapter.symbolicate(request.clone()).await.is_err());
    }
    Ok(samples as f64 / started.elapsed().as_secs_f64())
}

fn upload_id(project_id: ProjectId, sha1: [u8; 20]) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric/debug-upload-id/v1");
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(&sha1);
    let mut id = [0_u8; 16];
    id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    id
}

fn run_cli(
    executable: PathBuf,
    fixture: PathBuf,
    url: String,
    secret: String,
) -> Result<std::process::Output, Box<dyn Error + Send + Sync>> {
    let mut child = Command::new(executable)
        .args([
            "debug-files",
            "upload",
            "--org",
            "acme",
            "--project",
            "native",
        ])
        .arg(fixture)
        .env("SENTRY_URL", url)
        .env("SENTRY_AUTH_TOKEN", secret)
        .env("SENTRY_LOG_LEVEL", "debug")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if child.wait_timeout(Duration::from_secs(20))?.is_none() {
        child.kill()?;
    }
    Ok(child.wait_with_output()?)
}

fn run_sourcemaps_cli(
    executable: PathBuf,
    fixture: PathBuf,
    url: String,
    secret: String,
) -> Result<std::process::Output, Box<dyn Error + Send + Sync>> {
    let mut child = Command::new(executable)
        .args([
            "sourcemaps",
            "upload",
            "--org",
            "acme",
            "--project",
            "native",
            "--release",
            "metric-phase18@1.0.0",
            "--dist",
            "windows",
            "--url-prefix",
            "~/static",
        ])
        .arg(fixture)
        .env("SENTRY_URL", url)
        .env("SENTRY_AUTH_TOKEN", secret)
        .env("SENTRY_LOG_LEVEL", "debug")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if child.wait_timeout(Duration::from_secs(30))?.is_none() {
        child.kill()?;
    }
    Ok(child.wait_with_output()?)
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("server crate lives under repository/crates")
        .to_path_buf()
}

async fn test_database() -> Result<Database, Box<dyn Error + Send + Sync>> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI")
        .unwrap_or_else(|_| "mongodb://localhost:27017".to_owned());
    let client = Client::with_uri_str(uri).await?;
    let name = format!("metric_phase17_{}", uuid::Uuid::new_v4().simple());
    Ok(client.database(&name))
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
