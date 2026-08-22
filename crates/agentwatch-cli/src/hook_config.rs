//! The settings snippet that enables monitoring.
//!
//! This module prints and nothing else. `agentwatch init` and `install-hooks`
//! are what actually edit a settings file, and they show a diff and ask first —
//! partly out of courtesy, and partly because a monitor that silently rewrites
//! the config of the thing it monitors has no business calling itself a
//! security tool. This is here for anyone who would rather paste it themselves.

use crate::install::HOOK_EVENTS;

/// Default command in a release install.
///
/// One executable serves every role, so the hook is a subcommand of it.
const DEFAULT_BINARY: &str = "~/.local/bin/agentwatch hook";

/// Renders the settings snippet.
#[must_use]
pub(crate) fn snippet(binary: Option<&str>) -> String {
    let binary = binary.map_or_else(
        || DEFAULT_BINARY.to_owned(),
        |path| agentwatch_types::hook_command(std::path::Path::new(path)),
    );

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
        let output = snippet(Some("/opt/agentwatch"));
        assert!(output.contains("/opt/agentwatch hook"));
        assert!(!output.contains(DEFAULT_BINARY));
    }

    #[test]
    fn what_it_prints_is_recognised_as_ours() {
        // Otherwise someone who pastes this by hand ends up with entries that
        // `install-hooks` would duplicate and `--uninstall` would not remove.
        let output = snippet(Some("/opt/agentwatch"));
        assert!(agentwatch_types::is_our_hook_command(
            "/opt/agentwatch hook"
        ));
        assert!(output.contains("/opt/agentwatch hook"));
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

    #[test]
    fn the_snippet_names_every_event_the_installer_registers() {
        // These were once two separate lists. They drifted: `HOOK_EVENTS` grew
        // to eight here while the installer's copy stayed at four, so
        // `install-hooks` quietly wrote half the hooks and the printed snippet
        // promised the rest. There is one constant now, and this asserts the
        // snippet is generated from it rather than hand-maintained beside it.
        let snippet = snippet(None);
        for event in crate::install::HOOK_EVENTS {
            assert!(snippet.contains(event), "snippet omits {event}");
        }
    }
}
