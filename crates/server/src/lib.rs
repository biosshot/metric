//! Configuration and composition root for the single `all` role.

pub mod config;
pub mod http;
pub mod ingest_http;

use std::{io, process::ExitCode};

use config::{Cli, ConfigError};
use faultkeep_application::{
    observability::{Metric, Metrics, Outcome},
    shutdown::ShutdownRoot,
};
use faultkeep_domain::Timestamp;
use faultkeep_ports::{
    Clock, EventSink, EventSinkError, OutcomeSink, PortFuture, ProjectResolveError,
    ProjectResolver, RandomSource,
};
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("HTTP server failed: {0}")]
    Http(#[from] io::Error),
    #[error("structured tracing could not be initialized")]
    Tracing,
}

pub async fn execute(cli: Cli) -> Result<ExitCode, ServerError> {
    let config = config::load(&cli)?;
    let resolved_mongodb_uri = config.validate_secrets()?;
    if config.has_literal_secret_warning() {
        eprintln!("warning: a literal secret is enabled for local development");
    }
    if cli.check_config {
        println!("configuration is valid");
        return Ok(ExitCode::SUCCESS);
    }
    if cli.print_effective_config {
        print!("{}", config.effective_redacted());
        return Ok(ExitCode::SUCCESS);
    }
    drop(resolved_mongodb_uri);

    init_tracing()?;
    let metrics = Metrics;
    let shutdown = ShutdownRoot::new();
    let ingest = std::sync::Arc::new(faultkeep_application::ingest::IngestService::new(
        std::sync::Arc::new(UnavailableProjectResolver),
        std::sync::Arc::new(UnavailableEventSink),
        std::sync::Arc::new(NoopOutcomeSink),
        std::sync::Arc::new(SystemClock),
        std::sync::Arc::new(SystemRandom),
        config.ingest.max_waiting_for_storage,
        shutdown.signal(),
    ));
    let app = http::router(
        shutdown.signal(),
        metrics,
        ingest_http::router(ingest, config.ingest.clone(), shutdown.signal()),
    );
    let listener = TcpListener::bind(config.server.http_address).await?;
    info!(
        operation = "runtime.ready",
        role = %config.role,
        address = %config.server.http_address,
        "HTTP listener ready"
    );

    let server = http::run(
        listener,
        shutdown.signal(),
        config.server.shutdown_grace.get(),
        app,
    );
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result?,
        () = wait_for_os_shutdown() => {
            warn!(operation = "runtime.shutdown", "shutdown signal received");
            shutdown.begin();
            server.await?;
        }
    }
    metrics.increment(Metric::Shutdowns, Outcome::Ok);
    info!(operation = "runtime.stopped", "graceful shutdown complete");
    Ok(ExitCode::SUCCESS)
}

struct UnavailableProjectResolver;

impl ProjectResolver for UnavailableProjectResolver {
    fn resolve(
        &self,
        _key: faultkeep_domain::DsnKey,
    ) -> PortFuture<'_, Result<faultkeep_domain::ProjectSnapshot, ProjectResolveError>> {
        Box::pin(async { Err(ProjectResolveError::Unavailable) })
    }
}

struct UnavailableEventSink;

impl EventSink for UnavailableEventSink {
    fn persist(
        &self,
        _event: faultkeep_domain::AcceptedEvent,
    ) -> PortFuture<'_, Result<faultkeep_ports::DurableOutcome, EventSinkError>> {
        Box::pin(async { Err(EventSinkError::Unavailable) })
    }
}

struct NoopOutcomeSink;

impl OutcomeSink for NoopOutcomeSink {
    fn record(&self, _outcome: faultkeep_ports::IngestOutcome) {}
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Timestamp::from_unix_millis(i64::try_from(millis).unwrap_or(i64::MAX))
            .expect("current system time is in the supported range")
    }
}

struct SystemRandom;

impl RandomSource for SystemRandom {
    fn fill_bytes(&self, output: &mut [u8]) {
        for chunk in output.chunks_mut(16) {
            let bytes = uuid::Uuid::new_v4().into_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

fn init_tracing() -> Result<(), ServerError> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init()
        .map_err(|_| ServerError::Tracing)
}

#[cfg(unix)]
async fn wait_for_os_shutdown() {
    use tokio::signal::unix::{SignalKind, signal};

    let terminate = signal(SignalKind::terminate());
    match terminate {
        Ok(mut terminate) => {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if result.is_err() {
                        error!(operation = "runtime.signal", error_code = "signal_handler_failed", "signal handler failed");
                    }
                }
                _ = terminate.recv() => {}
            }
        }
        Err(_) => {
            error!(
                operation = "runtime.signal",
                error_code = "signal_handler_failed",
                "SIGTERM handler failed"
            );
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(not(unix))]
async fn wait_for_os_shutdown() {
    if tokio::signal::ctrl_c().await.is_err() {
        error!(
            operation = "runtime.signal",
            error_code = "signal_handler_failed",
            "signal handler failed"
        );
    }
}

#[cfg(test)]
mod production_fence_tests {
    use super::*;

    #[tokio::test]
    async fn production_composition_has_no_fake_project_or_durable_success() {
        let key = faultkeep_domain::DsnKey::parse("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(
            UnavailableProjectResolver.resolve(key).await,
            Err(ProjectResolveError::Unavailable)
        );
    }
}
