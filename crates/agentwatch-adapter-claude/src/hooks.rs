//! Hook payload parsing and mapping.

use agentwatch_events::{
    AdapterError, AgentEvent, CommandEvent, Event, EvidenceSource, FileEvent, HookAdapter,
    HookEnvelope, McpEvent, PromptEvent, SessionEnded, SessionStarted, ToolCallEvent, UnknownEvent,
};
use agentwatch_types::{AgentId, ExternalSessionId};
use serde::Deserialize;

use crate::SOURCE;
use crate::redact;

/// Prefix Claude Code puts on MCP tool names.
const MCP_PREFIX: &str = "mcp__";

/// Separator between server and tool inside an MCP tool name.
const MCP_SEPARATOR: &str = "__";

/// The Claude Code adapter.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct ClaudeAdapter;

impl ClaudeAdapter {
    /// Creates the adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl HookAdapter for ClaudeAdapter {
    fn source(&self) -> &'static str {
        SOURCE
    }

    fn normalize(&self, envelope: &HookEnvelope) -> Result<AgentEvent, AdapterError> {
        let payload: HookPayload = serde_json::from_str(envelope.payload.get())?;

        let mut event = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            payload.to_event(),
        )
        .at(envelope.sent_at);

        if let Some(session) = payload.session_id {
            event = event.with_session(ExternalSessionId::from(session));
        }
        if let Some(cwd) = payload.cwd {
            event = event.with_project_path(cwd);
        }

        Ok(event)
    }
}

/// A Claude Code hook payload.
///
/// Every field is optional and unknown keys are ignored: the payload shape is
/// an undocumented surface that changes between releases, and a new key must
/// never cost us an event.
///
/// `tool_response` is absent by design. Not deserializing it is what guarantees
/// tool output cannot leak into storage.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HookPayload {
    /// Which hook fired.
    hook_event_name: Option<String>,
    /// The agent's session identifier.
    session_id: Option<String>,
    /// Where the agent writes this session's transcript.
    transcript_path: Option<String>,
    /// The session's working directory.
    cwd: Option<String>,
    /// For tool hooks, the tool that ran.
    tool_name: Option<String>,
    /// For tool hooks, the arguments it ran with.
    tool_input: Option<ToolInput>,
    /// For prompt hooks, the prompt text. Summarized, never stored.
    prompt: Option<String>,
    /// For `SessionStart`, how the session began.
    source: Option<String>,
    /// For `SessionEnd`, why it ended.
    reason: Option<String>,
}

/// The subset of tool arguments worth keeping.
///
/// Notably absent: `content`, `old_string`, and `new_string`, so file contents
/// never enter memory.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolInput {
    /// Target of a file tool.
    file_path: Option<String>,
    /// Target of a notebook tool.
    notebook_path: Option<String>,
    /// The command line for `Bash`.
    command: Option<String>,
    /// The agent's own description of a command.
    description: Option<String>,
}

impl HookPayload {
    /// Maps the payload onto a normalized event.
    fn to_event(&self) -> Event {
        match self.hook_event_name.as_deref() {
            Some("SessionStart") => Event::SessionStarted(SessionStarted {
                trigger: self.source.clone(),
                transcript_path: self.transcript_path.clone(),
            }),
            Some("SessionEnd") => Event::SessionEnded(SessionEnded {
                reason: self.reason.clone(),
            }),
            Some("UserPromptSubmit") => Event::Prompt(self.prompt_metadata()),
            Some("PreToolUse" | "PostToolUse") => self.tool_event(),
            other => Event::Unknown(UnknownEvent {
                label: other.unwrap_or("missing_hook_event_name").to_owned(),
            }),
        }
    }

    /// Summarizes the prompt without keeping it.
    fn prompt_metadata(&self) -> PromptEvent {
        let Some(prompt) = self.prompt.as_deref() else {
            return PromptEvent::default();
        };

        let (char_count, sha256) = redact::summarize(prompt);
        PromptEvent {
            char_count,
            sha256: Some(sha256),
        }
    }

