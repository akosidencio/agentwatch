//! Reading token usage back out of Claude Code's own session transcript.
//!
//! Hooks are the fast path but they are lossy: the daemon can be restarted, a
//! session can predate it, and no hook reports token counts at all. The
//! transcript on disk is the durable record, so it is the correcting path.
//!
//! # The rule that makes the numbers right
//!
//! One API response appears in the transcript as **several** records — one per
//! content block, so a response containing thinking plus two tool calls is
//! three lines. Every one of those records repeats the usage for the whole
//! response.
//!
//! Measured over 40 real transcripts on the development machine, counting
//! records rather than responses inflates totals by **1.75x overall and 3.08x
//! on input tokens**. Deduplicating by `message.id` is therefore not a tidy-up:
//! it is the difference between a usable product and a wrong one.
//!
//! # What this reader never touches
//!
//! `message.content` and `toolUseResult` are not deserialized, so conversation
//! text and tool output never enter memory — the same discipline the hook
//! adapter applies, for the same reason.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

use agentwatch_events::{AgentEvent, Event, EvidenceSource, TokenUsageEvent};
use agentwatch_types::{AgentId, EventId, ExternalSessionId, Timestamp};
use serde::Deserialize;

/// The provider these transcripts describe.
const PROVIDER: &str = "anthropic";

/// Usage keys promoted to real fields. Everything else is kept verbatim.
const KNOWN_USAGE_KEYS: [&str; 4] = [
    "input_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "output_tokens",
];

/// A transcript that could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TranscriptError {
    /// The file could not be opened or read.
    #[error("could not read the transcript")]
    Io(#[from] std::io::Error),
}

/// What a single pass over a transcript found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptSummary {
    /// Lines examined.
    pub lines: u64,
    /// Lines that were not valid JSON, and were skipped.
    pub unparseable_lines: u64,
    /// Assistant records carrying usage.
    pub usage_records: u64,
    /// Distinct responses those records represent.
    pub responses: u64,
}

impl TranscriptSummary {
    /// How much a naive per-record count would have over-reported.
    ///
    /// `1.0` means every response was a single record. Returns `None` when
    /// there were no responses to compare.
    #[must_use]
    pub fn record_inflation(&self) -> Option<f64> {
        (self.responses > 0).then(|| self.usage_records as f64 / self.responses as f64)
    }
}

/// Where Claude Code keeps its transcripts, relative to the home directory.
const TRANSCRIPT_ROOT: [&str; 2] = [".claude", "projects"];

/// Returns the directory holding every transcript.
///
/// # Errors
///
/// Returns an error if `HOME` is not set.
pub fn transcript_root() -> Result<std::path::PathBuf, TranscriptError> {
    let home = std::env::var_os("HOME").ok_or_else(|| std::io::Error::other("HOME is not set"))?;
    Ok(TRANSCRIPT_ROOT
        .iter()
        .fold(std::path::PathBuf::from(home), |path, part| path.join(part)))
}

/// Derives the transcript path for a session from its working directory.
///
/// Claude Code names the directory after the project path with separators
/// replaced by dashes, and the file after the session id. Prefer the path the
/// `SessionStart` hook reported when there is one — this is the fallback for
/// sessions whose start we never saw, and it depends on an undocumented naming
/// rule that could change.
#[must_use]
pub fn derived_transcript_path(
    root: &Path,
    project_path: &str,
    session_id: &str,
) -> std::path::PathBuf {
    let slug: String = project_path
        .chars()
        .map(|c| if c == '/' { '-' } else { c })
        .collect();
    root.join(slug).join(format!("{session_id}.jsonl"))
}

/// Deepest directory nesting [`find_transcripts`] will descend into.
///
/// A guard against pathological trees, not a real limit: the layout is two
/// levels deep and nothing legitimate approaches this.
const MAX_SEARCH_DEPTH: usize = 16;

/// Finds every transcript beneath a directory.
///
/// Returns paths only; reading them is the caller's decision, since a full
/// history import and a single-session reconcile want the same discovery but
/// very different insertion behaviour.
#[must_use]
pub fn find_transcripts(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    collect_transcripts(root, MAX_SEARCH_DEPTH, &mut found);
    found.sort();
    found
}

/// Recursive half of [`find_transcripts`].
///
/// Symlinked directories are not followed and the depth is bounded. Both matter
/// for the same reason: this walks a directory the user can put anything in,
/// and a symlink pointing at its own parent would otherwise recurse until the
/// stack ran out.
fn collect_transcripts(directory: &Path, depth: usize, found: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        // `DirEntry::file_type` does not follow symlinks, so a link to a
        // directory is neither descended into nor mistaken for a transcript.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();

        if kind.is_dir() {
            if let Some(remaining) = depth.checked_sub(1) {
                collect_transcripts(&path, remaining, found);
            }
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        {
            found.push(path);
        }
    }
}

