//! Durable telemetry adapter for OpenAI Codex rollout files.
//!
//! Rollouts are richer than the session index: they carry the model selected
//! for each turn, cumulative token counters, tool working directories, and
//! changed file paths. The adapter reads only that metadata. Prompt text,
//! assistant messages, command output, and patch bodies are not deserialized.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod rollout;

pub use rollout::{
    RolloutError, RolloutSummary, find_rollouts, read_rollout, read_rollout_from, rollout_root,
};
