//! Benchmark-only HTTP composition with real MongoDB durability.

use std::{sync::Arc, time::SystemTime};

use metric_application::{
    ingest::IngestService,
    log_writer::{LogWriter, LogWriterConfig},
    observability::Metrics,
    scheduler::{Scheduler, SchedulerConfig},
    shutdown::ShutdownRoot,
    span_writer::{SpanWriter, SpanWriterConfig},
    writer::{MongoWriter, MongoWriterConfig},
};
use metric_domain::{
    AcceptedEvent, DsnKey, IpScrubPolicy, ItemCapabilities, ProjectAcceptanceState, ProjectId,
    ProjectIngestLimits, ProjectKeyState, ProjectSnapshot, ScrubPolicy, SecretBytes, Timestamp,
};
use metric_mongo::{EventCodecConfig, MongoProjectStore};
use metric_ports::{
    AcceptedEventHandoff, Clock, IngestOutcome, OutcomeSink, PortFuture, ProjectResolveError,
    ProjectResolver, RandomError, RandomSource,
};
use metric_server::{config::IngestConfig, http, ingest_http};
use mongodb::{Client, bson::doc};
use tokio::net::TcpListener;

const KEY: &str = "0123456789abcdef0123456789abcdef";

struct BenchResolver(ProjectSnapshot);

impl ProjectResolver for BenchResolver {
    fn resolve(&self, key: DsnKey) -> PortFuture<'_, Result<ProjectSnapshot, ProjectResolveError>> {
        Box::pin(async move {
            if key == DsnKey::parse(KEY).expect("constant key is valid") {
                Ok(self.0.clone())
            } else {
                Err(ProjectResolveError::Unauthorized)
            }
        })
    }
}

struct DiscardHandoff;

impl AcceptedEventHandoff for DiscardHandoff {
    fn offer(&self, _event: AcceptedEvent) -> Result<(), AcceptedEvent> {
        Ok(())
    }
}

struct NoopOutcomeSink;

impl OutcomeSink for NoopOutcomeSink {
    fn record(&self, _outcome: IngestOutcome) {}
}

struct BenchClock;

impl Clock for BenchClock {
    fn now(&self) -> Timestamp {
        let millis = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("benchmark clock is after Unix epoch")
            .as_millis();
        Timestamp::from_unix_millis(i64::try_from(millis).expect("timestamp fits i64"))
            .expect("benchmark timestamp is supported")
    }
}

struct BenchRandom;

