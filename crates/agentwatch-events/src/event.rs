//! The normalized event.

use agentwatch_types::{AgentId, EventId, ExternalSessionId, ProjectId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::evidence::{Confidence, EvidenceSource};

/// One observed action, normalized out of whatever the agent reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AgentEvent {
    /// Unique, time-ordered identifier.
    pub id: EventId,
    /// When the action happened, as reported by the agent where possible.
    pub timestamp: Timestamp,
    /// Which agent product performed it.
    pub agent_id: AgentId,
    /// AgentWatch's session identifier, derived from `external_session_id`.
    pub session_id: Option<SessionId>,
    /// The agent's own session identifier, kept for cross-referencing.
    pub external_session_id: Option<ExternalSessionId>,
    /// Project the session was working in, derived from its working directory.
    pub project_id: Option<ProjectId>,
    /// The working directory itself.
    pub project_path: Option<String>,
    /// Git branch the session was on, when the agent reports one.
    ///
    /// Session context rather than event detail, but it travels on the event
    /// because that is the only thing an adapter emits. Storage folds it onto
    /// the session row.
    pub git_branch: Option<String>,
    /// How this event was obtained.
    pub evidence: EvidenceSource,
    /// How much to trust it.
    pub confidence: Confidence,
    /// What actually happened.
    pub event: Event,
}

impl AgentEvent {
    /// Starts a directly observed event with generated id and current time.
    #[must_use]
    pub fn observed(agent_id: AgentId, evidence: EvidenceSource, event: Event) -> Self {
        Self {
            id: EventId::new(),
            timestamp: Timestamp::now(),
            agent_id,
            session_id: None,
            external_session_id: None,
            project_id: None,
            project_path: None,
            git_branch: None,
            evidence,
            confidence: Confidence::CERTAIN,
            event,
        }
    }

    /// Attaches session identity, deriving the internal id from the external one.
    #[must_use]
    pub fn with_session(mut self, external: ExternalSessionId) -> Self {
        self.session_id = Some(SessionId::from_external(&self.agent_id, &external));
        self.external_session_id = Some(external);
        self
    }

    /// Attaches a project, deriving its id from the path.
    #[must_use]
    pub fn with_project_path(mut self, path: String) -> Self {
        self.project_id = Some(ProjectId::from_path(&path));
        self.project_path = Some(path);
        self
    }

    /// Replaces the generated id with a deterministic one.
    ///
    /// For events re-read from a durable source, so that reading the same
    /// record twice produces the same row rather than a second copy.
    #[must_use]
    pub const fn with_id(mut self, id: EventId) -> Self {
        self.id = id;
        self
    }

    /// Attaches the git branch the session was on.
    #[must_use]
    pub fn with_git_branch(mut self, branch: Option<String>) -> Self {
        self.git_branch = branch;
        self
    }

    /// Overrides the timestamp.
    #[must_use]
    pub const fn at(mut self, timestamp: Timestamp) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// The stable kind string stored in the database.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        self.event.kind()
    }
}

/// What happened.
///
/// The payload structs below are intentionally *not* `#[non_exhaustive]`:
/// adapters in other crates construct them, and the marker would make that
/// impossible. Adding a field to one is a breaking change for adapters, which
/// is the right amount of friction given they all live in this workspace.
///
/// Deliberately small for phase 1. Token usage, process, network, permission,
/// and security variants arrive with the phases that can actually populate them.
///
/// Not `#[non_exhaustive]`, deliberately. Every consumer lives in this
/// workspace, so exhaustiveness checking is worth more than forward
/// compatibility for external crates that do not exist: adding a variant should
/// break the build at the storage projection and the renderer, not silently
/// produce events that land nowhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Event {
    /// A session began.
    #[serde(rename = "session.started")]
    SessionStarted(SessionStarted),
    /// A session finished.
    #[serde(rename = "session.ended")]
    SessionEnded(SessionEnded),
    /// The user submitted a prompt. Metadata only, never content.
    #[serde(rename = "prompt")]
    Prompt(PromptEvent),
    /// A file was read.
    #[serde(rename = "file.read")]
    FileRead(FileEvent),
    /// A file was created or modified.
    #[serde(rename = "file.write")]
    FileWrite(FileEvent),
    /// A shell command ran.
    #[serde(rename = "command")]
    Command(CommandEvent),
    /// An MCP tool was invoked.
    #[serde(rename = "mcp.call")]
    McpCall(McpEvent),
    /// Some other agent tool ran.
    #[serde(rename = "tool.call")]
    ToolCall(ToolCallEvent),
    /// A model response's token usage.
    #[serde(rename = "token.usage")]
    TokenUsage(TokenUsageEvent),
    /// Collection was paused or resumed by the user.
    #[serde(rename = "collection")]
    Collection(CollectionEvent),
    /// Our own hook configuration changed underneath us.
    #[serde(rename = "config.changed")]
    ConfigChanged(ConfigChangedEvent),
    /// The agent reported something this version does not model yet.
    #[serde(rename = "unknown")]
    Unknown(UnknownEvent),
}

