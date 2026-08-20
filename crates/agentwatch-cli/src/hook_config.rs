//! The settings snippet that enables monitoring.
//!
//! This module prints and nothing else. Editing a user's agent configuration is
//! a phase 4 concern, and it will show a diff and ask first — partly out of
//! courtesy, and partly because a monitor that silently rewrites the config of
//! the thing it monitors has no business calling itself a security tool.

/// Hooks phase 1 installs.
///
/// `PreToolUse` is absent on purpose: with both installed every tool call would
/// produce two events, and distinguishing "attempted" from "completed" needs
/// the correlation work that lands in phase 2. `PostToolUse` alone means we
/// record what actually ran.
const HOOK_EVENTS: [&str; 4] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PostToolUse",
];

/// Default location of the hook binary in a release build.
const DEFAULT_BINARY: &str = "~/.local/bin/agentwatch-hook";

/// Renders the settings snippet.
#[must_use]
pub(crate) fn snippet(binary: Option<&str>) -> String {
    let binary = binary.unwrap_or(DEFAULT_BINARY);

    let mut hooks = String::new();
    for (index, event) in HOOK_EVENTS.iter().enumerate() {
        let separator = if index + 1 == HOOK_EVENTS.len() {
            ""
        } else {
            ","
        };
        hooks.push_str(&format!(
            "    \"{event}\": [
      {{
        \"hooks\": [
          {{ \"type\": \"command\", \"command\": \"{binary}\" }}
        ]
      }}
    ]{separator}\n"
        ));
    }

    format!(
        "Add this to ~/.claude/settings.json (merge with any hooks you already have):

{{
  \"hooks\": {{
{hooks}  }}
}}

Nothing was written. Copy the block above yourself.

The hook exits 0 on every path, so it cannot fail a tool call, and it forwards
payloads without interpreting them. Prompt text and tool output are discarded
by the daemon before anything reaches storage.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_every_hook_this_phase_installs() {
        let output = snippet(None);
        for event in HOOK_EVENTS {
            assert!(output.contains(event), "missing {event}");
        }
    }

    #[test]
    fn does_not_install_pretooluse_yet() {
        assert!(!snippet(None).contains("PreToolUse"));
    }

    #[test]
    fn uses_the_supplied_binary_path() {
        let output = snippet(Some("/opt/agentwatch-hook"));
        assert!(output.contains("/opt/agentwatch-hook"));
        assert!(!output.contains(DEFAULT_BINARY));
    }

    #[test]
    fn emits_parseable_json() {
        let output = snippet(None);
        let start = output.find('{').expect("json starts");
        let end = output.rfind('}').expect("json ends");
        let json: serde_json::Value =
            serde_json::from_str(&output[start..=end]).expect("valid json");

        let hooks = json.get("hooks").and_then(serde_json::Value::as_object);
        assert_eq!(hooks.map(serde_json::Map::len), Some(HOOK_EVENTS.len()));
    }
}
