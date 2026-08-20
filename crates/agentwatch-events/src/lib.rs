//! The normalized event schema every adapter maps into, plus the wire format
//! the hook binary uses to reach the daemon.
//!
//! Nothing in this crate knows about Claude Code, SQLite, or Tokio. That
//! separation is what makes a second adapter a week of work rather than a
//! rewrite.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod adapter;
mod event;
mod evidence;
mod wire;

pub use adapter::{AdapterError, HookAdapter};
pub use event::{
    AgentEvent, CommandEvent, Event, FileEvent, McpEvent, PromptEvent, SessionEnded,
    SessionStarted, TokenUsageEvent, ToolCallEvent, UnknownEvent,
};
pub use evidence::{Confidence, EvidenceSource};
pub use wire::{
    FrameError, HookEnvelope, HookEnvelopeRef, MAX_FRAME_BYTES, PROTOCOL_VERSION, decode_frame_len,
    encode_frame,
};