impl Event {
    /// The stable kind string stored in the database.
    ///
    /// Kept in sync by hand with the serde renames above so that the column and
    /// the JSON payload always agree.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::SessionStarted(_) => "session.started",
            Self::SessionEnded(_) => "session.ended",
            Self::Prompt(_) => "prompt",
            Self::FileRead(_) => "file.read",
            Self::FileWrite(_) => "file.write",
            Self::Command(_) => "command",
            Self::McpCall(_) => "mcp.call",
            Self::ToolCall(_) => "tool.call",
            Self::TokenUsage(_) => "token.usage",
            Self::Collection(collection) => {
                if collection.paused {
                    "collection.paused"
                } else {
                    "collection.resumed"
                }
            }
            Self::ConfigChanged(_) => "config.changed",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// A session beginning.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionStarted {
    /// How the session began, when the agent says: `startup`, `resume`, `clear`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Path to the agent's own transcript for this session, if it exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
}

/// A session ending.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionEnded {
    /// Why the session ended, when the agent says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Prompt metadata.
///
/// Never carries prompt text. The hash exists so repeated prompts can be
/// recognized without storing what they said.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PromptEvent {
    /// Number of characters in the prompt.
    pub char_count: u32,
    /// Lowercase hex SHA-256 of the prompt text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// A file access.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEvent {
    /// Absolute path, as the agent reported it.
    pub path: String,
    /// The agent tool that touched it, for provenance.
    pub tool: String,
}

/// A shell command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEvent {
    /// The command line as submitted.
    pub command: String,
    /// The agent's own description of it, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// An MCP tool invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpEvent {
    /// The MCP server name.
    pub server: String,
    /// The tool invoked on it.
    pub tool: String,
}

/// Any other agent tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallEvent {
    /// The tool name as the agent reported it.
    pub tool: String,
}

/// Token usage for one model response.
///
/// # Why four counters and not three
///
/// The original spec had a single `cached_input_tokens`. Real transcripts
/// report cache *creation* and cache *read* separately, and providers bill them
/// very differently — writing to the cache costs more than an ordinary input
/// token, reading from it costs a small fraction of one. Merging them at
/// ingestion would make cost estimation permanently unfixable, so they stay
/// apart all the way down.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsageEvent {
    /// Who served the request, for example `anthropic`.
    pub provider: String,
    /// The exact model identifier the provider reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The provider's identifier for this response.
    ///
    /// The deduplication key. One API response appears as several transcript
    /// records — one per content block — each repeating the whole response's
    /// usage, so counting records rather than responses inflates every total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Uncached input tokens.
    pub input_tokens: u64,
    /// Input tokens written into the prompt cache.
    pub cache_creation_input_tokens: u64,
    /// Input tokens served from the prompt cache.
    pub cache_read_input_tokens: u64,
    /// Tokens generated.
    pub output_tokens: u64,
    /// Whether this response belongs to a subagent rather than the main thread.
    ///
    /// Kept so subagent consumption can be told apart from the user's own turns
    /// rather than silently inflating a session's headline number.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_subagent: bool,
    /// Everything else the provider reported, kept verbatim.
    ///
    /// Providers add usage categories without warning. Keeping the remainder
    /// means a new one is preserved rather than silently dropped, and can be
    /// promoted to a real column later without re-reading history.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub provider_usage: serde_json::Map<String, serde_json::Value>,
}

impl TokenUsageEvent {
    /// Every input token, cached or not.
    #[must_use]
    pub const fn total_input(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.cache_read_input_tokens)
    }

    /// Every token the response accounted for.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total_input().saturating_add(self.output_tokens)
    }
}