    /// Maps a tool call onto the most specific event it fits.
    ///
    /// Falls back to a generic tool call rather than inventing a path or a
    /// command that the payload did not contain.
    fn tool_event(&self) -> Event {
        let Some(tool) = self.tool_name.as_deref() else {
            return Event::Unknown(UnknownEvent {
                label: "missing_tool_name".to_owned(),
            });
        };

        if let Some(rest) = tool.strip_prefix(MCP_PREFIX) {
            return mcp_event(tool, rest);
        }

        let input = self.tool_input.as_ref();
        let file_path = input.and_then(|input| {
            input
                .file_path
                .as_deref()
                .or(input.notebook_path.as_deref())
        });

        match tool {
            "Read" | "NotebookRead" => match file_path {
                Some(path) => Event::FileRead(FileEvent {
                    path: path.to_owned(),
                    tool: tool.to_owned(),
                }),
                None => generic_tool(tool),
            },
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => match file_path {
                Some(path) => Event::FileWrite(FileEvent {
                    path: path.to_owned(),
                    tool: tool.to_owned(),
                }),
                None => generic_tool(tool),
            },
            "Bash" | "BashOutput" => match input.and_then(|input| input.command.as_deref()) {
                Some(command) => Event::Command(CommandEvent {
                    command: command.to_owned(),
                    description: input.and_then(|input| input.description.clone()),
                }),
                None => generic_tool(tool),
            },
            _ => generic_tool(tool),
        }
    }
}

/// Splits `mcp__server__tool` into its parts.
fn mcp_event(full_name: &str, rest: &str) -> Event {
    match rest.split_once(MCP_SEPARATOR) {
        Some((server, tool)) if !server.is_empty() && !tool.is_empty() => {
            Event::McpCall(McpEvent {
                server: server.to_owned(),
                tool: tool.to_owned(),
            })
        }
        // A prefixed name we cannot split is still an MCP call; record it whole
        // rather than guessing at a server boundary that is not there.
        _ => Event::McpCall(McpEvent {
            server: "unknown".to_owned(),
            tool: full_name.to_owned(),
        }),
    }
}