/// Reads token usage events from one transcript file.
///
/// Returns one event per distinct model response, in file order.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read. Individual malformed
/// lines are counted and skipped rather than failing the pass: a transcript
/// being appended to while we read it will legitimately end mid-line.
pub fn read_token_usage(
    path: impl AsRef<Path>,
) -> Result<(Vec<AgentEvent>, TranscriptSummary), TranscriptError> {
    let file = std::fs::File::open(path.as_ref())?;
    read_token_usage_from(BufReader::new(file))
}

/// Reads token usage from any source of transcript lines.
///
/// # Errors
///
/// Returns an error if the underlying reader fails.
pub fn read_token_usage_from<R: BufRead>(
    reader: R,
) -> Result<(Vec<AgentEvent>, TranscriptSummary), TranscriptError> {
    let mut summary = TranscriptSummary::default();
    let mut events = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        summary.lines += 1;

        let Ok(mut record) = serde_json::from_str::<TranscriptRecord>(&line) else {
            summary.unparseable_lines += 1;
            continue;
        };

        if record.kind.as_deref() != Some("assistant") {
            continue;
        }
        let Some(message) = record.message.take() else {
            continue;
        };
        let Some(usage) = message.usage else { continue };
        summary.usage_records += 1;

        // Prefer the message id: it identifies the response. `requestId` is a
        // usable fallback but is absent on some records.
        let Some(key) = message.id.or_else(|| record.request_id.clone()) else {
            continue;
        };
        if !seen.insert(key.clone()) {
            continue;
        }
        summary.responses += 1;

        events.push(to_event(&record, message.model, key, usage));
    }

    Ok((events, summary))
}

/// Builds one normalized event from a deduplicated response.
fn to_event(
    record: &TranscriptRecord,
    model: Option<String>,
    key: String,
    usage: serde_json::Map<String, serde_json::Value>,
) -> AgentEvent {
    let mut token_usage = TokenUsageEvent {
        provider: PROVIDER.to_owned(),
        model,
        request_id: Some(key.clone()),
        input_tokens: count(&usage, "input_tokens"),
        cache_creation_input_tokens: count(&usage, "cache_creation_input_tokens"),
        cache_read_input_tokens: count(&usage, "cache_read_input_tokens"),
        output_tokens: count(&usage, "output_tokens"),
        is_subagent: record.is_sidechain,
        provider_usage: serde_json::Map::new(),
    };

    for (key, value) in usage {
        if !KNOWN_USAGE_KEYS.contains(&key.as_str()) {
            token_usage.provider_usage.insert(key, value);
        }
    }

    // Deterministic: reconciling the same transcript twice must produce the
    // same event, not a second copy of it.
    let mut event = AgentEvent::observed(
        AgentId::CLAUDE_CODE,
        EvidenceSource::Transcript,
        Event::TokenUsage(token_usage),
    )
    .with_id(EventId::from_key(&AgentId::CLAUDE_CODE, &key));

    if let Some(timestamp) = record
        .timestamp
        .as_deref()
        .and_then(|t| Timestamp::parse_rfc3339(t).ok())
    {
        event = event.at(timestamp);
    }
    if let Some(session) = record.session_id.clone() {
        event = event.with_session(ExternalSessionId::from(session));
    }
    if let Some(cwd) = record.cwd.clone() {
        event = event.with_project_path(cwd);
    }

    event
        .with_git_branch(record.git_branch.clone())
        .with_surface(record.entrypoint.clone())
}

/// Reads a usage counter, treating anything non-numeric as absent.
fn count(usage: &serde_json::Map<String, serde_json::Value>, key: &str) -> u64 {
    usage
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// One transcript line.
///
/// Every field is optional and unknown keys are ignored: the transcript is an
/// undocumented format carrying at least a dozen record types, most of which
/// this reader has no interest in.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TranscriptRecord {
    /// The record type: `assistant`, `user`, and many others.
    #[serde(rename = "type")]
    kind: Option<String>,
    /// The provider's request identifier.
    request_id: Option<String>,
    /// When the record was written, RFC 3339.
    timestamp: Option<String>,
    /// The agent's session identifier.
    session_id: Option<String>,
    /// Working directory at the time.
    cwd: Option<String>,
    /// Git branch at the time, when the session was inside a repository.
    git_branch: Option<String>,
    /// Which surface the session ran in, for example `claude-vscode`.
    ///
    /// Present on every record of every transcript observed so far, but read as
    /// optional like everything else here — the format is undocumented and this
    /// reader must not start failing if a field disappears.
    entrypoint: Option<String>,
    /// Whether this turn belongs to a subagent rather than the main thread.
    is_sidechain: bool,
    /// The model message.
    message: Option<TranscriptMessage>,
}

