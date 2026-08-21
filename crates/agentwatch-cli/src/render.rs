//! Turning rows into lines.

use agentwatch_storage::{EventRow, Notable, SessionRow, TokenTotals};
use agentwatch_types::Timestamp;
pub(crate) use agentwatch_types::thousands;

/// What the sensitive-access listing cannot tell you.
///
/// Printed with every security listing rather than buried in documentation: an
/// empty list is the moment someone is most likely to read it as "nothing
/// happened", and that is exactly what it does not mean.
pub(crate) const SECURITY_CAVEAT: &str = concat!(
    "Coverage note: paths are classified by name, never by reading them, so a\n",
    "credential in an ordinarily-named file is invisible here. Command lines are\n",
    "scanned, not parsed, so indirection hides references. Entries marked derived\n",
    "are inferred from a command line, not observed."
);

/// Column header for a session listing.
pub(crate) fn session_header() -> String {
    format!(
        "{:<8}  {:<16}  {:<9}  {:>13}  {:>4}  {:>4}  {:>4}  {:>4}  {}",
        "session", "started", "duration", "tokens", "cmd", "file", "mcp", "sens", "project"
    )
}

/// One row of a session listing.
pub(crate) fn session_line(row: &SessionRow) -> String {
    let started = row.started_at_us.map_or_else(|| "-".to_owned(), clock_time);
    let duration = row.duration_ms.map_or_else(
        || match row.status.as_str() {
            "active" => "running".to_owned(),
            // Never watched running, so neither a duration nor "running" is a
            // claim this row can support.
            "unknown" => "?".to_owned(),
            _ => "-".to_owned(),
        },
        format_duration,
    );

    let home = std::env::var("HOME").ok();
    let project = row
        .project
        .as_deref()
        .map_or_else(|| "-".to_owned(), |path| short_path(path, home.as_deref()));

    let sensitive = if row.sensitive > 0 {
        row.sensitive.to_string()
    } else {
        "-".to_owned()
    };

    format!(
        "{:<8}  {started:<16}  {duration:<9}  {:>13}  {:>4}  {:>4}  {:>4}  {sensitive:>4}  {project}",
        &row.id[..8.min(row.id.len())],
        thousands(row.tokens),
        row.commands,
        row.files,
        row.mcp_calls,
    )
}

/// Renders a duration in milliseconds as a short human string.
fn format_duration(milliseconds: i64) -> String {
    let seconds = milliseconds / 1_000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", seconds % 60);
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// Column header for a sensitive-access listing.
pub(crate) fn notable_header() -> String {
    format!(
        "{:<17}  {:<8}  {:<9}  {:<8}  {}",
        "severity", "utc", "kind", "evidence", "path"
    )
}

/// One row of a sensitive-access listing.
pub(crate) fn notable_line(row: &Notable) -> String {
    let home = std::env::var("HOME").ok();
    format!(
        "{:<17}  {:<8}  {:<9}  {:<8}  {}",
        row.sensitivity,
        clock_time(row.timestamp_us),
        row.kind,
        row.evidence,
        short_path(&row.path, home.as_deref())
    )
}

/// The column header for a token breakdown.
pub(crate) fn group_header(by: &str) -> String {
    format!("{by:<34} {:>14} {:>7}", "tokens", "share")
}

/// Formats one row of a token breakdown.
pub(crate) fn group_line(label: &str, totals: &TokenTotals, overall: i64) -> String {
    let share = if overall > 0 {
        totals.total() as f64 * 100.0 / overall as f64
    } else {
        0.0
    };

    let home = std::env::var("HOME").ok();
    let shown = short_path(label, home.as_deref());
    let shown = if shown.chars().count() > 34 {
        let tail: String = shown.chars().skip(shown.chars().count() - 31).collect();
        format!("...{tail}")
    } else {
        shown
    };

    format!(
        "{shown:<34} {:>14} {share:>6.1}%",
        thousands(totals.total())
    )
}

/// The column header for a timeline listing.
pub(crate) fn header() -> String {
    format!(
        "{:<8}  {:<11}  {:<13}  {}",
        "utc", "agent", "event", "detail"
    )
}

/// Formats one event as a timeline line.
pub(crate) fn event_line(row: &EventRow) -> String {
    let time = clock_time(row.timestamp_us);
    let detail = detail(&row.kind, &row.payload);
    let home = std::env::var("HOME").ok();
    let project = row
        .project_path
        .as_deref()
        .map(|path| short_path(path, home.as_deref()))
        .unwrap_or_default();

    format!(
        "{time}  {:<11}  {:<13}  {detail}  {project}",
        row.agent_id, row.kind
    )
    .trim_end()
    .to_owned()
}

/// Renders a timestamp as local wall-clock time.
fn clock_time(micros: i64) -> String {
    Timestamp::from_micros(micros)
        .to_rfc3339()
        .split('T')
        .nth(1)
        .and_then(|time| time.get(..8))
        .map_or_else(|| micros.to_string(), str::to_owned)
}

/// Pulls the one interesting field out of an event payload.
///
/// Best effort by design: an event kind this build does not know how to
/// summarize still prints its kind and time rather than being hidden.
fn detail(kind: &str, payload: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return String::new();
    };

    let field = match kind {
        "file.read" | "file.write" => "path",
        "command" => "command",
        "tool.call" => "tool",
        "unknown" => "label",
        "prompt" => return format!("{} chars", string_field(&value, "char_count")),
        "mcp.call" => {
            return format!(
                "{}.{}",
                string_field(&value, "server"),
                string_field(&value, "tool")
            );
        }
        "collection.paused" => return "collection PAUSED by the user".to_owned(),
        "collection.resumed" => return "collection resumed".to_owned(),
        "config.changed" => {
            // The transition, not the path, is the story: a timeline row saying
            // only that a file changed leaves the reader to work out whether
            // monitoring is still running.
            let present = value
                .get("hooks_present")
                .and_then(serde_json::Value::as_bool);
            let state = match present {
                Some(true) => "hooks present",
                Some(false) => "OUR HOOKS REMOVED — collection stopped",
                None => "changed",
            };
            let home = std::env::var("HOME").ok();
            return format!(
                "{state}  {}",
                short_path(&string_field(&value, "path"), home.as_deref())
            );
        }
        _ => return String::new(),
    };

    string_field(&value, field)
}

