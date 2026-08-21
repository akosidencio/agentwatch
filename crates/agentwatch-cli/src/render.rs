//! Turning rows into lines.

use agentwatch_storage::{
    EventRow, Notable, ReceiptCommand, ReceiptFile, ReceiptTokenGroup, SessionRow, TokenTotals,
};
use agentwatch_types::Timestamp;
pub(crate) use agentwatch_types::thousands;

use crate::theme;

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
    theme::paint(
        &format!(
            "{:<8}  {:<11}  {:<18}  {:<16}  {:<9}  {:>13}  {:>4}  {:>4}  {:>4}  {:>4}  {:<13}  {}",
            "session",
            "agent",
            "model",
            "started",
            "duration",
            "tokens",
            "cmd",
            "file",
            "mcp",
            "sens",
            "surface",
            "project"
        ),
        theme::MUTED,
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
    let mut project = row
        .project
        .as_deref()
        .map_or_else(|| "-".to_owned(), |path| short_path(path, home.as_deref()));
    if row.projects > 1 {
        project.push_str(&format!(" (+{} paths)", row.projects - 1));
    }

    let sensitive = if row.sensitive > 0 {
        row.sensitive.to_string()
    } else {
        "-".to_owned()
    };

    // Never observed, rather than a default. Only transcripts carry the field,
    // so a session seen purely through hooks has none until it is reconciled.
    let surface = row.surface.as_deref().unwrap_or("?");
    let model = row.model.as_deref().unwrap_or("?");

    // Painted per column, not per line: the identity, the liveness, and the
    // one number worth a second look each carry their own meaning, and a line
    // in a single colour says only "this is a session".
    let id = theme::paint(
        &format!("{:<8}", &row.id[..8.min(row.id.len())]),
        theme::ACCENT,
    );
    let duration = theme::paint(
        &format!("{duration:<9}"),
        if row.status == "active" {
            theme::GOOD
        } else {
            theme::MUTED
        },
    );
    let sensitive = theme::paint(
        &format!("{sensitive:>4}"),
        if row.sensitive > 0 {
            theme::BAD
        } else {
            theme::MUTED
        },
    );
    let surface = theme::paint(&format!("{surface:<13}"), theme::MUTED);
    let started = theme::paint(&format!("{started:<16}"), theme::MUTED);

    format!(
        "{id}  {:<11}  {model:<18}  {started}  {duration}  {:>13}  {:>4}  {:>4}  {:>4}  {sensitive}  {surface}  {project}",
        row.agent_id,
        thousands(row.tokens),
        row.commands,
        row.files,
        row.mcp_calls,
    )
}