/// The subset of a model message worth reading.
///
/// `content` is deliberately absent: not deserializing it is what guarantees
/// conversation text cannot reach storage.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TranscriptMessage {
    /// The provider's identifier for this response, and our dedup key.
    id: Option<String>,
    /// The exact model identifier.
    model: Option<String>,
    /// Reported usage, kept as a map so unknown categories survive.
    usage: Option<serde_json::Map<String, serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Two records for one response, exactly as the real format writes them.
    const ONE_RESPONSE_TWO_RECORDS: &str = r#"
{"type":"assistant","requestId":"req_1","timestamp":"2026-08-20T17:22:02.051Z","sessionId":"s-1","cwd":"/work","gitBranch":"main","message":{"id":"msg_1","model":"claude-opus-5","content":[{"type":"thinking"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":5585,"cache_read_input_tokens":45723,"output_tokens":497,"service_tier":"standard"}}}
{"type":"assistant","requestId":"req_1","timestamp":"2026-08-20T17:22:02.051Z","sessionId":"s-1","cwd":"/work","gitBranch":"main","message":{"id":"msg_1","model":"claude-opus-5","content":[{"type":"tool_use"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":5585,"cache_read_input_tokens":45723,"output_tokens":497,"service_tier":"standard"}}}
"#;

    fn read(text: &str) -> (Vec<AgentEvent>, TranscriptSummary) {
        read_token_usage_from(Cursor::new(text)).expect("reads")
    }

    #[test]
    fn derives_the_transcript_path_from_the_project_and_session() {
        let path = derived_transcript_path(
            Path::new("/home/.claude/projects"),
            "/Users/dev/Documents/BOOKSPINE",
            "d6dc6c78-2189-4e9f-bb43-49b53adaa441",
        );
        assert_eq!(
            path,
            Path::new("/home/.claude/projects")
                .join("-Users-dev-Documents-BOOKSPINE")
                .join("d6dc6c78-2189-4e9f-bb43-49b53adaa441.jsonl")
        );
    }

    #[test]
    fn finds_transcripts_beneath_a_directory() {
        let directory = tempfile::tempdir().expect("temp dir");
        let project = directory.path().join("-work-acme");
        std::fs::create_dir_all(&project).expect("create");
        std::fs::write(project.join("a.jsonl"), "").expect("write");
        std::fs::write(project.join("notes.txt"), "").expect("write");

        let found = find_transcripts(directory.path());
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("a.jsonl"));
    }

    #[test]
    fn a_symlink_loop_does_not_run_the_search_off_the_stack() {
        let directory = tempfile::tempdir().expect("temp dir");
        let project = directory.path().join("-work-acme");
        std::fs::create_dir_all(&project).expect("create");
        std::fs::write(project.join("a.jsonl"), "").expect("write");

        // A link pointing back at its own ancestor: the shape that used to
        // recurse until the stack ran out.
        std::os::unix::fs::symlink(directory.path(), project.join("loop")).expect("symlink");

        let found = find_transcripts(directory.path());
        assert_eq!(found.len(), 1, "the real transcript, exactly once");
    }

    #[test]
    fn counts_one_response_once_even_though_it_spans_records() {
        let (events, summary) = read(ONE_RESPONSE_TWO_RECORDS);

        assert_eq!(summary.usage_records, 2);
        assert_eq!(summary.responses, 1);
        assert_eq!(events.len(), 1, "duplicate records must not double-count");
    }

    #[test]
    fn reports_how_much_naive_counting_would_have_inflated() {
        let (_, summary) = read(ONE_RESPONSE_TWO_RECORDS);
        assert_eq!(summary.record_inflation(), Some(2.0));
    }

    #[test]
    fn keeps_the_four_counters_apart() {
        let (events, _) = read(ONE_RESPONSE_TWO_RECORDS);
        let Event::TokenUsage(usage) = &events[0].event else {
            panic!("expected token usage")
        };

        assert_eq!(usage.input_tokens, 2);
        assert_eq!(usage.cache_creation_input_tokens, 5_585);
        assert_eq!(usage.cache_read_input_tokens, 45_723);
        assert_eq!(usage.output_tokens, 497);
        assert_eq!(usage.total(), 51_807);
    }

    #[test]
    fn preserves_usage_categories_it_does_not_model() {
        let (events, _) = read(ONE_RESPONSE_TWO_RECORDS);
        let Event::TokenUsage(usage) = &events[0].event else {
            panic!("expected token usage")
        };

        assert_eq!(
            usage
                .provider_usage
                .get("service_tier")
                .and_then(serde_json::Value::as_str),
            Some("standard")
        );
        assert!(!usage.provider_usage.contains_key("input_tokens"));
    }

    #[test]
    fn carries_session_project_model_and_time() {
        let (events, _) = read(ONE_RESPONSE_TWO_RECORDS);
        let event = &events[0];

        assert_eq!(event.evidence, EvidenceSource::Transcript);
        assert_eq!(event.project_path.as_deref(), Some("/work"));
        assert!(event.session_id.is_some());
        assert_eq!(
            event.timestamp,
            Timestamp::parse_rfc3339("2026-08-20T17:22:02.051Z").unwrap()
        );

        let Event::TokenUsage(usage) = &event.event else {
            panic!("expected token usage")
        };
        assert_eq!(usage.model.as_deref(), Some("claude-opus-5"));
    }

    #[test]
    fn reading_the_same_transcript_twice_produces_identical_events() {
        let (first, _) = read(ONE_RESPONSE_TWO_RECORDS);
        let (second, _) = read(ONE_RESPONSE_TWO_RECORDS);
        assert_eq!(
            first[0].id, second[0].id,
            "a reconcile pass must be idempotent"
        );
    }

    #[test]
    fn carries_the_git_branch_when_the_transcript_has_one() {
        let (events, _) = read(ONE_RESPONSE_TWO_RECORDS);
        assert_eq!(events[0].git_branch.as_deref(), Some("main"));
    }

    #[test]
    fn marks_subagent_turns() {
        let text = r#"
{"type":"assistant","isSidechain":true,"timestamp":"2026-08-20T17:22:02.051Z","message":{"id":"m1","model":"x","usage":{"output_tokens":5}}}
"#;
        let (events, _) = read(text);
        let Event::TokenUsage(usage) = &events[0].event else {
            panic!("expected token usage")
        };
        assert!(usage.is_subagent);
    }

    #[test]
    fn ignores_record_types_it_does_not_care_about() {
        let text = r#"
{"type":"user","message":{"role":"user"}}
{"type":"last-prompt","promptId":"p1"}
{"type":"file-history-snapshot"}
{"type":"summary","summary":"something"}
"#;
        let (events, summary) = read(text);
        assert!(events.is_empty());
        assert_eq!(summary.usage_records, 0);
        assert_eq!(summary.lines, 4);
    }

    #[test]
    fn skips_a_truncated_final_line_rather_than_failing() {
        let text = format!("{ONE_RESPONSE_TWO_RECORDS}\n{{\"type\":\"assist");
        let (events, summary) = read(&text);

        assert_eq!(events.len(), 1, "the good records should still be read");
        assert_eq!(summary.unparseable_lines, 1);
    }

    #[test]
    fn never_carries_conversation_text_or_tool_output() {
        let text = r#"
{"type":"assistant","timestamp":"2026-08-20T17:22:02.051Z","sessionId":"s-1","message":{"id":"msg_9","model":"claude-opus-5","content":[{"type":"text","text":"the launch code is hunter2"}],"usage":{"input_tokens":1,"output_tokens":1}}}
{"type":"user","toolUseResult":{"stdout":"AKIAIOSFODNN7EXAMPLE"}}
"#;
        let (events, _) = read(text);
        let encoded = serde_json::to_string(&events).expect("serializable");

        assert!(
            !encoded.contains("hunter2"),
            "conversation text must never be stored"
        );
        assert!(
            !encoded.contains("AKIA"),
            "tool output must never be stored"
        );
    }

    #[test]
    fn falls_back_to_the_request_id_when_a_message_has_no_id() {
        let text = r#"
{"type":"assistant","requestId":"req_only","timestamp":"2026-08-20T17:22:02.051Z","message":{"model":"m","usage":{"output_tokens":5}}}
"#;
        let (events, summary) = read(text);
        assert_eq!(summary.responses, 1);

        let Event::TokenUsage(usage) = &events[0].event else {
            panic!("expected token usage")
        };
        assert_eq!(usage.request_id.as_deref(), Some("req_only"));
    }

    #[test]
    fn an_assistant_record_without_usage_is_not_a_response() {
        let text = r#"{"type":"assistant","message":{"id":"m1","model":"x"}}"#;
        let (events, summary) = read(text);
        assert!(events.is_empty());
        assert_eq!(summary.usage_records, 0);
    }

    #[test]
    fn treats_a_non_numeric_counter_as_zero() {
        let text = r#"
{"type":"assistant","message":{"id":"m1","model":"x","usage":{"input_tokens":null,"output_tokens":"lots"}}}
"#;
        let (events, _) = read(text);
        let Event::TokenUsage(usage) = &events[0].event else {
            panic!("expected token usage")
        };
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }
}
