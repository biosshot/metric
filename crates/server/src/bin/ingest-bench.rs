//! Benchmark-only HTTP composition. It never provides production durability.

use std::{future::pending, sync::Arc, time::SystemTime};

use faultkeep_application::{
    ingest::IngestService, observability::Metrics, shutdown::ShutdownRoot,
};
use faultkeep_domain::{
    AcceptedEvent, DsnKey, IpScrubPolicy, ItemCapabilities, ProjectAcceptanceState, ProjectId,
    ProjectIngestLimits, ProjectKeyState, ProjectSnapshot, ScrubPolicy, SecretBytes, Timestamp,
};
use faultkeep_ports::{
    Clock, DurableOutcome, EventSink, EventSinkError, IngestOutcome, OutcomeSink, PortFuture,
    ProjectResolveError, ProjectResolver, RandomError, RandomSource,
};
use faultkeep_server::{config::IngestConfig, http, ingest_http};
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

struct BenchEventSink;

impl EventSink for BenchEventSink {
    fn persist(
        &self,
        _event: AcceptedEvent,
    ) -> PortFuture<'_, Result<DurableOutcome, EventSinkError>> {
        Box::pin(async { Ok(DurableOutcome::Accepted) })
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
async fn main() -> std::io::Result<()> {
    let address =
        std::env::var("FAULTKEEP_BENCH_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3100".to_owned());
    let root = ShutdownRoot::new();
    let config = benchmark_config();
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
        Arc::new(BenchEventSink),
        Arc::new(NoopOutcomeSink),
        Arc::new(BenchClock),
        Arc::new(BenchRandom),
        config.max_waiting_for_storage,
        root.signal(),
    ));
    let app = http::router(
        root.signal(),
        Metrics,
        ingest_http::router(service, config, root.signal()),
    );
    let listener = TcpListener::bind(&address).await?;
    println!("benchmark fake ingest listening on {address}");
    let server = http::run(
        listener,
        root.signal(),
        std::time::Duration::from_secs(5),
        app,
    );
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            root.begin();
            server.await
        }
        () = pending() => unreachable!(),
    }
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
