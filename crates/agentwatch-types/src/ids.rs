//! Identifier newtypes.
//!
//! Every id is a distinct type so a session id can never be passed where a
//! project id is expected.

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Namespace for deterministic v5 ids derived from filesystem paths.
///
/// Fixed for the lifetime of the project: changing it re-keys every project row.
const PROJECT_NAMESPACE: Uuid = Uuid::from_u128(0x6167_656e_7477_6174_6368_5f70_726f_6a73);

/// Identifies an agent product, such as Claude Code.
///
/// Backed by [`Cow`] so built-in agents cost no allocation while adapters
/// resolved at runtime can still supply an owned name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(Cow<'static, str>);

impl AgentId {
    /// The Claude Code agent.
    pub const CLAUDE_CODE: Self = Self(Cow::Borrowed("claude-code"));

    /// Wraps a statically known agent name.
    #[must_use]
    pub const fn from_static(id: &'static str) -> Self {
        Self(Cow::Borrowed(id))
    }

    /// Borrows the underlying identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for AgentId {
    fn from(value: String) -> Self {
        Self(Cow::Owned(value))
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a fresh, time-ordered identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wraps an existing UUID.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

uuid_id! {
    /// Identifies a single stored event.
    EventId
}

uuid_id! {
    /// AgentWatch's own identifier for a session.
    ///
    /// Distinct from [`ExternalSessionId`], which is whatever the agent calls it.
    SessionId
}

uuid_id! {
    /// Identifies a project, derived deterministically from its root path.
    ProjectId
}

impl ProjectId {
    /// Derives a stable id from a project root path.
    ///
    /// Deterministic, so ingestion never needs a read-before-write to discover
    /// whether a project row already exists.
    #[must_use]
    pub fn from_path(path: &str) -> Self {
        Self(Uuid::new_v5(&PROJECT_NAMESPACE, path.as_bytes()))
    }
}

impl EventId {
    /// Derives a stable id from something that identifies the event uniquely.
    ///
    /// Used for events re-read from a durable source: a reconcile pass over the
    /// same transcript must produce the same ids, or every pass would append a
    /// fresh copy of history.
    #[must_use]
    pub fn from_key(agent: &AgentId, key: &str) -> Self {
        let mut namespaced = String::with_capacity(agent.as_str().len() + key.len() + 1);
        namespaced.push_str(agent.as_str());
        namespaced.push('\u{1f}');
        namespaced.push_str(key);
        Self(Uuid::new_v5(&PROJECT_NAMESPACE, namespaced.as_bytes()))
    }
}

impl SessionId {
    /// Derives a stable internal id from the agent's own session identifier.
    ///
    /// Deterministic for the same reason [`ProjectId::from_path`] is: ingestion
    /// can attribute an event to a session without consulting the database.
    #[must_use]
    pub fn from_external(agent: &AgentId, external: &ExternalSessionId) -> Self {
        let mut key = String::with_capacity(agent.as_str().len() + external.as_str().len() + 1);
        key.push_str(agent.as_str());
        key.push('\u{1f}');
        key.push_str(external.as_str());
        Self(Uuid::new_v5(&PROJECT_NAMESPACE, key.as_bytes()))
    }
}

/// The session identifier as reported by the agent itself.
///
/// Opaque: Claude Code uses a UUID string today, but nothing here depends on that.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExternalSessionId(String);

impl ExternalSessionId {
    /// Borrows the underlying identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ExternalSessionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for ExternalSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_is_stable_across_calls() {
        let first = ProjectId::from_path("/Users/dev/projects/acme-api");
        let second = ProjectId::from_path("/Users/dev/projects/acme-api");
        assert_eq!(first, second);
    }

    #[test]
    fn project_id_differs_between_paths() {
        let api = ProjectId::from_path("/Users/dev/projects/acme-api");
        let store = ProjectId::from_path("/Users/dev/projects/storefront");
        assert_ne!(api, store);
    }

    #[test]
    fn a_keyed_event_id_is_stable_across_passes() {
        let first = EventId::from_key(&AgentId::CLAUDE_CODE, "msg_abc");
        let second = EventId::from_key(&AgentId::CLAUDE_CODE, "msg_abc");
        assert_eq!(first, second);
    }

    #[test]
    fn keyed_event_ids_differ_per_key() {
        let first = EventId::from_key(&AgentId::CLAUDE_CODE, "msg_abc");
        let second = EventId::from_key(&AgentId::CLAUDE_CODE, "msg_xyz");
        assert_ne!(first, second);
    }

    #[test]
    fn session_id_is_derived_deterministically() {
        let external = ExternalSessionId::from("abc-123".to_owned());
        let first = SessionId::from_external(&AgentId::CLAUDE_CODE, &external);
        let second = SessionId::from_external(&AgentId::CLAUDE_CODE, &external);
        assert_eq!(first, second);
    }

    #[test]
    fn session_id_is_namespaced_by_agent() {
        let external = ExternalSessionId::from("abc-123".to_owned());
        let claude = SessionId::from_external(&AgentId::CLAUDE_CODE, &external);
        let other = SessionId::from_external(&AgentId::from_static("codex"), &external);
        assert_ne!(claude, other);
    }

    #[test]
    fn event_ids_sort_by_creation_time() {
        let first = EventId::new();
        let second = EventId::new();
        assert!(first < second, "v7 ids should sort by creation time");
    }
}
