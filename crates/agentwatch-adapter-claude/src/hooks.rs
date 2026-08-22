//! Hook payload parsing and mapping.

use agentwatch_events::{
    AdapterError, AgentEvent, ContextCompactedEvent, Event, EvidenceSource, HookAdapter,
    HookEnvelope, NotificationEvent, PromptEvent, SessionEnded, SessionStarted, TurnEndedEvent,
    UnknownEvent,
};
use agentwatch_types::{AgentId, EventId, ExternalSessionId};
use serde::Deserialize;

use crate::SOURCE;
use crate::redact;
use crate::tools::{self, ToolInput};

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

        // Key the event on the tool call's own identifier when the agent sends
        // one. The transcript names the same call the same way, so the live
        // event and the one recovered by a later reconcile become the same row
        // rather than two records of one action.
        if let Some(tool_use_id) = payload.tool_use_id.as_deref() {
            event = event.with_id(EventId::from_key(&AgentId::CLAUDE_CODE, tool_use_id));
        }

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
    /// The agent's identifier for this specific tool call, when it sends one.
    ///
    /// The same identifier the transcript writes on the `tool_use` block, which
    /// makes it the one key both observation paths can agree on. When it is
    /// present the live event and the reconciled one derive the same event id
    /// and storage collapses them into a single row; when it is absent the two
    /// paths each record the call and the timeline double-counts it.
    tool_use_id: Option<String>,
    /// For prompt hooks, the prompt text. Summarized, never stored.
    prompt: Option<String>,
    /// For `SessionStart`, how the session began.
    source: Option<String>,
    /// For `SessionEnd`, why it ended.
    reason: Option<String>,
    /// For `Notification`, what the agent is waiting for.
    message: Option<String>,
    /// For `PreCompact`, whether compaction was automatic or asked for.
    trigger: Option<String>,
}

