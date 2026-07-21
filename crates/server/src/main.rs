use std::process::ExitCode;

use clap::Parser;
use faultkeep_server::config::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    match faultkeep_server::execute(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("faultkeep startup failed: {error}");
            ExitCode::FAILURE
        }
    }
}