/// Collection was deliberately stopped or restarted.
///
/// Recorded so that the resulting hole in the timeline explains itself. A
/// monitor that can be silenced without the silence being visible is only
/// telling you what someone was willing to let you see.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionEvent {
    /// Whether collection is paused as of this event.
    pub paused: bool,
}

/// The monitored agent's hook configuration changed.
///
/// Recorded because a monitor whose own collection can be switched off without
/// trace is not much of a monitor. The common cause is benign — a reinstall, a
/// co-installed tool rewriting the file — so this is evidence, not an
/// accusation. What matters is that the gap is visible afterwards.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigChangedEvent {
    /// Settings file that changed.
    pub path: String,
    /// Whether our hooks are present now.
    ///
    /// The interesting transition is `true` to `false`: monitoring stopped and
    /// every later silence in the timeline is explained by this, not by an idle
    /// agent.
    pub hooks_present: bool,
    /// Fingerprint of the hook configuration after the change.
    pub fingerprint: String,
    /// Fingerprint before, when we had seen this file before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_fingerprint: Option<String>,
}

/// Something the agent reported that this version does not model.
///
/// Kept rather than dropped so that a new hook event shows up as an unknown row
/// instead of silently vanishing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnknownEvent {
    /// The agent's name for whatever this was.
    pub label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_matches_the_serialized_tag() {
        let cases = [
            Event::SessionStarted(SessionStarted::default()),
            Event::SessionEnded(SessionEnded::default()),
            Event::Prompt(PromptEvent::default()),
            Event::FileRead(FileEvent {
                path: "/tmp/a".into(),
                tool: "Read".into(),
            }),
            Event::Command(CommandEvent {
                command: "ls".into(),
                description: None,
            }),
            Event::McpCall(McpEvent {
                server: "github".into(),
                tool: "get_issue".into(),
            }),
            Event::ToolCall(ToolCallEvent {
                tool: "Glob".into(),
            }),
            Event::Unknown(UnknownEvent {
                label: "Notification".into(),
            }),
        ];

        for event in cases {
            let json = serde_json::to_value(&event).expect("serializable");
            let tag = json.get("kind").and_then(serde_json::Value::as_str);
            assert_eq!(tag, Some(event.kind()), "tag and kind() disagree");
        }
    }

    #[test]
    fn token_totals_add_every_input_category() {
        let usage = TokenUsageEvent {
            input_tokens: 2,
            cache_creation_input_tokens: 5_585,
            cache_read_input_tokens: 45_723,
            output_tokens: 497,
            ..TokenUsageEvent::default()
        };
        assert_eq!(usage.total_input(), 51_310);
        assert_eq!(usage.total(), 51_807);
    }

    #[test]
    fn unknown_usage_categories_survive_a_round_trip() {
        let encoded = r#"{"kind":"token.usage","provider":"anthropic","input_tokens":1,
            "cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":2,
            "provider_usage":{"service_tier":"standard","some_future_counter":9}}"#;
        let event: Event = serde_json::from_str(encoded).expect("parses");
        let Event::TokenUsage(usage) = event else {
            panic!("expected token usage")
        };
        assert_eq!(
            usage
                .provider_usage
                .get("some_future_counter")
                .and_then(serde_json::Value::as_u64),
            Some(9)
        );
    }

    #[test]
    fn session_identity_is_derived_from_the_external_id() {
        let event = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::Prompt(PromptEvent::default()),
        )
        .with_session(ExternalSessionId::from("s-1".to_owned()));

        assert!(event.session_id.is_some());
        assert_eq!(
            event.session_id,
            Some(SessionId::from_external(
                &AgentId::CLAUDE_CODE,
                &ExternalSessionId::from("s-1".to_owned())
            ))
        );
    }

    #[test]
    fn agent_event_round_trips_through_json() {
        let original = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::Command(CommandEvent {
                command: "cargo test".into(),
                description: None,
            }),
        )
        .with_project_path("/Users/dev/projects/acme".to_owned());

        let encoded = serde_json::to_string(&original).expect("serializable");
        let decoded: AgentEvent = serde_json::from_str(&encoded).expect("deserializable");
        assert_eq!(decoded, original);
    }
}
