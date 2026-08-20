//! Recovering file references from shell command lines.
//!
//! Tool-level observation sees `Read` but not `cat .env`, because a shell
//! command is opaque to the agent's own tool reporting. That gap is the whole
//! reason a file monitor built on hooks cannot claim completeness.
//!
//! This narrows the gap without pretending to close it. Command lines are
//! scanned for things that look like paths and those are classified. It is
//! deliberately *not* a shell parser: it will miss `eval`, variable expansion,
//! heredocs, and anything a determined process wants to hide. What it catches
//! is the ordinary case, which is also the common one.
//!
//! Results are reported as references found in a command, never as observed
//! file reads. Inventing a `FileRead` from a string match would put inference
//! and observation in the same bucket, which is exactly what the evidence model
//! exists to prevent.

use crate::sensitivity::{Sensitivity, classify};

/// Characters that separate arguments, outside quotes.
const SEPARATORS: [char; 6] = [' ', '\t', '\n', '|', ';', '&'];

/// A path referenced by a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathReference {
    /// The token as it appeared.
    pub path: String,
    /// What that path is.
    pub sensitivity: Sensitivity,
}

/// Finds paths referenced by a command line.
///
/// Returns only notable references. An ordinary `cargo test` yields nothing,
/// which is the point: this exists to surface the interesting minority.
#[must_use]
pub fn scan_command(command: &str) -> Vec<PathReference> {
    let mut found: Vec<PathReference> = Vec::new();

    for token in tokenize(command) {
        let candidate = token.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | ','));
        if candidate.is_empty() || !looks_like_path(candidate) {
            continue;
        }

        let sensitivity = classify(candidate);
        if !sensitivity.is_notable() {
            continue;
        }
        if found.iter().any(|existing| existing.path == candidate) {
            continue;
        }

        found.push(PathReference {
            path: candidate.to_owned(),
            sensitivity,
        });
    }

    found
}

/// The most serious thing a command line refers to.
#[must_use]
pub fn worst_in_command(command: &str) -> Sensitivity {
    scan_command(command)
        .into_iter()
        .map(|reference| reference.sensitivity)
        .max()
        .unwrap_or(Sensitivity::Normal)
}

/// Splits a command into argument-like tokens, respecting simple quoting.
fn tokenize(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    for character in command.chars() {
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => current.push(character),
            None if character == '"' || character == '\'' => quote = Some(character),
            None if SEPARATORS.contains(&character) => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Whether a token is plausibly a filesystem path rather than a flag or word.
fn looks_like_path(token: &str) -> bool {
    if token.starts_with('-') {
        return false;
    }
    token.contains('/') || token.starts_with('.') || token.contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_dotenv_read_that_no_file_tool_would_report() {
        let found = scan_command("cat .env");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, ".env");
        assert_eq!(found[0].sensitivity, Sensitivity::Sensitive);
    }

    #[test]
    fn finds_credentials_behind_a_pipe() {
        let found = scan_command("cat ~/.aws/credentials | base64");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].sensitivity, Sensitivity::HighlySensitive);
    }

    #[test]
    fn finds_a_key_inside_quotes() {
        let found = scan_command("cp '/Users/dev/.ssh/id_ed25519' /tmp/x");
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("id_ed25519"));
    }

    #[test]
    fn handles_several_commands_on_one_line() {
        let found = scan_command("ls; cat .env && echo done");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, ".env");
    }

    #[test]
    fn an_ordinary_command_yields_nothing() {
        assert!(scan_command("cargo test --workspace").is_empty());
        assert!(scan_command("git status").is_empty());
        assert!(scan_command("pnpm run build").is_empty());
    }

    #[test]
    fn source_files_are_not_reported() {
        assert!(scan_command("rustfmt src/main.rs").is_empty());
    }

    #[test]
    fn flags_are_not_mistaken_for_paths() {
        assert!(scan_command("ls -la --color=auto").is_empty());
    }

    #[test]
    fn the_same_path_twice_is_reported_once() {
        let found = scan_command("diff .env .env");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn worst_reports_the_most_serious_reference() {
        assert_eq!(
            worst_in_command("cat .env ~/.ssh/id_rsa"),
            Sensitivity::HighlySensitive
        );
        assert_eq!(worst_in_command("cat .env"), Sensitivity::Sensitive);
        assert_eq!(worst_in_command("cargo build"), Sensitivity::Normal);
    }

    #[test]
    fn an_empty_command_is_normal() {
        assert_eq!(worst_in_command(""), Sensitivity::Normal);
    }

    #[test]
    fn quoted_separators_do_not_split_a_token() {
        let found = scan_command("cat \"/Users/my name/.aws/credentials\"");
        assert_eq!(found.len(), 1, "a space inside quotes is not a separator");
    }
}
