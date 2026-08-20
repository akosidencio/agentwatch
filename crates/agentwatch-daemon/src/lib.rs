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
mod pipeline;
mod reconcile;
mod registry;
mod server;

pub use pipeline::{Daemon, DaemonConfig};
pub use reconcile::{ReconcileReport, reconcile_session, sweep};
pub use registry::AdapterRegistry;