/// Reads a field as a display string, whatever its JSON type.
fn string_field(value: &serde_json::Value, field: &str) -> String {
    match value.get(field) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Shortens a home-relative path for display.
///
/// Takes the home directory as an argument rather than reading it, so the
/// behaviour is testable without mutating process-wide environment state.
pub(crate) fn short_path(path: &str, home: Option<&str>) -> String {
    match home
        .filter(|home| !home.is_empty())
        .and_then(|home| path.strip_prefix(home))
    {
        Some(rest) => format!("~{rest}"),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, payload: &str) -> EventRow {
        EventRow {
            timestamp_us: 1_755_000_000_000_000,
            agent_id: "claude-code".to_owned(),
            kind: kind.to_owned(),
            evidence: "hook".to_owned(),
            project_path: Some("/work/acme".to_owned()),
            payload: payload.to_owned(),
        }
    }

    #[test]
    fn a_duration_reads_in_the_largest_useful_unit() {
        assert_eq!(format_duration(45_000), "45s");
        assert_eq!(format_duration(125_000), "2m 5s");
        assert_eq!(format_duration(7_500_000), "2h 5m");
    }

    #[test]
    fn a_running_session_says_so_rather_than_showing_a_dash() {
        let row = SessionRow {
            id: "abcdef1234".to_owned(),
            agent_id: "claude-code".to_owned(),
            project: Some("/work/acme".to_owned()),
            git_branch: None,
            started_at_us: Some(1_755_000_000_000_000),
            duration_ms: None,
            status: "active".to_owned(),
            tokens: 1_000,
            responses: 2,
            commands: 3,
            files: 4,
            mcp_calls: 0,
            sensitive: 0,
        };
        let line = session_line(&row);
        assert!(line.contains("running"), "{line}");
        assert!(line.contains("abcdef12"), "{line}");
    }

    #[test]
    fn a_session_that_was_never_watched_says_so() {
        let row = SessionRow {
            id: "abcdef1234".to_owned(),
            agent_id: "claude-code".to_owned(),
            project: None,
            git_branch: None,
            started_at_us: Some(1_755_000_000_000_000),
            duration_ms: None,
            status: "unknown".to_owned(),
            tokens: 5,
            responses: 1,
            commands: 0,
            files: 0,
            mcp_calls: 0,
            sensitive: 0,
        };
        let line = session_line(&row);
        assert!(line.contains('?'), "{line}");
        assert!(
            !line.contains("running"),
            "an imported session is not running"
        );
    }

    #[test]
    fn a_session_with_no_sensitive_access_shows_a_dash_not_a_zero() {
        let row = SessionRow {
            id: "abcdef1234".to_owned(),
            agent_id: "claude-code".to_owned(),
            project: None,
            git_branch: None,
            started_at_us: None,
            duration_ms: Some(1_000),
            status: "ended".to_owned(),
            tokens: 0,
            responses: 0,
            commands: 0,
            files: 0,
            mcp_calls: 0,
            sensitive: 0,
        };
        assert!(
            session_line(&row).contains(" -"),
            "a zero count should read as absent"
        );
    }

    #[test]
    fn the_security_caveat_names_both_of_its_blind_spots() {
        assert!(SECURITY_CAVEAT.contains("classified by name"));
        assert!(SECURITY_CAVEAT.contains("scanned, not parsed"));
    }

    #[test]
    fn a_config_change_that_removed_our_hooks_says_so_loudly() {
        let line = detail(
            "config.changed",
            r#"{"path":"/Users/dev/.claude/settings.json","hooks_present":false}"#,
        );
        assert!(line.contains("REMOVED"), "{line}");
        assert!(line.contains("collection stopped"), "{line}");
    }

    #[test]
    fn a_config_change_that_kept_our_hooks_is_stated_plainly() {
        let line = detail(
            "config.changed",
            r#"{"path":"/Users/dev/.claude/settings.json","hooks_present":true}"#,
        );
        assert!(line.contains("hooks present"), "{line}");
        assert!(!line.contains("REMOVED"), "{line}");
    }

    #[test]
    fn separates_thousands_for_negative_drift() {
        assert_eq!(thousands(-12_345), "-12,345");
    }

    #[test]
    fn a_group_line_shows_its_share_of_the_whole() {
        let totals = TokenTotals {
            input: 10,
            cache_creation: 0,
            cache_read: 0,
            output: 10,
            responses: 1,
        };
        let line = group_line("/work/acme", &totals, 80);
        assert!(line.contains("25.0%"), "{line}");
    }

    #[test]
    fn a_group_line_survives_a_zero_total() {
        let totals = TokenTotals::default();
        assert!(group_line("/work", &totals, 0).contains("0.0%"));
    }

    #[test]
    fn a_long_project_path_is_truncated_from_the_left() {
        let totals = TokenTotals::default();
        let line = group_line(
            "/very/long/path/that/keeps/going/and/going/to/the/end",
            &totals,
            1,
        );
        assert!(line.starts_with("..."), "{line}");
    }

    #[test]
    fn the_header_labels_the_clock_as_utc() {
        assert!(header().starts_with("utc"), "{}", header());
    }

    #[test]
    fn shows_the_path_for_a_file_event() {
        let line = event_line(&row(
            "file.read",
            r#"{"kind":"file.read","path":"/src/a.rs"}"#,
        ));
        assert!(line.contains("/src/a.rs"), "{line}");
        assert!(line.contains("file.read"), "{line}");
    }

    #[test]
    fn shows_the_command_for_a_command_event() {
        let line = event_line(&row(
            "command",
            r#"{"kind":"command","command":"cargo test"}"#,
        ));
        assert!(line.contains("cargo test"), "{line}");
    }

    #[test]
    fn joins_server_and_tool_for_an_mcp_call() {
        let line = event_line(&row(
            "mcp.call",
            r#"{"kind":"mcp.call","server":"github","tool":"get_issue"}"#,
        ));
        assert!(line.contains("github.get_issue"), "{line}");
    }

    #[test]
    fn shows_a_character_count_for_a_prompt_and_never_text() {
        let line = event_line(&row("prompt", r#"{"kind":"prompt","char_count":42}"#));
        assert!(line.contains("42 chars"), "{line}");
    }

    #[test]
    fn survives_a_payload_it_cannot_parse() {
        let line = event_line(&row("command", "not json"));
        assert!(line.contains("command"), "{line}");
    }

    #[test]
    fn abbreviates_the_home_directory() {
        assert_eq!(
            short_path("/Users/dev/projects/acme", Some("/Users/dev")),
            "~/projects/acme"
        );
    }

    #[test]
    fn leaves_paths_outside_home_alone() {
        assert_eq!(short_path("/opt/other", Some("/Users/dev")), "/opt/other");
    }

    #[test]
    fn leaves_paths_alone_when_home_is_unknown() {
        assert_eq!(short_path("/Users/dev/x", None), "/Users/dev/x");
    }
}
