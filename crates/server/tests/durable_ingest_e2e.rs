use std::{
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use faultkeep_application::{
    dispatcher::{Dispatcher, DispatcherConfig},
    finalizer::{Finalizer, FinalizerConfig},
    ingest::IngestService,
    normalizer::{Normalizer, NormalizerLimits},
    observability::Metrics,
    processor::{
        FinalizerBatchConfig, FinalizerBatcher, GrouperStage, IssuePreparerStage, Processor,
        ProcessorConfig, ProcessorOutcome, StageFailure, SymbolicationStage,
    },
    projects::{ProjectCacheConfig, ProjectService},
    shutdown::ShutdownRoot,
    symbolication::BaselineSymbolicationService,
    writer::{MongoWriter, MongoWriterConfig},
};
use faultkeep_domain::{
    AcceptedEvent, DisplayName, DsnKey, EventId, EventKey, IpScrubPolicy, ItemCapabilities,
    OrganizationId, OrganizationIdentity, ProjectAcceptanceState, ProjectId, ProjectIdentity,
    ProjectIngestLimits, ProjectKeyIdentity, ProjectKeyLabel, ProjectKeyState, SecretBytes, Slug,
    Timestamp,
    event::NormalizedEvent,
    processing::{PendingEvent, ProcessingErrorCode},
    symbolication::SymbolicationResult,
};
use faultkeep_mongo::{
    EventCodecConfig, IssueCodecConfig, MongoProjectStore, decode_pending_event,
};
use faultkeep_ports::{
    AcceptedEventHandoff, DurableOutcome, EventBacklog, EventPrepareError, EventSink, EventStore,
    EventStoreError, EventWriteStatus, PortFuture, ProjectResolver, ProjectStore, WorkHandler,
};
use faultkeep_server::{config::IngestConfig, http, ingest_http};
use faultkeep_testkit::{FakeOutcomeSink, FixedClock, FixedRandom};
use mongodb::{Client, Database, bson::doc};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
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

struct CompletingWorkHandler {
    database: Database,
    handled: Mutex<Vec<EventKey>>,
    started: Notify,
    release: Notify,
}

struct RetryOnceSymbolicator(AtomicUsize);

impl SymbolicationStage for RetryOnceSymbolicator {
    fn symbolicate<'a>(
        &'a self,
        event: &'a NormalizedEvent,
        _debug_file_revision: u64,
        _artifact_revision: u64,
        _cancellation: &'a CancellationToken,
    ) -> PortFuture<'a, Result<SymbolicationResult, StageFailure>> {
        Box::pin(async move {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(StageFailure::temporary(
                    ProcessingErrorCode::SymbolicationRetryable,
                ))
            } else {
                Ok(BaselineSymbolicationService::symbolicate(event))
            }
        })
    }
}