impl HookPayload {
    /// Maps the payload onto a normalized event.
    ///
    /// # Why the name is lowercased before it is matched
    ///
    /// The hook name is not spelled consistently across surfaces. Payloads
    /// observed on this machine carry `postToolUse` and `sessionStart` where
    /// the documented spelling is `PostToolUse` and `SessionStart`, and an
    /// exact match sent all of them to [`Event::Unknown`] — which meant a tool
    /// call arrived with its file path or command line in hand and was stored
    /// as a bare label. Casing is not a distinction this payload ever intends
    /// to make, so it is not one worth honouring.
    ///
    /// The label on an unrecognised event keeps its **original** casing: it is
    /// the only evidence of what the agent actually said, and normalizing it
    /// would hide the very difference someone reads it to diagnose.
    fn to_event(&self) -> Event {
        let raw = self.hook_event_name.as_deref();

        match raw.map(str::to_ascii_lowercase).as_deref() {
            Some("sessionstart") => Event::SessionStarted(SessionStarted {
                trigger: self.source.clone(),
                transcript_path: self.transcript_path.clone(),
            }),
            Some("sessionend") => Event::SessionEnded(SessionEnded {
                reason: self.reason.clone(),
            }),
            // `beforeSubmitPrompt` is a second name for the same moment, seen
            // in payloads alongside the documented one. It is mapped here
            // rather than left unknown because the alternative is losing the
            // prompt metadata entirely; if it should ever turn out to carry no
            // `prompt` field, `prompt_metadata` already degrades to a zero
            // count rather than inventing one.
            Some("userpromptsubmit" | "beforesubmitprompt") => {
                Event::Prompt(self.prompt_metadata())
            }
            Some("pretooluse" | "posttooluse") => self.tool_event(),
            // The only signal here that no transcript can reconstruct: the
            // agent waiting on a person.
            Some("notification") => Event::Notification(NotificationEvent {
                message: self.message.clone(),
            }),
            Some("stop") => Event::TurnEnded(TurnEndedEvent { subagent: false }),
            Some("subagentstop") => Event::TurnEnded(TurnEndedEvent { subagent: true }),
            Some("precompact") => Event::ContextCompacted(ContextCompactedEvent {
                trigger: self.trigger.clone().or_else(|| self.source.clone()),
            }),
            // `None` is a different failure from a name we do not recognise —
            // a payload with no name at all is malformed rather than merely
            // new — and the two are told apart by the label.
            _ => Event::Unknown(UnknownEvent {
                label: raw.unwrap_or("missing_hook_event_name").to_owned(),
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
    /// Delegates to the shared classifier so a hook and a transcript describe
    /// the same call the same way.
    fn tool_event(&self) -> Event {
        tools::tool_event(self.tool_name.as_deref(), self.tool_input.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{CommandEvent, FileEvent, McpEvent, ToolCallEvent};
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
    fn the_waiting_and_boundary_hooks_are_mapped() {
        use agentwatch_events::{ContextCompactedEvent, NotificationEvent, TurnEndedEvent};

        let cases = [
            (
                r#"{"hook_event_name":"Notification","message":"Claude needs your permission"}"#,
                Event::Notification(NotificationEvent {
                    message: Some("Claude needs your permission".into()),
                }),
            ),
            (
                r#"{"hook_event_name":"Stop"}"#,
                Event::TurnEnded(TurnEndedEvent { subagent: false }),
            ),
            (
                // A subagent finishing must not read as the main thread
                // finishing, or every spawned agent would end its parent's turn.
                r#"{"hook_event_name":"SubagentStop"}"#,
                Event::TurnEnded(TurnEndedEvent { subagent: true }),
            ),
            (
                r#"{"hook_event_name":"PreCompact","trigger":"auto"}"#,
                Event::ContextCompacted(ContextCompactedEvent {
                    trigger: Some("auto".into()),
                }),
            ),
        ];

        for (payload, expected) in cases {
            assert_eq!(normalize(payload).event, expected, "payload: {payload}");
        }
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
    fn a_tool_use_id_makes_the_live_event_and_the_transcript_agree() {
        use agentwatch_types::EventId;

        let event = normalize(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"toolu_abc",
                "tool_input":{"command":"cargo test"}}"#,
        );

        // The transcript reader derives its id from the same string. If these
        // ever diverge the timeline double-counts every tool call a live
        // session made and a later reconcile re-read.
        assert_eq!(
            event.id,
            EventId::from_key(&AgentId::CLAUDE_CODE, "toolu_abc")
        );
    }

    #[test]
    fn without_a_tool_use_id_the_event_still_gets_one_of_its_own() {
        // Older agents, and hooks other than the tool ones, send no such id.
        // They must still produce an event; they just cannot be reconciled
        // against the transcript by identity.
        let first = normalize(r#"{"hook_event_name":"PostToolUse","tool_name":"Glob"}"#);
        let second = normalize(r#"{"hook_event_name":"PostToolUse","tool_name":"Glob"}"#);
        assert_ne!(first.id, second.id, "two calls are two events");
    }

    #[test]
    fn hook_names_are_matched_whatever_their_casing() {
        // Not hypothetical: every one of these spellings was found in the
        // database on the development machine, stored as `unknown` because the
        // match was exact. `postToolUse` alone accounted for 31 tool calls
        // whose file path or command line was discarded.
        let cases = [
            (
                r#"{"hook_event_name":"sessionStart","source":"startup"}"#,
                Event::SessionStarted(SessionStarted {
                    trigger: Some("startup".into()),
                    transcript_path: None,
                }),
            ),
            (
                r#"{"hook_event_name":"sessionEnd","reason":"clear"}"#,
                Event::SessionEnded(SessionEnded {
                    reason: Some("clear".into()),
                }),
            ),
            (
                r#"{"hook_event_name":"postToolUse","tool_name":"Read",
                    "tool_input":{"file_path":"/src/auth.rs"}}"#,
                Event::FileRead(FileEvent {
                    path: "/src/auth.rs".into(),
                    tool: "Read".into(),
                }),
            ),
            (
                r#"{"hook_event_name":"PRETOOLUSE","tool_name":"Bash",
                    "tool_input":{"command":"cargo test"}}"#,
                Event::Command(CommandEvent {
                    command: "cargo test".into(),
                    description: None,
                }),
            ),
        ];

        for (payload, expected) in cases {
            assert_eq!(normalize(payload).event, expected, "payload: {payload}");
        }
    }

    #[test]
    fn before_submit_prompt_is_a_prompt_like_any_other() {
        let event = normalize(
            r#"{"hook_event_name":"beforeSubmitPrompt","prompt":"my secret plan"}"#,
        );

        let Event::Prompt(prompt) = event.event else {
            panic!("expected a prompt event");
        };
        assert_eq!(prompt.char_count, 14);
        assert!(prompt.sha256.is_some());
    }

    #[test]
    fn an_unrecognised_label_keeps_the_casing_the_agent_used() {
        // The label is the only record of what actually arrived. Lowercasing it
        // to match would erase the difference someone reads it to diagnose.
        let event = normalize(r#"{"hook_event_name":"someFutureHook"}"#);
        assert_eq!(
            event.event,
            Event::Unknown(UnknownEvent {
                label: "someFutureHook".into()
            })
        );
    }

    #[test]
    fn a_payload_with_no_hook_name_is_distinguishable_from_an_unknown_one() {
        let event = normalize(r#"{"tool_name":"Read"}"#);
        assert_eq!(
            event.event,
            Event::Unknown(UnknownEvent {
                label: "missing_hook_event_name".into()
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
