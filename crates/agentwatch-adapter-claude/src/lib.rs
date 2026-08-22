//! Claude Code integration.
//!
//! Turns hook payloads into normalized events, and drops everything that would
//! make AgentWatch a copy of the conversation.
//!
//! # What this adapter never keeps
//!
//! - Prompt text. Only a character count and a hash.
//! - Tool results. `tool_response` is not even deserialized, so file contents
//!   and command output never reach a Rust value, let alone the database.
//! - File contents from `Write` and `Edit` payloads.
//! - Conversation text from transcripts. `message.content` is read only through
//!   types that name a tool call's identity and arguments; the `text` field
//!   carrying prose and thinking is declared nowhere, so it is dropped during
//!   parsing rather than after it.
//!
//! Command lines *are* kept, because a command monitor that hides commands is
//! pointless. That is a deliberate exception; storage applies the shared
//! command redactor before either the raw event or query projection is written.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod hooks;
mod redact;
mod tools;
mod transcript;

pub use hooks::ClaudeAdapter;
pub use transcript::{
    TranscriptError, TranscriptSummary, derived_transcript_path, find_transcripts,
    read_token_usage, read_token_usage_from, read_transcript, read_transcript_from,
    transcript_root,
};

/// The `source` value the Claude hook binary puts in its envelopes.
pub const SOURCE: &str = "claude-code";