/// Wraps a tool this version does not model specifically.
fn generic_tool(tool: &str) -> Event {
    Event::ToolCall(ToolCallEvent {
        tool: tool.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use agentwatch_types::Timestamp;
    use serde_json::value::RawValue;

    use super::*;

    fn envelope(payload: &str) -> HookEnvelope {
        let json = serde_json::json!({
            "v": agentwatch_events::PROTOCOL_VERSION,
            "source": SOURCE,
            "sent_at": 1_755_000_000_000_000_i64,
            "hook_version": "0.1.0",
            "payload": RawValue::from_string(payload.to_owned()).expect("valid json"),
        });
        serde_json::from_value(json).expect("valid envelope")
    }

    fn normalize(payload: &str) -> AgentEvent {
        ClaudeAdapter::new()
            .normalize(&envelope(payload))
            .expect("normalizes")
    }

    #[test]
    fn maps_a_read_to_a_file_read() {
        let event = normalize(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Read",
                "tool_input":{"file_path":"/src/auth.rs"}}"#,
        );
        assert_eq!(
            event.event,
            Event::FileRead(FileEvent {
                path: "/src/auth.rs".into(),
                tool: "Read".into()
            })
        );
    }

    #[test]
    fn maps_an_edit_to_a_file_write() {
        let event = normalize(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Edit",
                "tool_input":{"file_path":"/src/auth.rs","old_string":"a","new_string":"b"}}"#,
        );
        assert_eq!(
            event.event,
            Event::FileWrite(FileEvent {
                path: "/src/auth.rs".into(),
                tool: "Edit".into()
            })
        );
    }

    #[test]
    fn maps_bash_to_a_command() {
        let event = normalize(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Bash",
                "tool_input":{"command":"cargo test","description":"run tests"}}"#,
        );
        assert_eq!(
            event.event,
            Event::Command(CommandEvent {
                command: "cargo test".into(),
                description: Some("run tests".into()),
            })
        );
    }

    #[test]
    fn splits_an_mcp_tool_name_into_server_and_tool() {
        let event = normalize(
            r#"{"hook_event_name":"PostToolUse","tool_name":"mcp__github__search_issues"}"#,
        );
        assert_eq!(
            event.event,
            Event::McpCall(McpEvent {
                server: "github".into(),
                tool: "search_issues".into()
            })
        );
    }

    #[test]
    fn keeps_an_unsplittable_mcp_name_whole() {
        let event = normalize(r#"{"hook_event_name":"PostToolUse","tool_name":"mcp__weird"}"#);
        assert_eq!(
            event.event,
            Event::McpCall(McpEvent {
                server: "unknown".into(),
                tool: "mcp__weird".into()
            })
        );
    }

    #[test]
    fn falls_back_to_a_generic_tool_call_for_unmodelled_tools() {
        let event = normalize(r#"{"hook_event_name":"PostToolUse","tool_name":"Glob"}"#);
        assert_eq!(
            event.event,
            Event::ToolCall(ToolCallEvent {
                tool: "Glob".into()
            })
        );
    }

    #[test]
    fn falls_back_when_a_file_tool_has_no_path() {
        let event = normalize(r#"{"hook_event_name":"PostToolUse","tool_name":"Read"}"#);
        assert_eq!(
            event.event,
            Event::ToolCall(ToolCallEvent {
                tool: "Read".into()
            })
        );
    }

    #[test]
    fn reduces_a_prompt_to_a_count_and_a_hash() {
        let event =
            normalize(r#"{"hook_event_name":"UserPromptSubmit","prompt":"my secret plan"}"#);

        let Event::Prompt(prompt) = event.event else {
            panic!("expected a prompt event");
        };
        assert_eq!(prompt.char_count, 14);
        assert!(prompt.sha256.is_some());

        let encoded = serde_json::to_string(&prompt).expect("serializable");
        assert!(
            !encoded.contains("secret"),
            "prompt text must never be stored"
        );
    }

    #[test]
    fn never_carries_tool_output_through() {
        let event = normalize(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Read",
                "tool_input":{"file_path":"/etc/hosts"},
                "tool_response":{"content":"AKIAIOSFODNN7EXAMPLE"}}"#,
        );

        let encoded = serde_json::to_string(&event).expect("serializable");
        assert!(
            !encoded.contains("AKIA"),
            "tool output must never be stored"
        );
    }

    #[test]
    fn records_session_start_with_its_trigger_and_transcript() {
        let event = normalize(
            r#"{"hook_event_name":"SessionStart","source":"startup",
                "transcript_path":"/tmp/t.jsonl","session_id":"s-1","cwd":"/work"}"#,
        );

        assert_eq!(
            event.event,
            Event::SessionStarted(SessionStarted {
                trigger: Some("startup".into()),
                transcript_path: Some("/tmp/t.jsonl".into()),
            })
        );
        assert!(event.session_id.is_some());
        assert_eq!(event.project_path.as_deref(), Some("/work"));
    }

    #[test]
    fn records_session_end_with_its_reason() {
        let event = normalize(r#"{"hook_event_name":"SessionEnd","reason":"clear"}"#);
        assert_eq!(
            event.event,
            Event::SessionEnded(SessionEnded {
                reason: Some("clear".into())
            })
        );
    }

    #[test]
    fn an_unrecognized_hook_becomes_a_visible_unknown_event() {
        let event = normalize(r#"{"hook_event_name":"SomeFutureHook"}"#);
        assert_eq!(
            event.event,
            Event::Unknown(UnknownEvent {
                label: "SomeFutureHook".into()
            })
        );
    }

    #[test]
    fn tolerates_unknown_keys() {
        let event = normalize(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Bash",
                "tool_input":{"command":"ls","timeout_ms":5000},
                "some_future_field":{"nested":true}}"#,
        );
        assert!(matches!(event.event, Event::Command(_)));
    }

    #[test]
    fn takes_its_timestamp_from_the_envelope() {
        let event = normalize(r#"{"hook_event_name":"SessionStart"}"#);
        assert_eq!(
            event.timestamp,
            Timestamp::from_micros(1_755_000_000_000_000)
        );
    }

    #[test]
    fn rejects_a_payload_that_is_not_an_object() {
        let result = ClaudeAdapter::new().normalize(&envelope("[1,2,3]"));
        assert!(result.is_err());
    }
}
