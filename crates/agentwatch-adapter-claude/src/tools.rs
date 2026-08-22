//! Turning one tool invocation into the event that best describes it.
//!
//! Shared because the same tool call reaches us two ways — live, as a hook
//! payload, and later, as a `tool_use` block in the session transcript — and
//! the two must classify it identically. If they disagreed, the same `Edit`
//! would be a `file.write` in a live session and a bare `tool.call` after a
//! reconcile, and the timeline would change shape depending on which path
//! happened to observe it.
//!
//! # What this module refuses to see
//!
//! [`ToolInput`] names only the arguments worth keeping. `content`,
//! `old_string`, and `new_string` are absent from the type, so a `Write` or
//! `Edit` payload cannot carry file contents into a Rust value no matter which
//! path it arrives on. This is the same discipline the transcript reader
//! applies to `toolUseResult`, and it is enforced by the shape of the struct
//! rather than by remembering to avoid a field.

use agentwatch_events::{
    CommandEvent, Event, FileEvent, McpEvent, ToolCallEvent, UnknownEvent,
};
use serde::Deserialize;

/// Prefix Claude Code puts on MCP tool names.
const MCP_PREFIX: &str = "mcp__";

/// Separator between server and tool inside an MCP tool name.
const MCP_SEPARATOR: &str = "__";

/// The subset of tool arguments worth keeping.
///
/// Notably absent: `content`, `old_string`, and `new_string`, so file contents
/// never enter memory.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ToolInput {
    /// Target of a file tool.
    pub(crate) file_path: Option<String>,
    /// Target of a notebook tool.
    pub(crate) notebook_path: Option<String>,
    /// The command line for `Bash`.
    pub(crate) command: Option<String>,
    /// The agent's own description of a command.
    pub(crate) description: Option<String>,
}

/// Maps a tool call onto the most specific event it fits.
///
/// Falls back to a generic tool call rather than inventing a path or a command
/// that the arguments did not contain.
pub(crate) fn tool_event(tool: Option<&str>, input: Option<&ToolInput>) -> Event {
    let Some(tool) = tool else {
        return Event::Unknown(UnknownEvent {
            label: "missing_tool_name".to_owned(),
        });
    };

    if let Some(rest) = tool.strip_prefix(MCP_PREFIX) {
        return mcp_event(tool, rest);
    }

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

/// Splits `mcp__server__tool` into its parts.
fn mcp_event(full_name: &str, rest: &str) -> Event {
    match rest.split_once(MCP_SEPARATOR) {
        Some((server, tool)) if !server.is_empty() && !tool.is_empty() => Event::McpCall(McpEvent {
            server: server.to_owned(),
            tool: tool.to_owned(),
        }),
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
    use super::*;

    fn input(json: &str) -> ToolInput {
        serde_json::from_str(json).expect("valid tool input")
    }

    #[test]
    fn file_contents_cannot_be_deserialized_even_when_offered() {
        // The guarantee is the shape of the struct, not the caller's restraint:
        // an `Edit` payload carries the replacement text and it must not
        // survive being parsed.
        let parsed = input(
            r#"{"file_path":"/src/auth.rs","old_string":"AKIAIOSFODNN7EXAMPLE",
                "new_string":"secret","content":"more secret"}"#,
        );
        let event = tool_event(Some("Edit"), Some(&parsed));

        let encoded = serde_json::to_string(&event).expect("serializable");
        assert!(!encoded.contains("AKIA"), "{encoded}");
        assert!(!encoded.contains("secret"), "{encoded}");
        assert_eq!(
            event,
            Event::FileWrite(FileEvent {
                path: "/src/auth.rs".into(),
                tool: "Edit".into()
            })
        );
    }

    #[test]
    fn a_tool_with_no_name_is_reported_rather_than_guessed_at() {
        assert_eq!(
            tool_event(None, None),
            Event::Unknown(UnknownEvent {
                label: "missing_tool_name".to_owned()
            })
        );
    }
}
