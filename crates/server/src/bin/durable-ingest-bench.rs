//! Benchmark-only HTTP composition with real MongoDB durability.

use std::{sync::Arc, time::SystemTime};

use faultkeep_application::{
    ingest::IngestService,
    observability::Metrics,
    shutdown::ShutdownRoot,
    writer::{MongoWriter, MongoWriterConfig},
};
use faultkeep_domain::{
    AcceptedEvent, DsnKey, IpScrubPolicy, ItemCapabilities, ProjectAcceptanceState, ProjectId,
    ProjectIngestLimits, ProjectKeyState, ProjectSnapshot, ScrubPolicy, SecretBytes, Timestamp,
};
use faultkeep_mongo::{EventCodecConfig, MongoProjectStore};
use faultkeep_ports::{
    AcceptedEventHandoff, Clock, IngestOutcome, OutcomeSink, PortFuture, ProjectResolveError,
    ProjectResolver, RandomError, RandomSource,
};
use faultkeep_server::{config::IngestConfig, http, ingest_http};
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
    let uri = std::env::var("FAULTKEEP_BENCH_MONGODB_URI")?;
    let database_name = std::env::var("FAULTKEEP_BENCH_DATABASE")?;
    let address =
        std::env::var("FAULTKEEP_BENCH_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3101".to_owned());
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
    let (writer, writer_task) = MongoWriter::start(
        event_store,
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
        },
        limits: ProjectIngestLimits::default(),
        grouping_revision: 1,
    };
    let service = Arc::new(IngestService::new(
        Arc::new(BenchResolver(snapshot)),
        writer,
        Arc::new(NoopOutcomeSink),
        Arc::new(BenchClock),
        Arc::new(BenchRandom),
        config.max_waiting_for_storage,
        root.signal(),
    ));
    let app = http::router_with_readiness(
        root.signal(),
        Metrics,
        ingest_http::router(service, config, root.signal()),
        true,
    );
    let listener = TcpListener::bind(&address).await?;
    println!("durable benchmark ingest listening on {address}");
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
    writer_task.wait().await;
    let count = database
        .collection::<mongodb::bson::Document>("events")
        .count_documents(doc! {})
        .await?;
    println!("durable benchmark Event count: {count}");
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
    }
}
