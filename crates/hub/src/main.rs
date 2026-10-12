use std::process::ExitCode;

use clap::Parser;
use disktree_hub::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("DISKTREE_HUB_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = Config::parse();
    match disktree_hub::run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err}");
            ExitCode::FAILURE
        }
    }
}
