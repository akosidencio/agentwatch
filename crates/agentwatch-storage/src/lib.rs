//! SQLite persistence.
//!
//! One writer, many readers. The daemon owns a single [`Store`] on a dedicated
//! thread and writes in batches; the CLI opens its own read-only connection and
//! relies on WAL mode to avoid blocking the writer.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod migrations;
mod query;
mod repositories;
mod security;
mod sessions;
mod store;
mod tokens;

pub use query::{EventRow, Totals};
pub use repositories::BackfillReport;
pub use security::Notable;
pub use sessions::{ActivityFilter, Coverage, SessionRow};
pub use store::{Store, StoreError};
pub use tokens::{PendingSession, TokenGroup, TokenTotals};