/// Renders a duration in milliseconds as a short human string.
pub(crate) fn format_duration(milliseconds: i64) -> String {
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

/// Column header for the files section of a session receipt.
pub(crate) fn receipt_file_header() -> String {
    theme::paint(
        &format!(
            "{:>5}  {:>5}  {:<8}  {:<20}  {}",
            "read", "write", "last utc", "tools", "path"
        ),
        theme::MUTED,
    )
}

/// One aggregated file row in a session receipt.
pub(crate) fn receipt_file_line(row: &ReceiptFile) -> String {
    let home = std::env::var("HOME").ok();
    let path = scoped_path(&row.path, row.project.as_deref(), home.as_deref());
    format!(
        "{:>5}  {:>5}  {:<8}  {:<20}  {}",
        row.reads,
        row.writes,
        clock_time(row.last_seen_us),
        row.tools,
        path
    )
}

/// Column header for the commands section of a session receipt.
pub(crate) fn receipt_command_header() -> String {
    theme::paint(
        &format!("{:<8}  {:<17}  {}", "utc", "sensitivity", "command"),
        theme::MUTED,
    )
}

/// One command row in a session receipt.
pub(crate) fn receipt_command_line(row: &ReceiptCommand) -> String {
    let command = row.command.replace(['\r', '\n'], " ");
    let detail = row
        .description
        .as_deref()
        .filter(|description| !description.is_empty())
        .map_or_else(String::new, |description| format!(" — {description}"));
    let home = std::env::var("HOME").ok();
    let project = row.project.as_deref().map_or_else(String::new, |project| {
        format!("  [{}]", short_path(project, home.as_deref()))
    });
    format!(
        "{:<8}  {:<17}  {command}{detail}{project}",
        clock_time(row.timestamp_us),
        row.sensitivity
    )
}

/// Column header for the detailed token section of a session receipt.
pub(crate) fn receipt_token_header() -> String {
    theme::paint(
        &format!(
            "{:<9}  {:<24}  {:>12}  {:>12}  {:>12}  {:>12}  {:>12}  {:>9}",
            "role", "model", "input", "cache new", "cache read", "output", "total", "responses"
        ),
        theme::MUTED,
    )
}

/// One model/role token row in a session receipt.
pub(crate) fn receipt_token_line(row: &ReceiptTokenGroup) -> String {
    format!(
        "{:<9}  {:<24}  {:>12}  {:>12}  {:>12}  {:>12}  {:>12}  {:>9}",
        if row.is_subagent { "subagent" } else { "main" },
        row.model,
        thousands(row.totals.input),
        thousands(row.totals.cache_creation),
        thousands(row.totals.cache_read),
        thousands(row.totals.output),
        thousands(row.totals.total()),
        thousands(row.totals.responses),
    )
}

/// Column header for a sensitive-access listing.
pub(crate) fn notable_header() -> String {
    theme::paint(
        &format!(
            "{:<17}  {:<8}  {:<9}  {:<10}  {}",
            "severity", "utc", "kind", "evidence", "path"
        ),
        theme::MUTED,
    )
}

/// One row of a sensitive-access listing.
pub(crate) fn notable_line(row: &Notable) -> String {
    let home = std::env::var("HOME").ok();
    let path = scoped_path(&row.path, row.project.as_deref(), home.as_deref());
    // Severity is the column people scan, so it is the column that carries the
    // colour. Unknown severities stay unpainted rather than being guessed at.
    let severity_colour = match row.sensitivity.to_string().as_str() {
        "critical" | "high" | "highly_sensitive" => theme::BAD,
        "medium" | "sensitive" => theme::WARN,
        _ => theme::MUTED,
    };
    format!(
        "{}  {}  {:<9}  {}  {}",
        theme::paint(&format!("{:<17}", row.sensitivity), severity_colour),
        theme::paint(
            &format!("{:<8}", clock_time(row.timestamp_us)),
            theme::MUTED
        ),
        row.kind,
        theme::paint(&format!("{:<10}", row.evidence), theme::MUTED),
        path
    )
}

/// Honest limitations for one adapter's session receipt.
///
/// Unknown adapters receive only product-wide limitations. This makes a new
/// adapter useful immediately without attributing Codex- or Claude-specific
/// blind spots to it.
pub(crate) fn coverage_gaps(agent_id: &str, branch_known: bool) -> Vec<String> {
    let mut gaps = Vec::new();
    if !branch_known {
        gaps.push("git branch     not reported by the source log".to_owned());
    }
    if agent_id == "codex" {
        gaps.push(
            "Codex files    rollout logs expose patch writes, but not direct file-read events"
                .to_owned(),
        );
        gaps.push(
            "Codex MCP      rollout calls are generic tools unless Codex identifies MCP metadata"
                .to_owned(),
        );
    }
    gaps.push("network        not collected".to_owned());
    gaps.push("processes      not collected".to_owned());
    gaps.push("prompt content disabled by design".to_owned());
    gaps
}

/// Shows a relative path together with the project that gives it meaning.
fn scoped_path(path: &str, project: Option<&str>, home: Option<&str>) -> String {
    let path = short_path(path, home);
    if std::path::Path::new(&path).is_absolute() || path.starts_with("~/") {
        return path;
    }
    project.map_or(path.clone(), |project| {
        format!("{path}  [{}]", short_path(project, home))
    })
}

/// The column header for a token breakdown.
pub(crate) fn group_header(by: &str) -> String {
    // Trimmed before it is painted: trim_end on a painted string finds the
    // reset sequence, not the spaces in front of it, and removes nothing.
    theme::paint(
        format!("{by:<34} {:>14} {:>7}", "tokens", "share").trim_end(),
        theme::MUTED,
    )
}

/// How wide the share bar is drawn, in columns.
const SHARE_BAR: usize = 8;

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

    // The percentage stays: the bar is for ranking at a glance, and eight
    // columns cannot distinguish 3% from 4% no matter how it is drawn.
    format!(
        "{shown:<34} {:>14} {} {}",
        thousands(totals.total()),
        theme::paint(&format!("{share:>6.1}%"), theme::MUTED),
        // Trimmed before painting for the same reason as the header: the bar
        // pads to a fixed width for the TUI's columns, and a listing that is
        // piped must not differ from one that is not.
        theme::paint(
            theme::bar(share / 100.0, SHARE_BAR).trim_end(),
            theme::ACCENT
        )
    )
    .trim_end()
    .to_owned()
}

/// The column header for a timeline listing.
pub(crate) fn header() -> String {
    theme::paint(
        &format!(
            "{:<8}  {:<11}  {:<13}  {}",
            "utc", "agent", "event", "detail"
        ),
        theme::MUTED,
    )
}

