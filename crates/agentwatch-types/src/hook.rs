//! The command string that runs our hook.
//!
//! Shared because two crates have to agree on it exactly: the CLI writes it
//! into the agent's settings, and the daemon reads those settings back to tell
//! whether monitoring is still configured. If those two ever disagreed, the
//! daemon would report collection as disabled while it was running fine, or —
//! worse — report it as running while the entries were gone.
//!
//! Two forms are recognised. Since 0.2 everything is one executable, so the
//! command is `<exe> hook`. Before that the hook was its own binary, and
//! settings written by 0.1 still say `<dir>/agentwatch-hook`. Both are ours:
//! an upgrade must not silently stop recognising the entries it wrote itself,
//! and `install-hooks --uninstall` must still be able to remove them.

use std::path::Path;

/// The subcommand that forwards a payload.
const SUBCOMMAND: &str = "hook";

/// Executable name, as installed.
const EXECUTABLE: &str = "agentwatch";

/// The 0.1 hook binary, kept only so its entries stay recognisable.
const LEGACY_EXECUTABLE: &str = "agentwatch-hook";

/// Builds the command that runs the hook.
///
/// Quoted when the path needs it. The agent runs hook commands through a shell,
/// so an executable under a directory with a space in its name would otherwise
/// be split into a program that does not exist and an argument nobody reads —
/// and the hook failing to run is exactly the failure this project refuses to
/// let happen silently.
#[must_use]
pub fn hook_command(executable: &Path) -> String {
    format!(
        "{} {SUBCOMMAND}",
        shell_quote(&executable.to_string_lossy())
    )
}

/// Whether a hook command in a settings file is one of ours.
///
/// Deliberately narrow. Matching any command merely *containing* `agentwatch`
/// would claim a user's own wrapper script, and `--uninstall` would then delete
/// it. Ours is an `agentwatch` executable invoked as the hook and nothing else.
///
/// The executable's name is matched by prefix rather than exactly, so a binary
/// someone renamed — `agentwatch-0.2`, or the hashed name Cargo gives a test
/// harness — is still recognised as the thing that wrote the entry. Getting
/// this wrong is not cosmetic: an unrecognised entry means the next
/// `install-hooks` adds a second one beside it, and the agent then runs two
/// hooks on every tool call.
#[must_use]
pub fn is_our_hook_command(command: &str) -> bool {
    let command = command.trim();

    let program = match command.strip_suffix(SUBCOMMAND) {
        // `<exe> hook`: what this version writes. The separator has to be real
        // whitespace, so `/opt/nothook` is not a match.
        Some(head) if head.ends_with(char::is_whitespace) => head.trim_end(),
        // Anything else is only ours if it is the old standalone binary, whose
        // name is matched exactly: with no subcommand to disambiguate it, a
        // prefix would start claiming the CLI itself.
        _ => return basename(&unquote(command)) == LEGACY_EXECUTABLE,
    };

    basename(&unquote(program)).starts_with(EXECUTABLE)
}

/// The last path component, or the whole string if it has none.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Removes one layer of matching shell quotes.
fn unquote(text: &str) -> String {
    for quote in ['\'', '"'] {
        if let Some(inner) = text.strip_prefix(quote).and_then(|t| t.strip_suffix(quote)) {
            // Only single quotes can carry an escaped quote of their own, and
            // only in the form this module writes.
            return inner.replace("'\\''", "'");
        }
    }
    text.to_owned()
}

/// Quotes a word for a POSIX shell, if it needs it.
///
/// Single quotes, because they are literal: nothing inside them is expanded, so
/// a path containing `$` or a backslash survives intact. A single quote in the
/// path itself is the one character that cannot appear inside them, and is
/// closed, escaped, and reopened in the usual way.
fn shell_quote(word: &str) -> String {
    let safe =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '=');
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_is_left_unquoted() {
        let command = hook_command(Path::new("/Users/a/.local/bin/agentwatch"));
        assert_eq!(command, "/Users/a/.local/bin/agentwatch hook");
        assert!(is_our_hook_command(&command));
    }

    #[test]
    fn a_path_with_a_space_is_quoted_and_still_recognised() {
        let command = hook_command(Path::new("/Users/a/My Tools/agentwatch"));
        assert_eq!(command, "'/Users/a/My Tools/agentwatch' hook");
        assert!(
            is_our_hook_command(&command),
            "a quoted path must survive the round trip, or the daemon reports \
             collection as disabled while it is running"
        );
    }

    #[test]
    fn a_path_with_a_quote_in_it_survives() {
        let command = hook_command(Path::new("/Users/o'brien/bin/agentwatch"));
        assert!(is_our_hook_command(&command), "{command}");
    }

    #[test]
    fn the_standalone_binary_from_0_1_is_still_ours() {
        assert!(is_our_hook_command("/Users/a/.local/bin/agentwatch-hook"));
        assert!(is_our_hook_command("  /opt/agentwatch-hook  "));
    }

    #[test]
    fn a_renamed_executable_is_still_ours() {
        // A second `install-hooks` must not add a duplicate beside the entry
        // the same install already wrote.
        for command in [
            "/Users/a/.local/bin/agentwatch-0.2.1 hook",
            "/Users/a/target/debug/deps/agentwatch-9f2c1a hook",
        ] {
            assert!(is_our_hook_command(command), "{command}");
        }
    }

    #[test]
    fn someone_elses_command_is_not_ours() {
        for command in [
            "/usr/local/bin/other-tool",
            "my-agentwatch-notes.sh",
            "/opt/watch hook",
            "echo agentwatch hook",
            "",
        ] {
            assert!(!is_our_hook_command(command), "claimed {command:?}");
        }
    }

    #[test]
    fn the_cli_itself_is_not_a_hook_entry() {
        assert!(
            !is_our_hook_command("/Users/a/.local/bin/agentwatch"),
            "the bare executable is not a hook command"
        );
        assert!(!is_our_hook_command(
            "/Users/a/.local/bin/agentwatch status"
        ));
    }
}
