//! The normalized event schema every adapter maps into, plus the wire format
//! the hook binary uses to reach the daemon.
//!
//! Nothing in this crate knows about Claude Code, SQLite, or Tokio. That
//! separation is what makes a second adapter a week of work rather than a
//! rewrite.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod adapter;
mod command_scan;
mod event;
mod evidence;
mod redact;
mod sensitivity;
mod wire;

pub use adapter::{AdapterError, HookAdapter};
pub use command_scan::{PathReference, scan_command, worst_in_command};
pub use event::{
    AgentEvent, CollectionEvent, CommandEvent, ConfigChangedEvent, Event, FileEvent, McpEvent,
    PromptEvent, SessionEnded, SessionStarted, TokenUsageEvent, ToolCallEvent, UnknownEvent,
};
pub use evidence::{Confidence, EvidenceSource};
pub use redact::{CommandRedactor, RedactionPatternError, redact_command};
pub use sensitivity::{Sensitivity, classify};
pub use wire::{
    FrameError, HookEnvelope, HookEnvelopeRef, MAX_FRAME_BYTES, PROTOCOL_VERSION, decode_frame_len,
    encode_frame,
};