/// The palette entry for one event kind.
///
/// Shared with the TUI's activity list so the same event is the same colour in
/// both surfaces. An unrecognised kind is deliberately left unpainted.
pub(crate) const fn kind_colour(kind: &str) -> Option<u8> {
    match kind.as_bytes() {
        b"command" => Some(theme::ACCENT),
        b"file.write" => Some(theme::VIOLET),
        b"mcp.call" => Some(theme::TEAL),
        b"session.started" | b"session.ended" => Some(theme::GOOD),
        b"config.changed" => Some(theme::BAD),
        b"token.usage" | b"file.read" => Some(theme::MUTED),
        _ => None,
    }
}

/// One event as a timeline line, coloured for a terminal.
///
/// The TUI does not go through here: it builds spans from [`event_segments`]
/// directly, because a ratatui buffer draws escape sequences as the literal
/// characters they are instead of interpreting them.
pub(crate) fn event_line_painted(row: &EventRow) -> String {
    let segments = event_segments(row);
    let kind = format!("{:<13}", segments.kind);

    format!(
        "{}  {}  {}  {}  {}",
        theme::paint(&segments.time, theme::MUTED),
        theme::paint(&format!("{:<11}", segments.agent), theme::MUTED),
        kind_colour(&segments.kind)
            .map_or_else(|| kind.clone(), |colour| theme::paint(&kind, colour)),
        segments.detail,
        // Painted only when there is something to paint: an escape pair around
        // an empty string survives trim_end and leaves codes on a bare line.
        if segments.project.is_empty() {
            String::new()
        } else {
            theme::paint(&segments.project, theme::MUTED)
        }
    )
    .trim_end()
    .to_owned()
}

/// The fields a timeline line is built from.
///
/// Exists so the plain form, the painted form, and the TUI's spans all agree
/// on what a row says without any of them re-deriving it.
pub(crate) struct EventSegments {
    /// Local wall-clock time.
    pub(crate) time: String,
    /// Which agent produced the event.
    pub(crate) agent: String,
    /// The event kind, verbatim.
    pub(crate) kind: String,
    /// The one interesting field, if this build knows how to summarize it.
    pub(crate) detail: String,
    /// The project path, shortened, or empty.
    pub(crate) project: String,
}

/// Breaks an event into its display fields.
pub(crate) fn event_segments(row: &EventRow) -> EventSegments {
    let home = std::env::var("HOME").ok();
    EventSegments {
        time: clock_time(row.timestamp_us),
        agent: row.agent_id.clone(),
        kind: row.kind.clone(),
        detail: detail(&row.kind, &row.payload),
        project: row
            .project_path
            .as_deref()
            .map(|path| short_path(path, home.as_deref()))
            .unwrap_or_default(),
    }
}

