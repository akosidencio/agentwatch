//! Daemon entry point.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

use agentwatch_daemon::{Daemon, DaemonConfig};
use anyhow::Result;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("AGENTWATCH_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    Daemon::new(DaemonConfig::from_env()?).run().await
}
