//! Turning rows into lines.

use agentwatch_storage::{EventRow, TokenTotals};
use agentwatch_types::Timestamp;

/// Formats an integer with thousands separators.
///
/// Token counts run to ten digits; unseparated they are unreadable.
pub(crate) fn thousands(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();

    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }

    if negative { format!("-{out}") } else { out }
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
fn short_path(path: &str, home: Option<&str>) -> String {
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
    fn separates_thousands() {
        assert_eq!(thousands(1_507_299_516), "1,507,299,516");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(0), "0");
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
