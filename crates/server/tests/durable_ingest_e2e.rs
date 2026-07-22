use std::{
    error::Error,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use faultkeep_application::{
    ingest::IngestService,
    observability::Metrics,
    projects::{ProjectCacheConfig, ProjectService},
    shutdown::ShutdownRoot,
    writer::{MongoWriter, MongoWriterConfig},
};
use faultkeep_domain::{
    AcceptedEvent, DisplayName, DsnKey, EventId, EventKey, IpScrubPolicy, ItemCapabilities,
    OrganizationId, OrganizationIdentity, ProjectAcceptanceState, ProjectId, ProjectIdentity,
    ProjectIngestLimits, ProjectKeyIdentity, ProjectKeyLabel, ProjectKeyState, SecretBytes, Slug,
    Timestamp,
};
use faultkeep_mongo::{EventCodecConfig, MongoProjectStore, decode_pending_event};
use faultkeep_ports::{
    AcceptedEventHandoff, DurableOutcome, EventPrepareError, EventSink, EventStore,
    EventStoreError, EventWriteStatus, PortFuture, ProjectResolver, ProjectStore,
};
use faultkeep_server::{config::IngestConfig, http, ingest_http};
use faultkeep_testkit::{FakeOutcomeSink, FixedClock, FixedRandom};
use mongodb::{Client, Database, bson::doc};
use tower::ServiceExt;

const KEY: DsnKey = DsnKey::from_bytes([4; 16]);
const EVENT: &str = include_str!("fixtures/python-2.32.0-error-event-v1.json");

#[derive(Default)]
struct CapturingHandoff(Mutex<Vec<EventKey>>);

impl AcceptedEventHandoff for CapturingHandoff {
    fn offer(&self, event: AcceptedEvent) -> Result<(), AcceptedEvent> {
        self.0
            .lock()
            .unwrap()
            .push(EventKey::new(event.project_id, event.event_id));
        Ok(())
    }
}

struct CountingStore {
    inner: faultkeep_mongo::MongoEventStore,
    batch_sizes: Mutex<Vec<usize>>,
}

impl EventStore for CountingStore {
    type Prepared = faultkeep_mongo::MongoPreparedEvent;

    fn prepare(&self, event: AcceptedEvent) -> Result<Self::Prepared, EventPrepareError> {
        self.inner.prepare(event)
    }

    fn insert_batch<'a>(
        &'a self,
        events: &'a [Self::Prepared],
    ) -> PortFuture<'a, Result<Vec<EventWriteStatus>, EventStoreError>> {
        self.batch_sizes.lock().unwrap().push(events.len());
        self.inner.insert_batch(events)
    }
}

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_official_sdk_http_to_mongo_writer_and_mongodb() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "performance baseline requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn performance_mongo_writer_rps_latency_and_occupancy() {
    let database = test_database().await.unwrap();
    let result = measure_writer(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn measure_writer(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let store = Arc::new(CountingStore {
        inner: control.event_store(EventCodecConfig::default()),
        batch_sizes: Mutex::new(Vec::new()),
    });
    let root = ShutdownRoot::new();
    let handoff = Arc::new(CapturingHandoff::default());
    let (writer, task) = MongoWriter::start(
        Arc::clone(&store),
        handoff.clone(),
        MongoWriterConfig {
            channel_capacity: 512,
            max_wait: Duration::from_millis(20),
            max_documents: 250,
            max_bytes: 8 * 1024 * 1024,
            operation_timeout: Duration::from_secs(10),
            shutdown_drain: Duration::from_secs(10),
        },
        root.signal(),
    )?;

    let iterations = 20_000_u32;
    let started = Instant::now();
    let mut samples = Vec::with_capacity(iterations as usize);
    for chunk_start in (0..iterations).step_by(512) {
        let mut requests = Vec::new();
        for index in chunk_start..(chunk_start + 512).min(iterations) {
            let writer = Arc::clone(&writer);
            requests.push(tokio::spawn(async move {
                let request_started = Instant::now();
                let result = writer.persist(performance_event(index)).await;
                (result, request_started.elapsed())
            }));
        }
        for request in requests {
            let (result, elapsed) = request.await?;
            assert_eq!(result, Ok(DurableOutcome::Accepted));
            samples.push(elapsed);
        }
    }
    let elapsed = started.elapsed();
    samples.sort_unstable();
    let percentile = |percent: usize| samples[(samples.len() - 1) * percent / 100];
    let rps = f64::from(iterations) / elapsed.as_secs_f64();
    let batches = store.batch_sizes.lock().unwrap().clone();
    let occupancy = f64::from(iterations) / batches.len() as f64;

    for index in 0..100_u32 {
        assert_eq!(
            writer.persist(performance_event(index)).await,
            Ok(DurableOutcome::Duplicate)
        );
    }
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("events")
            .count_documents(doc! {})
            .await?,
        u64::from(iterations)
    );
    assert_eq!(handoff.0.lock().unwrap().len(), iterations as usize);
    eprintln!(
        "MongoWriter: {rps:.0} events/s, batches={}, avg occupancy={occupancy:.1}, p95={} ms, p99={} ms",
        batches.len(),
        percentile(95).as_millis(),
        percentile(99).as_millis()
    );
    assert!(
        rps >= 5_000.0,
        "MongoWriter baseline {rps:.0} RPS is below steady gate"
    );
    assert!(percentile(95) < Duration::from_millis(100));
    assert!(percentile(99) < Duration::from_millis(250));

    root.begin();
    task.wait().await;
    Ok(())
}

