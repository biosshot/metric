use std::process::ExitCode;

use clap::Parser;
use metric_server::config::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    match metric_server::execute(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("metric startup failed: {error}");
            ExitCode::FAILURE
        }
    }
}