/// Renders a timestamp as local wall-clock time.
pub(crate) fn clock_time(micros: i64) -> String {
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

    /// A row's fields joined without colour.
    ///
    /// These cases assert on what a timeline line *says* — including that an
    /// unsummarizable payload still leaves the kind visible — so they need the
    /// whole line, not just the detail field.
    fn event_text(row: &EventRow) -> String {
        let segments = event_segments(row);
        format!(
            "{}  {}  {}  {}  {}",
            segments.time, segments.agent, segments.kind, segments.detail, segments.project
        )
    }

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
    fn receipt_token_rows_keep_models_roles_and_all_counters_visible() {
        let row = ReceiptTokenGroup {
            model: "gpt-5.6-codex".to_owned(),
            is_subagent: true,
            totals: TokenTotals {
                input: 10,
                cache_creation: 20,
                cache_read: 30,
                output: 40,
                responses: 2,
            },
        };
        let line = receipt_token_line(&row);

        assert!(line.contains("subagent"), "{line}");
        assert!(line.contains("gpt-5.6-codex"), "{line}");
        for count in ["10", "20", "30", "40", "100", "2"] {
            assert!(line.contains(count), "missing {count} in {line}");
        }
    }

    #[test]
    fn receipt_commands_cannot_break_the_table_with_embedded_newlines() {
        let row = ReceiptCommand {
            timestamp_us: 1_755_000_000_000_000,
            command: "printf first\nprintf second".to_owned(),
            description: Some("two writes".to_owned()),
            sensitivity: "normal".to_owned(),
            project: Some("/work/acme".to_owned()),
        };
        let line = receipt_command_line(&row);

        assert!(!line.contains('\n'), "{line:?}");
        assert!(line.contains("printf first printf second"), "{line}");
        assert!(line.contains("two writes"), "{line}");
        assert!(line.contains("[/work/acme]"), "{line}");
    }

    #[test]
    fn relative_sensitive_paths_are_scoped_to_their_project() {
        let row = Notable {
            timestamp_us: 1_755_000_000_000_000,
            sensitivity: "sensitive".to_owned(),
            kind: "command".to_owned(),
            path: ".env".to_owned(),
            evidence: "derived".to_owned(),
            project: Some("/work/acme".to_owned()),
        };

        let line = notable_line(&row);
        assert!(line.contains(".env  [/work/acme]"), "{line}");
    }

    #[test]
    fn coverage_gaps_are_adapter_specific_but_safe_for_future_adapters() {
        let claude = coverage_gaps("claude-code", true);
        assert!(!claude.iter().any(|gap| gap.contains("Codex")));
        assert!(!claude.iter().any(|gap| gap.contains("git branch")));

        let codex = coverage_gaps("codex", false);
        assert!(codex.iter().any(|gap| gap.contains("file-read")));
        assert!(codex.iter().any(|gap| gap.contains("MCP")));
        assert!(codex.iter().any(|gap| gap.contains("git branch")));

        let future = coverage_gaps("future-agent", true);
        assert!(!future.iter().any(|gap| gap.contains("Codex")));
        assert!(future.iter().any(|gap| gap.contains("network")));
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
            model: None,
            project: Some("/work/acme".to_owned()),
            projects: 1,
            is_subagent: false,
            git_branch: None,
            surface: Some("claude-vscode".to_owned()),
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
    fn the_surface_is_shown_when_known() {
        let row = SessionRow {
            id: "abcdef1234".to_owned(),
            agent_id: "claude-code".to_owned(),
            model: None,
            project: None,
            projects: 0,
            is_subagent: false,
            git_branch: None,
            surface: Some("claude-vscode".to_owned()),
            started_at_us: Some(1),
            duration_ms: Some(1_000),
            status: "ended".to_owned(),
            tokens: 0,
            responses: 0,
            commands: 0,
            files: 0,
            mcp_calls: 0,
            sensitive: 0,
        };
        assert!(session_line(&row).contains("claude-vscode"));
    }

    #[test]
    fn an_unobserved_surface_reads_as_unknown_not_as_a_default() {
        let row = SessionRow {
            id: "abcdef1234".to_owned(),
            agent_id: "claude-code".to_owned(),
            model: None,
            project: None,
            projects: 0,
            is_subagent: false,
            git_branch: None,
            surface: None,
            started_at_us: Some(1),
            duration_ms: Some(1_000),
            status: "ended".to_owned(),
            tokens: 0,
            responses: 0,
            commands: 0,
            files: 0,
            mcp_calls: 0,
            sensitive: 0,
        };
        let line = session_line(&row);
        assert!(line.contains('?'), "{line}");
        assert!(!line.contains("claude-vscode"), "must not invent a surface");
    }

    #[test]
    fn a_session_that_was_never_watched_says_so() {
        let row = SessionRow {
            id: "abcdef1234".to_owned(),
            agent_id: "claude-code".to_owned(),
            model: None,
            project: None,
            projects: 0,
            is_subagent: false,
            git_branch: None,
            surface: Some("claude-vscode".to_owned()),
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
            model: None,
            project: None,
            projects: 0,
            is_subagent: false,
            git_branch: None,
            surface: Some("claude-vscode".to_owned()),
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
        let line = event_text(&row(
            "file.read",
            r#"{"kind":"file.read","path":"/src/a.rs"}"#,
        ));
        assert!(line.contains("/src/a.rs"), "{line}");
        assert!(line.contains("file.read"), "{line}");
    }

    #[test]
    fn shows_the_command_for_a_command_event() {
        let line = event_text(&row(
            "command",
            r#"{"kind":"command","command":"cargo test"}"#,
        ));
        assert!(line.contains("cargo test"), "{line}");
    }

    #[test]
    fn joins_server_and_tool_for_an_mcp_call() {
        let line = event_text(&row(
            "mcp.call",
            r#"{"kind":"mcp.call","server":"github","tool":"get_issue"}"#,
        ));
        assert!(line.contains("github.get_issue"), "{line}");
    }

    #[test]
    fn shows_a_character_count_for_a_prompt_and_never_text() {
        let line = event_text(&row("prompt", r#"{"kind":"prompt","char_count":42}"#));
        assert!(line.contains("42 chars"), "{line}");
    }

    #[test]
    fn survives_a_payload_it_cannot_parse() {
        let line = event_text(&row("command", "not json"));
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