impl RandomSource for BenchRandom {
    fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
        output.fill(0x5a);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let uri = std::env::var("METRIC_BENCH_MONGODB_URI")?;
    let database_name = std::env::var("METRIC_BENCH_DATABASE")?;
    let address =
        std::env::var("METRIC_BENCH_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3101".to_owned());
    let client = Client::with_uri_str(&uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    let database = client.database(&database_name);
    let control =
        MongoProjectStore::from_database(database.clone(), SecretBytes::new([0x2a; 32]), 32);
    control.bootstrap_or_validate().await?;

    let root = ShutdownRoot::new();
    let config = benchmark_config();
    let event_store = Arc::new(control.event_store(EventCodecConfig::default()));
    let signal_store: Arc<dyn metric_ports::SignalStore> = Arc::new(
        control.signal_store_with_retention(metric_mongo::SignalRetention {
            logs_days: 30,
            spans_days: 30,
            span_stats_hourly_days: 90,
        }),
    );
    let clock: Arc<dyn Clock> = Arc::new(BenchClock);
    let (writer, writer_task) = MongoWriter::start(
        Arc::clone(&event_store),
        Arc::new(DiscardHandoff),
        MongoWriterConfig {
            channel_capacity: config.max_waiting_for_storage,
            max_wait: config.batch.max_wait.get(),
            max_documents: config.batch.max_documents,
            max_bytes: config.batch.max_bytes,
            operation_timeout: config.request_timeout.get(),
            shutdown_drain: std::time::Duration::from_secs(10),
        },
        root.signal(),
    )?;
    let (log_writer, log_writer_task) = LogWriter::start(
        Arc::clone(&signal_store),
        LogWriterConfig {
            channel_capacity: config.max_waiting_for_storage,
            max_wait: config.batch.max_wait.get(),
            max_documents: config.batch.max_documents,
            max_bytes: config.batch.max_bytes,
            operation_timeout: config.request_timeout.get(),
            shutdown_drain: std::time::Duration::from_secs(10),
        },
        root.signal(),
    )?;
    let (span_writer, span_writer_task) = SpanWriter::start(
        Arc::clone(&signal_store),
        SpanWriterConfig {
            channel_capacity: config.max_waiting_for_storage,
            max_wait: config.batch.max_wait.get(),
            max_documents: config.batch.max_documents,
            max_bytes: config.batch.max_bytes,
            operation_timeout: config.request_timeout.get(),
            shutdown_drain: std::time::Duration::from_secs(10),
        },
        root.signal(),
    )?;
    let snapshot = ProjectSnapshot {
        project_id: ProjectId::new(42).expect("constant project is valid"),
        state: ProjectAcceptanceState::Active,
        key_state: ProjectKeyState::Active,
        scrub_policy: ScrubPolicy {
            revision: 1,
            ip_policy: IpScrubPolicy::Hmac,
            hmac_key: SecretBytes::new([0x2a; 32]),
        },
        items: ItemCapabilities {
            error: true,
            client_report: true,
            log: true,
            transaction: true,
            span: true,
        },
        limits: ProjectIngestLimits::default(),
        grouping_revision: 1,
    };
    let service = Arc::new(
        IngestService::new(
            Arc::new(BenchResolver(snapshot)),
            writer,
            Arc::new(NoopOutcomeSink),
            Arc::clone(&clock),
            Arc::new(BenchRandom),
            config.max_waiting_for_storage,
            root.signal(),
        )
        .with_log_sink(log_writer)
        .with_span_sink(span_writer),
    );
    let app = http::router_with_readiness(
        root.signal(),
        Metrics,
        ingest_http::router(service, config, root.signal()),
        true,
    );
    let listener = TcpListener::bind(&address).await?;
    let scheduler_task = if std::env::var("METRIC_BENCH_MAINTENANCE").as_deref() == Ok("1") {
        let (_, task) = Scheduler::start(
            Arc::new(control.maintenance_store()),
            clock,
            SchedulerConfig {
                poll_interval: std::time::Duration::from_millis(20),
                maintenance_interval: std::time::Duration::from_millis(100),
                reconciliation_interval: std::time::Duration::from_secs(1),
                backlog_interval: std::time::Duration::from_millis(100),
                task_timeout: std::time::Duration::from_secs(5),
                retry_base: std::time::Duration::from_millis(100),
                retry_max: std::time::Duration::from_secs(5),
                batch_size: 500,
                ..SchedulerConfig::default()
            },
            root.signal(),
        )
        .await?;
        Some(task)
    } else {
        None
    };
    println!(
        "durable benchmark ingest listening on {address}; maintenance={}",
        scheduler_task.is_some()
    );
    let server = http::run(
        listener,
        root.signal(),
        std::time::Duration::from_secs(10),
        app,
    );
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result?,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            root.begin();
            server.await?;
        }
    }
    root.begin();
    if let Some(task) = scheduler_task {
        task.wait().await;
    }
    writer_task.wait().await;
    log_writer_task.wait().await;
    span_writer_task.wait().await;
    for collection in ["error_events", "logs", "spans"] {
        let count = database
            .collection::<mongodb::bson::Document>(collection)
            .count_documents(doc! {})
            .await?;
        println!("durable benchmark {collection} count: {count}");
    }
    Ok(())
}

fn benchmark_config() -> IngestConfig {
    IngestConfig {
        max_compressed_request_bytes: 20 * 1024 * 1024,
        max_decompressed_request_bytes: 100 * 1024 * 1024,
        max_event_bytes: 1024 * 1024,
        max_envelope_items: 100,
        max_active_requests: 4096,
        max_parsing_tasks: 0,
        max_waiting_for_storage: 4096,
        request_timeout: "10s".parse().expect("constant duration is valid"),
        unsupported_backoff_seconds: 3600,
        project_cache: Default::default(),
        batch: Default::default(),
        event_codec: Default::default(),
        backlog: Default::default(),
        attachments: Default::default(),
    }
}
