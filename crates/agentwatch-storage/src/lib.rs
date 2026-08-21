//! SQLite persistence.
//!
//! One writer, many readers. The daemon owns a single [`Store`] on a dedicated
//! thread and writes in batches; the CLI opens its own read-only connection and
//! relies on WAL mode to avoid blocking the writer.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod config_watch;
mod migrations;
mod query;
mod receipt;
mod repositories;
mod security;
mod sessions;
mod store;
mod tokens;

pub use config_watch::ConfigCheck;
pub use query::{EventRow, Totals};
pub use receipt::{ReceiptCommand, ReceiptFile, ReceiptTokenGroup};
pub use repositories::BackfillReport;
pub use security::Notable;
pub use sessions::{ActivityFilter, Coverage, SessionFilter, SessionRow};
pub use store::{Store, StoreError};
pub use tokens::{PendingSession, TokenGroup, TokenTotals};