impl WorkHandler for CompletingWorkHandler {
    fn handle(&self, event: faultkeep_domain::processing::PendingEvent) -> PortFuture<'_, ()> {
        Box::pin(async move {
            let key = event.key();
            self.handled.lock().unwrap().push(key);
            self.started.notify_one();
            self.release.notified().await;
            self.database
                .collection::<mongodb::bson::Document>("error_events")
                .update_one(
                    doc! { "_id": mongodb::bson::Binary {
                        subtype: mongodb::bson::spec::BinarySubtype::Generic,
                        bytes: key.as_bytes().to_vec(),
                    }},
                    doc! { "$set": { "q": { "s": 1_i32, "a": 1_i32, "c": 1_i32 } } },
                )
                .await
                .unwrap();
        })
    }
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
async fn infrastructure_official_sdk_http_to_dispatcher_work_handler() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_official_sdk_to_processor_issue_and_hourly_stats() {
    let database = test_database().await.unwrap();
    let result = exercise_processor_e2e(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_processor_retry_restart_fences_and_terminal_failure() {
    let database = test_database().await.unwrap();
    let result = exercise_processor_recovery(&database).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "performance baseline requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn performance_processor_recovery_rps() {
    let database = test_database().await.unwrap();
    let result = measure_processor_recovery(&database).await;
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
            .collection::<mongodb::bson::Document>("error_events")
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

async fn measure_processor_recovery(database: &Database) -> Result<(), Box<dyn Error>> {
    const EVENTS: u32 = 1_000;
    const CONCURRENCY: usize = 256;
    const ACCEPTED_STEADY_RPS: f64 = 1_158.0;
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    seed(&control).await?;
    let codec = EventCodecConfig::default();
    let event_store = Arc::new(control.event_store(codec));
    let source = (0..EVENTS)
        .map(processor_performance_event)
        .collect::<Vec<_>>();
    for chunk in source.chunks(250) {
        let prepared = chunk
            .iter()
            .cloned()
            .map(|event| event_store.prepare(event))
            .collect::<Result<Vec<_>, _>>()?;
        assert!(
            event_store
                .insert_batch(&prepared)
                .await?
                .into_iter()
                .all(|status| status == EventWriteStatus::Inserted)
        );
    }
    let (batcher, batch_task) = FinalizerBatcher::start(
        Arc::new(Finalizer::new(
            Arc::new(control.finalization_store(codec, IssueCodecConfig::default())),
            FinalizerConfig {
                max_batch_events: 256,
                ..FinalizerConfig::default()
            },
        )?),
        FinalizerBatchConfig {
            channel_capacity: EVENTS as usize,
            max_wait: Duration::from_millis(2),
            max_events: 256,
            shutdown_drain: Duration::from_secs(10),
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
        Arc::new(FixedClock(Timestamp::from_unix_millis(10_000)?)),
        ProcessorConfig {
            max_concurrency: CONCURRENCY,
            stage_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(30),
            state_timeout: Duration::from_secs(5),
            ..ProcessorConfig::default()
        },
    )?);
    let pending = event_store
        .load_due(Timestamp::from_unix_millis(10_000)?, EVENTS as usize, &[])
        .await?;
    assert_eq!(pending.len(), EVENTS as usize);
    let started = Instant::now();
    let mut tasks = Vec::with_capacity(pending.len());
    for event in pending {
        let processor = processor.clone();
        tasks.push(tokio::spawn(async move { processor.process(event).await }));
    }
    for task in tasks {
        assert_eq!(task.await?, ProcessorOutcome::Processed);
    }
    let elapsed = started.elapsed();
    batcher.close();
    batch_task.wait().await;
    let rps = f64::from(EVENTS) / elapsed.as_secs_f64();
    let ratio = rps / ACCEPTED_STEADY_RPS;
    eprintln!(
        "Processor Phase 10: recovery_rps={rps:.0},events={EVENTS},concurrency={CONCURRENCY},accepted_steady_rps={ACCEPTED_STEADY_RPS:.0},recovery_ratio={ratio:.2},elapsed_ms={}",
        elapsed.as_millis()
    );
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("error_events")
            .count_documents(doc! { "q.s": 0_i32 })
            .await?,
        0
    );
    assert!(
        ratio >= 1.5,
        "Processor recovery ratio {ratio:.2} is below ADR-0037 gate"
    );
    Ok(())
}

fn processor_performance_event(index: u32) -> AcceptedEvent {
    AcceptedEvent {
        project_id: ProjectId::new(42).unwrap(),
        event_id: EventId::from_bytes(u128::from(index + 1_000_000).to_be_bytes()),
        received_at: Timestamp::from_unix_millis(5_000 + i64::from(index)).unwrap(),
        policy_revision: 1,
        payload: faultkeep_domain::ScrubbedEventPayload::new(
            format!(
                r#"{{"event_id":"{}","platform":"rust","level":"error","message":"shared processor recovery fixture"}}"#,
                hex::encode(u128::from(index + 1_000_000).to_be_bytes())
            )
            .into_bytes(),
        ),
    }
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
    let handler = Arc::new(CompletingWorkHandler {
        database: database.clone(),
        handled: Mutex::new(Vec::new()),
        started: Notify::new(),
        release: Notify::new(),
    });
    let backlog: Arc<dyn EventBacklog> = event_store.clone();
    let (dispatcher, dispatcher_task) = Dispatcher::start(
        backlog,
        handler.clone(),
        Arc::new(FixedClock(Timestamp::from_unix_millis(2_000)?)),
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
            max_oldest_pending_age: Some(Duration::from_secs(60 * 60)),
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
    tokio::time::timeout(Duration::from_secs(2), handler.started.notified()).await?;
    assert_eq!(app.oneshot(request()).await?.status(), StatusCode::OK);

    let events = database.collection::<mongodb::bson::Document>("error_events");
    assert_eq!(events.count_documents(doc! {}).await?, 1);
    let document = events.find_one(doc! {}).await?.unwrap();
    let decoded = decode_pending_event(&document, codec)?;
    assert_eq!(decoded.project_id, ProjectId::new(42)?);
    assert_eq!(
        decoded.event_id.to_string(),
        "aa40a14691564910ae6eb2affdba35f9"
    );
    assert_eq!(handler.handled.lock().unwrap().len(), 1);
    handler.release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let document = events.find_one(doc! {}).await.unwrap().unwrap();
            if document.get_document("q").unwrap().get_i32("s") == Ok(1) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;

    root.begin();
    writer_task.wait().await;
    dispatcher_task.wait().await;
    Ok(())
}

async fn exercise_processor_e2e(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    seed(&control).await?;
    let root = ShutdownRoot::new();
    let clock: Arc<dyn faultkeep_ports::Clock> =
        Arc::new(FixedClock(Timestamp::from_unix_millis(2_000)?));
    let projects = Arc::new(ProjectService::new(
        Arc::new(control.clone()),
        clock.clone(),
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
    let (finalizer_batcher, finalizer_batch_task) = FinalizerBatcher::start(
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
        finalizer_batcher.clone(),
        clock.clone(),
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
        clock.clone(),
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
            max_oldest_pending_age: Some(Duration::from_secs(60 * 60)),
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
    let ingest = Arc::new(IngestService::new(
        projects,
        writer,
        Arc::new(FakeOutcomeSink::default()),
        clock,
        Arc::new(FixedRandom(9)),
        32,
        root.signal(),
    ));
    let app = app(ingest, &root);
    assert_eq!(
        app.clone().oneshot(request()).await?.status(),
        StatusCode::OK
    );
    let events = database.collection::<mongodb::bson::Document>("error_events");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if events
                .find_one(doc! {})
                .await
                .unwrap()
                .is_some_and(|event| !event.contains_key("q"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("issues")
            .count_documents(doc! {})
            .await?,
        1
    );
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("issue_stats_hourly")
            .count_documents(doc! {})
            .await?,
        1
    );
    assert_eq!(app.oneshot(request()).await?.status(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(events.count_documents(doc! {}).await?, 1);
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("issues")
            .find_one(doc! {})
            .await?
            .unwrap()
            .get_i64("c"),
        Ok(1)
    );
    root.begin();
    writer_task.wait().await;
    dispatcher_task.wait().await;
    finalizer_batcher.close();
    finalizer_batch_task.wait().await;
    Ok(())
}

async fn exercise_processor_recovery(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    seed(&control).await?;
    let codec = EventCodecConfig::default();
    let event_store = Arc::new(control.event_store(codec));
    let retry_event = performance_event(100);
    let mut invalid_event = performance_event(101);
    invalid_event.payload = faultkeep_domain::ScrubbedEventPayload::new(
        format!(
            r#"{{"event_id":"{}","platform":"rust","release":"{}","message":"invalid identity bound"}}"#,
            invalid_event.event_id,
            "r".repeat(201)
        )
        .into_bytes(),
    );
    let fenced_event = performance_event(102);
    let prepared = [&retry_event, &invalid_event, &fenced_event]
        .into_iter()
        .map(|event| event_store.prepare(event.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        event_store
            .insert_batch(&prepared)
            .await?
            .into_iter()
            .all(|status| status == EventWriteStatus::Inserted)
    );

    let symbolicator = Arc::new(RetryOnceSymbolicator(AtomicUsize::new(0)));
    let processor = processor_for_test(
        &control,
        event_store.clone(),
        Arc::new(FixedClock(Timestamp::from_unix_millis(2_000)?)),
        symbolicator.clone(),
        codec,
    )?;
    assert_eq!(
        processor
            .process(PendingEvent::fresh(retry_event.clone()))
            .await,
        ProcessorOutcome::RetryScheduled
    );
    let events = database.collection::<mongodb::bson::Document>("error_events");
    let retried = events
        .find_one(doc! { "_id": event_binary(&retry_event) })
        .await?
        .unwrap();
    assert_eq!(retried.get_document("q")?.get_i32("a"), Ok(1));
    assert_eq!(retried.get_document("q")?.get_i32("s"), Ok(0));

    let due = event_store
        .load_due(Timestamp::from_unix_millis(3_001)?, 10, &[])
        .await?;
    let recovered = due
        .into_iter()
        .find(|pending| pending.event.event_id == retry_event.event_id)
        .ok_or("retry was not recovered after restart deadline")?;
    assert_eq!(recovered.attempts, 1);
    let restarted = processor_for_test(
        &control,
        event_store.clone(),
        Arc::new(FixedClock(Timestamp::from_unix_millis(3_001)?)),
        symbolicator,
        codec,
    )?;
    assert_eq!(
        restarted.process(recovered).await,
        ProcessorOutcome::Processed
    );

    assert_eq!(
        restarted
            .process(PendingEvent::fresh(invalid_event.clone()))
            .await,
        ProcessorOutcome::PermanentlyFailed
    );
    control
        .set_project_acceptance(ProjectId::new(42)?, ProjectAcceptanceState::Disabled)
        .await?;
    assert_eq!(
        restarted
            .process(PendingEvent::fresh(fenced_event.clone()))
            .await,
        ProcessorOutcome::PermanentlyFailed
    );
    assert_eq!(events.count_documents(doc! { "q.s": 0_i32 }).await?, 0);
    assert!(
        events
            .find_one(doc! { "_id": event_binary(&retry_event) })
            .await?
            .is_some_and(|event| !event.contains_key("q"))
    );
    for event in [&invalid_event, &fenced_event] {
        let document = events
            .find_one(doc! { "_id": event_binary(event) })
            .await?
            .unwrap();
        assert_eq!(document.get_document("q")?.get_i32("s"), Ok(1));
        assert!(!document.get_document("q")?.contains_key("n"));
    }
    Ok(())
}

fn processor_for_test(
    control: &MongoProjectStore,
    event_store: Arc<faultkeep_mongo::MongoEventStore>,
    clock: Arc<dyn faultkeep_ports::Clock>,
    symbolicator: Arc<dyn SymbolicationStage>,
    codec: EventCodecConfig,
) -> Result<Processor, Box<dyn Error>> {
    Ok(Processor::new(
        event_store.clone(),
        event_store,
        Arc::new(Normalizer::new(NormalizerLimits::default())?),
        symbolicator,
        Arc::new(GrouperStage),
        Arc::new(IssuePreparerStage),
        Arc::new(Finalizer::new(
            Arc::new(control.finalization_store(codec, IssueCodecConfig::default())),
            FinalizerConfig::default(),
        )?),
        clock,
        ProcessorConfig {
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(1),
            stage_timeout: Duration::from_secs(2),
            total_timeout: Duration::from_secs(5),
            state_timeout: Duration::from_secs(2),
            ..ProcessorConfig::default()
        },
    )?)
}

fn event_binary(event: &AcceptedEvent) -> mongodb::bson::Binary {
    mongodb::bson::Binary {
        subtype: mongodb::bson::spec::BinarySubtype::Generic,
        bytes: EventKey::new(event.project_id, event.event_id)
            .as_bytes()
            .to_vec(),
    }
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
                log: true,
                transaction: true,
                span: true,
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
