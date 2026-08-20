//! The seam every agent integration goes through.
//!
//! There is exactly one implementation today. The trait exists anyway: the
//! moment a second agent arrives, the daemon must not need to change.

use crate::event::AgentEvent;
use crate::wire::HookEnvelope;

/// An envelope that could not be turned into an event.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AdapterError {
    /// The payload did not parse as this adapter expects.
    #[error("could not parse the agent payload")]
    Payload(#[from] serde_json::Error),
    /// The envelope named a source no adapter is registered for.
    ///
    /// The field is `declared` rather than `source` because thiserror reads a
    /// field of that name as the underlying cause of the error.
    #[error("no adapter registered for source `{declared}`")]
    UnknownSource {
        /// The source the hook declared.
        declared: String,
    },
    /// The envelope used a protocol version this build does not understand.
    #[error("unsupported protocol version {version}; this build speaks {expected}")]
    UnsupportedProtocol {
        /// What the hook sent.
        version: u16,
        /// What this build expects.
        expected: u16,
    },
}

/// Translates one agent's hook payloads into normalized events.
///
/// Adapters translate. They do not aggregate, classify, or decide what to
/// store — that belongs downstream, where the rules are agent-independent.
pub trait HookAdapter: Send + Sync {
    /// The `source` value this adapter claims from an envelope.
    fn source(&self) -> &'static str;

    /// Turns one envelope into one event.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload cannot be parsed. A payload this adapter
    /// does not recognize is not an error: it becomes an `unknown` event, so a
    /// new agent feature shows up as a visible row rather than silently
    /// disappearing.
    fn normalize(&self, envelope: &HookEnvelope) -> Result<AgentEvent, AdapterError>;
}
