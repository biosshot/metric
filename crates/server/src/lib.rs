//! Configuration and composition root for the single `all` role.

pub mod config;
pub mod http;
pub mod ingest_http;

use std::{io, process::ExitCode};

use config::{Cli, ConfigError};
use faultkeep_application::{
    observability::{Metric, Metrics, Outcome},
    projects::{ProjectCacheConfig, ProjectService, ProjectServiceError},
    shutdown::ShutdownRoot,
};
use faultkeep_domain::Timestamp;
use faultkeep_mongo::{MongoBootstrapError, MongoProjectStore};
use faultkeep_ports::{
    Clock, EventSink, EventSinkError, OutcomeSink, PortFuture, ProjectResolveError,
    ProjectResolver, RandomError, RandomSource,
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::time::timeout;
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
    #[error(transparent)]
    Mongo(#[from] MongoBootstrapError),
    #[error(transparent)]
    Projects(#[from] ProjectServiceError),
    #[error("MongoDB schema bootstrap/check exceeded its deadline")]
    MongoBootstrapTimeout,
}

pub async fn execute(cli: Cli) -> Result<ExitCode, ServerError> {
    let config = config::load(&cli)?;
    let mut secrets = config.validate_secrets()?;
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
    init_tracing()?;
    let metrics = Metrics;
    let shutdown = ShutdownRoot::new();
    let clock: std::sync::Arc<dyn Clock> = std::sync::Arc::new(SystemClock);
    let random: std::sync::Arc<dyn RandomSource> = std::sync::Arc::new(SystemRandom);
    let project_resolver: std::sync::Arc<dyn ProjectResolver> =
        if let Some(uri) = secrets.mongodb_uri.take() {
            let hmac_key = secrets
                .scrub_hmac_key
                .take()
                .expect("validated MongoDB configuration has a scrub HMAC key");
            let setup = async {
                let store = MongoProjectStore::connect(
                    uri.expose(),
                    &config.mongodb.database,
                    hmac_key,
                    config.projects.max_keys_per_project,
                )
                .await?;
                store.bootstrap_or_validate().await?;
                Ok::<_, MongoBootstrapError>(store)
            };
            let store = timeout(config.mongodb.bootstrap_timeout.get(), setup)
                .await
                .map_err(|_| ServerError::MongoBootstrapTimeout)??;
            std::sync::Arc::new(ProjectService::new(
                std::sync::Arc::new(store),
                std::sync::Arc::clone(&clock),
                std::sync::Arc::clone(&random),
                config.projects.identity_collision_retries,
                ProjectCacheConfig {
                    capacity: config.ingest.project_cache.capacity,
                    max_inflight: config.ingest.project_cache.max_inflight,
                    positive_ttl: config.ingest.project_cache.positive_ttl.get(),
                    negative_ttl: config.ingest.project_cache.negative_ttl.get(),
                },
            )?)
        } else {
            std::sync::Arc::new(UnavailableProjectResolver)
        };
    let ingest = std::sync::Arc::new(faultkeep_application::ingest::IngestService::new(
        project_resolver,
        std::sync::Arc::new(UnavailableEventSink),
        std::sync::Arc::new(NoopOutcomeSink),
        clock,
        random,
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
    fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
        getrandom::fill(output).map_err(|_| RandomError)
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
