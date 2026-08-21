//! The AgentWatch daemon.
//!
//! ```text
//! socket ──► ingest ──► mpsc(bounded) ──► batcher ──► writer thread ──► sqlite
//! ```
//!
//! Every stage is bounded. If storage falls behind, the bounded channel fills,
//! the ingest task's send blocks, and the connection stops being read — which
//! is backpressure applied where it is harmless, since the hook has already
//! exited by then and no agent is waiting on us.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod batcher;
mod config_watch;
mod pause;
mod pipeline;
mod reconcile;
mod registry;
mod server;

pub use pipeline::{Daemon, DaemonConfig};
pub use reconcile::{ReconcileReport, reconcile_session, sweep};
pub use registry::AdapterRegistry;

/// Runs the daemon until it is asked to stop.
///
/// Owns the logging setup and the async runtime so that `agentwatch daemon` is
/// a single call. The runtime is built here rather than by the caller because
/// nothing else in the CLI is async: a `#[tokio::main]` on the whole binary
/// would start a thread pool for `agentwatch tokens`.
///
/// # Errors
///
/// Returns an error if the configuration cannot be resolved, the runtime cannot
/// be built, or the daemon itself fails.
pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("AGENTWATCH_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let config = DaemonConfig::from_env()?;
    tokio::runtime::Runtime::new()
        .map_err(anyhow::Error::from)?
        .block_on(async { Daemon::new(config).run().await })
}
