//! Configuration and composition root for the single `all` role.

pub mod config;
pub mod http;

use std::{io, process::ExitCode};

use config::{Cli, ConfigError};
use faultkeep_application::{
    observability::{Metric, Metrics, Outcome},
    shutdown::ShutdownRoot,
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
        metrics,
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