fn performance_event(index: u32) -> AcceptedEvent {
    let mut state = u64::from(index).saturating_add(1);
    let mut message = String::with_capacity(900);
    for _ in 0..900 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        message.push(char::from(b'a' + (state % 26) as u8));
    }
    AcceptedEvent {
        project_id: ProjectId::new(42).unwrap(),
        event_id: EventId::from_bytes(u128::from(index).to_be_bytes()),
        received_at: Timestamp::from_unix_millis(2_000 + i64::from(index)).unwrap(),
        policy_revision: 1,
        payload: faultkeep_domain::ScrubbedEventPayload::new(
            format!(
                r#"{{"event_id":"{}","platform":"rust","level":"error","message":"{message}"}}"#,
                hex::encode(u128::from(index).to_be_bytes())
            )
            .into_bytes(),
        ),
    }
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    seed(&control).await?;

    let root = ShutdownRoot::new();
    let projects = Arc::new(ProjectService::new(
        Arc::new(control.clone()),
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
    let codec = EventCodecConfig::default();
    let event_store = Arc::new(control.event_store(codec));
    let handoff = Arc::new(CapturingHandoff::default());
    let (writer, writer_task) = MongoWriter::start(
        event_store,
        handoff.clone(),
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
    let sink: Arc<dyn EventSink> = writer;
    let ingest = Arc::new(IngestService::new(
        resolver,
        sink,
        Arc::new(FakeOutcomeSink::default()),
        Arc::new(FixedClock(Timestamp::from_unix_millis(2_000)?)),
        Arc::new(FixedRandom(9)),
        32,
        root.signal(),
    ));
    let app = app(ingest, &root);

    assert_eq!(
        app.clone().oneshot(request()).await?.status(),
        StatusCode::OK
    );
    assert_eq!(app.oneshot(request()).await?.status(), StatusCode::OK);

    let events = database.collection::<mongodb::bson::Document>("events");
    assert_eq!(events.count_documents(doc! {}).await?, 1);
    let document = events.find_one(doc! {}).await?.unwrap();
    let decoded = decode_pending_event(&document, codec)?;
    assert_eq!(decoded.project_id, ProjectId::new(42)?);
    assert_eq!(
        decoded.event_id.to_string(),
        "aa40a14691564910ae6eb2affdba35f9"
    );
    assert_eq!(handoff.0.lock().unwrap().len(), 1);

    root.begin();
    writer_task.wait().await;
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
    };
    http::router(
        root.signal(),
        Metrics,
        ingest_http::router(service, config, root.signal()),
    )
}

fn request() -> Request<Body> {
    let envelope = format!(
        "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{}",
        EVENT.len(),
        EVENT
    );
    Request::builder()
        .method("POST")
        .uri("/api/42/envelope/")
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
    let uri = std::env::var("FAULTKEEP_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://faultkeep:faultkeep-local-only@127.0.0.1:27018/?authSource=admin&retryWrites=false&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "faultkeep_phase3_e2e_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
