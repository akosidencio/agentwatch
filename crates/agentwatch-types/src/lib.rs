//! Shared vocabulary types for AgentWatch.
//!
//! This crate is the root of the dependency graph and deliberately depends on
//! nothing but `serde`, `time`, and `uuid`. Anything that needs a runtime or
//! does real I/O belongs one layer up.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod ids;
mod paths;
mod repository;
mod timestamp;

pub use ids::{AgentId, EventId, ExternalSessionId, ProjectId, SessionId};
pub use paths::{DATA_DIR_ENV, PathError, Paths};
pub use repository::{RepositoryResolver, display_name};
pub use timestamp::Timestamp;
