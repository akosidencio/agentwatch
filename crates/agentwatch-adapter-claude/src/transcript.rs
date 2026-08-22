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
//! `toolUseResult` is not deserialized at all, so command output and file
//! contents never enter memory.
//!
//! `message.content` **is** read, but only through a type that declares three
//! fields: a block's `type`, a tool call's `id`, and its `name` and narrowed
//! `input`. The assistant's prose and its thinking live in a `text` field that
//! no type here names, so serde drops them while parsing rather than a later
//! filter removing them. The narrowing of `input` is [`ToolInput`]'s job, and
//! it omits `content`, `old_string`, and `new_string` for the same reason.
//!
//! This was a deliberate narrowing of an earlier, simpler rule — `content` used
//! not to be deserialized at all — and it was made because the alternative was
//! discarding the entire activity timeline of every session the hooks did not
//! witness. The guarantee is now enforced by the shape of the types instead of
//! by their absence, which is weaker to state and just as testable.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

use agentwatch_events::{
    AgentEvent, Event, EvidenceSource, PermissionModeEvent, QueueEvent, TokenUsageEvent, ToolOutcomeEvent,
};

use crate::tools::{self, ToolInput};
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
    /// Distinct tool calls recovered from the response content.
    pub tool_calls: u64,
    /// Queue movements seen.
    pub queue_operations: u64,
    /// Times the permission posture changed.
    pub permission_changes: u64,
    /// Tool calls that were matched to the result that answered them.
    ///
    /// Lower than [`Self::tool_calls`] when a transcript ends mid-flight: the
    /// last call has been made and not yet answered.
    pub tool_outcomes: u64,
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
    let (events, summary) = read_transcript_from(reader)?;
    Ok((
        events
            .into_iter()
            .filter(|event| matches!(event.event, Event::TokenUsage(_)))
            .collect(),
        summary,
    ))
}

/// Reads every event a transcript can account for: token usage and activity.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read.
pub fn read_transcript(
    path: impl AsRef<Path>,
) -> Result<(Vec<AgentEvent>, TranscriptSummary), TranscriptError> {
    let file = std::fs::File::open(path.as_ref())?;
    read_transcript_from(BufReader::new(file))
}

/// Reads token usage *and* activity from any source of transcript lines.
///
/// # Why the transcript is read for activity at all
///
/// Hooks are the live path, but they only exist while they are installed and
/// only from the moment they were. Everything before that — imported history,
/// a session that predates the daemon, a machine where the hooks were removed —
/// left a complete record of its tool calls in the transcript and nothing was
/// reading it. The result was sessions with perfect token accounting and an
/// empty timeline, which reported as "nothing observed" rather than "never
/// looked at".
///
/// # Why re-reading is safe
///
/// Both kinds of event get a deterministic id: a response keys off its message
/// id, and a tool call keys off the `tool_use` id the transcript assigns it.
/// Reading the same file twice therefore produces the same rows, and storage's
/// insert is idempotent on the primary key. That is also what lets the live
/// hook and this reader describe the same tool call without duplicating it —
/// provided the hook payload carries the same identifier.
///
/// # Errors
///
/// Returns an error if the underlying reader fails.
pub fn read_transcript_from<R: BufRead>(
    reader: R,
) -> Result<(Vec<AgentEvent>, TranscriptSummary), TranscriptError> {
    let mut summary = TranscriptSummary::default();
    let mut events = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_tools: HashSet<String> = HashSet::new();
    // Calls waiting for their result: the tool that ran and when it started.
    // A result arrives in a later record — for a long build, minutes and many
    // records later — so the pairing cannot be done within one line.
    let mut pending: std::collections::HashMap<String, (String, Timestamp)> =
        std::collections::HashMap::new();
    let mut seen_outcomes: HashSet<String> = HashSet::new();
    // The posture in force. Only transitions are recorded: it holds until
    // something says otherwise, so a row per turn would be noise.
    let mut posture: Option<String> = None;

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

        // The queue moves while the agent is busy, so these records are the
        // only measure of time the person spent waiting on it.
        if record.kind.as_deref() == Some("queue-operation") {
            if let Some(operation) = record.operation.as_deref() {
                summary.queue_operations += 1;
                events.push(queue_event(&record, operation));
            }
            continue;
        }

        // A `user` record carries the results of the calls the assistant made.
        // Only `tool_use_id` and `is_error` are declared on the way in; the
        // result's own `content` is named nowhere, so tool output cannot reach
        // a value here any more than it can through `toolUseResult`.
        if record.kind.as_deref() == Some("user") {
            if let Some(mode) = record.permission_mode.as_deref()
                && posture.as_deref() != Some(mode)
            {
                let previous = posture.replace(mode.to_owned());
                summary.permission_changes += 1;
                events.push(posture_event(&record, mode, previous));
            }

            let Some(message) = record.message.as_ref() else {
                continue;
            };
            let finished = record
                .timestamp
                .as_deref()
                .and_then(|at| Timestamp::parse_rfc3339(at).ok());

            for block in &message.content {
                if block.kind.as_deref() != Some("tool_result") {
                    continue;
                }
                let Some(tool_use_id) = block.tool_use_id.as_deref() else {
                    continue;
                };
                let Some((tool, started)) = pending.remove(tool_use_id) else {
                    continue;
                };
                if !seen_outcomes.insert(tool_use_id.to_owned()) {
                    continue;
                }
                summary.tool_outcomes += 1;

                events.push(outcome_event(
                    &record,
                    tool,
                    tool_use_id,
                    duration_ms(started, finished),
                    block.is_error,
                ));
            }
            continue;
        }

        if record.kind.as_deref() != Some("assistant") {
            continue;
        }
        let Some(message) = record.message.take() else {
            continue;
        };

        // Activity first, and independently of usage: a response's blocks are
        // repeated across its records, so they are deduplicated on their own
        // key rather than riding on the usage dedup. A record that carried no
        // usage at all would otherwise lose its tool calls.
        for block in &message.content {
            if block.kind.as_deref() != Some("tool_use") {
                continue;
            }
            let Some(tool_use_id) = block.id.as_deref() else {
                continue;
            };
            if !seen_tools.insert(tool_use_id.to_owned()) {
                continue;
            }
            summary.tool_calls += 1;

            let event = tools::tool_event(block.name.as_deref(), block.input.as_ref());
            let started = record
                .timestamp
                .as_deref()
                .and_then(|at| Timestamp::parse_rfc3339(at).ok());
            if let (Some(tool), Some(started)) = (block.name.clone(), started) {
                pending.insert(tool_use_id.to_owned(), (tool, started));
            }
            events.push(activity_event(&record, event, tool_use_id));
        }

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

        events.push(to_event(
            &record,
            message.model,
            key,
            usage,
            message.diagnostics.as_ref(),
        ));
    }

    Ok((events, summary))
}

/// Builds one activity event, keyed so re-reading cannot duplicate it.
fn activity_event(record: &TranscriptRecord, event: Event, tool_use_id: &str) -> AgentEvent {
    let mut built = AgentEvent::observed(AgentId::CLAUDE_CODE, EvidenceSource::Transcript, event)
        .with_id(EventId::from_key(&AgentId::CLAUDE_CODE, tool_use_id));

    if let Some(timestamp) = record
        .timestamp
        .as_deref()
        .and_then(|t| Timestamp::parse_rfc3339(t).ok())
    {
        built = built.at(timestamp);
    }
    if let Some(session) = record.session_id.clone() {
        built = built.with_session(ExternalSessionId::from(session));
    }
    if let Some(cwd) = record.cwd.clone() {
        built = built.with_project_path(cwd);
    }

    built
        .with_git_branch(record.git_branch.clone())
        .with_surface(record.entrypoint.clone())
}

/// Builds one normalized event from a deduplicated response.
/// Key the cache-miss cause is stored under in the usage remainder.
///
/// It rides in `provider_usage` rather than a column of its own because that
/// map is exactly the designed place for a provider-reported detail this
/// version has no schema for — "preserved rather than silently dropped, and
/// promotable to a real column later without re-reading history". Promotion is
/// cheap now that the remainder is read back; dropping it was the only
/// unrecoverable option.
const CACHE_MISS_KEY: &str = "cache_miss_reason";

fn to_event(
    record: &TranscriptRecord,
    model: Option<String>,
    key: String,
    usage: serde_json::Map<String, serde_json::Value>,
    diagnostics: Option<&Diagnostics>,
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

    // A cache miss means paying full input price on a prompt that was expected
    // to be nearly free, and nothing else in the transcript explains a cost
    // spike as directly. Only the cause is kept — a short enumerated string.
    if let Some(reason) = diagnostics
        .and_then(|diagnostics| diagnostics.cache_miss_reason.as_ref())
        .and_then(|reason| reason.kind.as_deref())
    {
        token_usage.provider_usage.insert(
            CACHE_MISS_KEY.to_owned(),
            serde_json::Value::String(reason.to_owned()),
        );
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

/// Builds the event recording one queue movement.
fn queue_event(record: &TranscriptRecord, operation: &str) -> AgentEvent {
    // The same operation recurs constantly within a session, so the key has to
    // carry the moment as well or a re-read would collapse them all into one.
    let key = format!(
        "queue:{}:{operation}:{}",
        record.session_id.as_deref().unwrap_or_default(),
        record.timestamp.as_deref().unwrap_or_default()
    );

    let mut built = AgentEvent::observed(
        AgentId::CLAUDE_CODE,
        EvidenceSource::Transcript,
        Event::Queue(QueueEvent {
            operation: operation.to_owned(),
        }),
    )
    .with_id(EventId::from_key(&AgentId::CLAUDE_CODE, &key));

    if let Some(timestamp) = record
        .timestamp
        .as_deref()
        .and_then(|at| Timestamp::parse_rfc3339(at).ok())
    {
        built = built.at(timestamp);
    }
    if let Some(session) = record.session_id.clone() {
        built = built.with_session(ExternalSessionId::from(session));
    }
    if let Some(cwd) = record.cwd.clone() {
        built = built.with_project_path(cwd);
    }

    built
        .with_git_branch(record.git_branch.clone())
        .with_surface(record.entrypoint.clone())
}

/// Builds the event marking a change of permission posture.
fn posture_event(record: &TranscriptRecord, mode: &str, previous: Option<String>) -> AgentEvent {
    // Keyed on the session, the posture and the moment: re-reading the same
    // transcript must not append the same transition twice, and the same
    // posture legitimately recurs later in a session.
    let key = format!(
        "{}:{mode}:{}",
        record.session_id.as_deref().unwrap_or_default(),
        record.timestamp.as_deref().unwrap_or_default()
    );

    let mut built = AgentEvent::observed(
        AgentId::CLAUDE_CODE,
        EvidenceSource::Transcript,
        Event::PermissionMode(PermissionModeEvent {
            mode: mode.to_owned(),
            previous,
        }),
    )
    .with_id(EventId::from_key(&AgentId::CLAUDE_CODE, &key));

    if let Some(timestamp) = record
        .timestamp
        .as_deref()
        .and_then(|at| Timestamp::parse_rfc3339(at).ok())
    {
        built = built.at(timestamp);
    }
    if let Some(session) = record.session_id.clone() {
        built = built.with_session(ExternalSessionId::from(session));
    }
    if let Some(cwd) = record.cwd.clone() {
        built = built.with_project_path(cwd);
    }

    built
        .with_git_branch(record.git_branch.clone())
        .with_surface(record.entrypoint.clone())
}

/// Milliseconds between a call and its result, when both are known.
///
/// `None` rather than zero for a missing endpoint: a duration nobody measured
/// is not an instant one, and averaging it as such would flatter every tool.
/// A negative span is discarded for the same reason — clocks in a transcript
/// come from whatever wrote it.
fn duration_ms(started: Timestamp, finished: Option<Timestamp>) -> Option<u64> {
    let finished = finished?;
    let micros = finished.as_micros().checked_sub(started.as_micros())?;
    u64::try_from(micros).ok().map(|micros| micros / 1_000)
}

/// Builds the event describing how one call turned out.
fn outcome_event(
    record: &TranscriptRecord,
    tool: String,
    tool_use_id: &str,
    duration_ms: Option<u64>,
    failed: bool,
) -> AgentEvent {
    // Keyed off the call's identifier with a suffix, so it is deterministic
    // like everything else here and cannot collide with the call itself.
    let key = format!("{tool_use_id}:outcome");
    let mut built = AgentEvent::observed(
        AgentId::CLAUDE_CODE,
        EvidenceSource::Transcript,
        Event::ToolOutcome(ToolOutcomeEvent {
            tool,
            tool_use_id: tool_use_id.to_owned(),
            duration_ms,
            failed,
        }),
    )
    .with_id(EventId::from_key(&AgentId::CLAUDE_CODE, &key));

    if let Some(timestamp) = record
        .timestamp
        .as_deref()
        .and_then(|at| Timestamp::parse_rfc3339(at).ok())
    {
        built = built.at(timestamp);
    }
    if let Some(session) = record.session_id.clone() {
        built = built.with_session(ExternalSessionId::from(session));
    }
    if let Some(cwd) = record.cwd.clone() {
        built = built.with_project_path(cwd);
    }

    built
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
    /// On a `queue-operation` record, what happened to the queue.
    operation: Option<String>,
    /// The permission posture in force, when the record says.
    ///
    /// Carried on `user` records. Not to be confused with the `mode` record
    /// type, which is a different vocabulary describing the input mode and has
    /// only ever held one value.
    permission_mode: Option<String>,
    /// The model message.
    message: Option<TranscriptMessage>,
}

/// The subset of a model message worth reading.
///
/// `content` is read, but only through [`ContentBlock`], which names the three
/// fields a tool call is identified by and nothing else. The assistant's prose
/// and its thinking live in a `text` field that no type here declares, so they
/// are dropped by the parser rather than by a later filter.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TranscriptMessage {
    /// The provider's identifier for this response, and our dedup key.
    id: Option<String>,
    /// The exact model identifier.
    model: Option<String>,
    /// Reported usage, kept as a map so unknown categories survive.
    usage: Option<serde_json::Map<String, serde_json::Value>>,
    /// What the provider noticed while serving the response.
    ///
    /// A sibling of `usage`, not part of it, which is why the remainder map
    /// never picked it up: it was never offered to it.
    diagnostics: Option<Diagnostics>,
    /// The response's content blocks, of which only `tool_use` is of interest.
    content: Vec<ContentBlock>,
}

/// Provider-side notes about how a response was served.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Diagnostics {
    /// Why the prompt cache was not used, when it was not.
    cache_miss_reason: Option<CacheMissReason>,
}

/// The reason the prompt cache missed.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CacheMissReason {
    /// A short machine-readable cause, e.g. `previous_message_not_found`.
    #[serde(rename = "type")]
    kind: Option<String>,
}

/// One block of a model response.
///
/// Only `tool_use` blocks are acted on. `text` and `thinking` blocks parse into
/// an all-`None` value and are skipped, which is the point: their payload field
/// is not declared here, so conversation text cannot reach a Rust value.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ContentBlock {
    /// `tool_use`, `text`, `thinking`, and others.
    #[serde(rename = "type")]
    kind: Option<String>,
    /// The provider's identifier for this tool call, e.g. `toolu_01H6Knd…`.
    ///
    /// The dedup key for activity, and the reason a transcript can be re-read
    /// without duplicating the timeline.
    id: Option<String>,
    /// The tool invoked.
    name: Option<String>,
    /// Its arguments, narrowed to the ones worth keeping.
    input: Option<ToolInput>,
    /// On a `tool_result` block, the call it answers.
    tool_use_id: Option<String>,
    /// On a `tool_result` block, whether the tool reported failure.
    ///
    /// The result's own `content` is deliberately not declared beside it: the
    /// flag is the telemetry, the output is not.
    is_error: bool,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use agentwatch_events::{CommandEvent, FileEvent};

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

    /// A response carrying two tool calls, in the real shape.
    const RESPONSE_WITH_TOOL_CALLS: &str = r#"
{"type":"assistant","timestamp":"2026-08-20T17:22:02.051Z","sessionId":"s-1","cwd":"/work","gitBranch":"main","entrypoint":"claude-vscode","message":{"id":"msg_t","model":"claude-opus-5","content":[{"type":"thinking","text":"the launch code is hunter2"},{"type":"tool_use","id":"toolu_a","name":"Bash","input":{"command":"cargo test","description":"run tests"}},{"type":"tool_use","id":"toolu_b","name":"Edit","input":{"file_path":"/src/auth.rs","old_string":"AKIAIOSFODNN7EXAMPLE","new_string":"redacted-please"}}],"usage":{"input_tokens":1,"output_tokens":2}}}
"#;

    fn read_all(text: &str) -> (Vec<AgentEvent>, TranscriptSummary) {
        read_transcript_from(Cursor::new(text)).expect("reads")
    }

    #[test]
    fn recovers_the_activity_timeline_from_tool_use_blocks() {
        // The gap this closes: a transcript holds every tool call a session
        // made, and the reader used to walk past all of them for the usage
        // figures alone.
        let (events, summary) = read_all(RESPONSE_WITH_TOOL_CALLS);
        assert_eq!(summary.tool_calls, 2);

        let kinds: Vec<&str> = events.iter().map(AgentEvent::kind).collect();
        assert_eq!(kinds, vec!["command", "file.write", "token.usage"]);

        assert_eq!(
            events[0].event,
            Event::Command(CommandEvent {
                command: "cargo test".into(),
                description: Some("run tests".into()),
            })
        );
        assert_eq!(
            events[1].event,
            Event::FileWrite(FileEvent {
                path: "/src/auth.rs".into(),
                tool: "Edit".into(),
            })
        );

        // Activity inherits the session context the usage events already carry.
        assert_eq!(events[0].project_path.as_deref(), Some("/work"));
        assert_eq!(events[0].git_branch.as_deref(), Some("main"));
        assert_eq!(events[0].surface.as_deref(), Some("claude-vscode"));
        assert!(events[0].session_id.is_some());
    }

    #[test]
    fn reading_a_transcript_twice_produces_identical_activity() {
        // The whole idempotency story rests on this: reconcile runs repeatedly
        // over a growing file, and a non-deterministic id would append the same
        // tool call to the timeline on every pass.
        let (first, _) = read_all(RESPONSE_WITH_TOOL_CALLS);
        let (second, _) = read_all(RESPONSE_WITH_TOOL_CALLS);
        assert_eq!(first, second);

        let ids: Vec<_> = first.iter().map(|event| event.id).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len(), "ids must not collide");
    }

    #[test]
    fn a_tool_call_is_recovered_even_when_its_record_reports_no_usage() {
        // Activity is deduplicated on its own key rather than riding on the
        // usage dedup, so a record with no usage still yields its tool calls.
        let text = r#"
{"type":"assistant","timestamp":"2026-08-20T17:22:02.051Z","sessionId":"s-1","message":{"id":"msg_n","content":[{"type":"tool_use","id":"toolu_lonely","name":"Read","input":{"file_path":"/etc/hosts"}}]}}
"#;
        let (events, summary) = read_all(text);
        assert_eq!(summary.tool_calls, 1);
        assert_eq!(summary.responses, 0, "no usage, so no response counted");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind(), "file.read");
    }

    #[test]
    fn one_tool_call_is_counted_once_though_its_response_spans_records() {
        // The same rule that governs usage: a response's blocks repeat across
        // the records it is split over.
        let doubled = format!("{RESPONSE_WITH_TOOL_CALLS}{RESPONSE_WITH_TOOL_CALLS}");
        let (events, summary) = read_all(&doubled);
        assert_eq!(summary.tool_calls, 2, "not four");
        assert_eq!(events.iter().filter(|e| e.kind() == "command").count(), 1);
    }

    #[test]
    fn reading_activity_still_keeps_prose_and_patch_bodies_out() {
        // The privacy rule, now that `content` is parsed rather than skipped.
        // `thinking` text and an `Edit`'s replacement string are both present in
        // the input and must not survive the parse.
        let (events, _) = read_all(RESPONSE_WITH_TOOL_CALLS);
        let encoded = serde_json::to_string(&events).expect("serializable");

        assert!(!encoded.contains("hunter2"), "thinking text leaked");
        assert!(!encoded.contains("AKIA"), "old_string leaked");
        assert!(!encoded.contains("redacted-please"), "new_string leaked");
    }

    #[test]
    fn a_cache_miss_reason_is_kept_so_a_cost_spike_can_be_explained() {
        // `diagnostics` sits beside `usage`, not inside it, which is exactly why
        // the remainder map never picked it up on its own.
        let text = r#"
{"type":"assistant","timestamp":"2026-08-20T17:22:02.051Z","sessionId":"s-1","message":{"id":"msg_c","model":"m","diagnostics":{"cache_miss_reason":{"type":"previous_message_not_found"}},"usage":{"input_tokens":9,"output_tokens":1}}}
"#;
        let (events, _) = read(text);
        let Event::TokenUsage(usage) = &events[0].event else {
            panic!("expected token usage")
        };
        assert_eq!(
            usage
                .provider_usage
                .get("cache_miss_reason")
                .and_then(serde_json::Value::as_str),
            Some("previous_message_not_found")
        );
    }

    #[test]
    fn a_response_that_hit_the_cache_records_no_reason() {
        let (events, _) = read(ONE_RESPONSE_TWO_RECORDS);
        let Event::TokenUsage(usage) = &events[0].event else {
            panic!("expected token usage")
        };
        assert!(!usage.provider_usage.contains_key("cache_miss_reason"));
    }

    /// A call and the result that answers it, four seconds later.
    const CALL_AND_RESULT: &str = r#"
{"type":"assistant","timestamp":"2026-08-20T17:22:02.000Z","sessionId":"s-1","cwd":"/work","message":{"id":"msg_o","content":[{"type":"tool_use","id":"toolu_slow","name":"Bash","input":{"command":"cargo build"}}],"usage":{"output_tokens":1}}}
{"type":"user","timestamp":"2026-08-20T17:22:06.000Z","sessionId":"s-1","cwd":"/work","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_slow","is_error":true,"content":"error: could not compile\nAKIAIOSFODNN7EXAMPLE"}]}}
"#;

    #[test]
    fn a_tool_call_is_paired_with_its_result_for_duration_and_failure() {
        let (events, summary) = read_all(CALL_AND_RESULT);
        assert_eq!(summary.tool_outcomes, 1);

        let outcome = events
            .iter()
            .find_map(|event| match &event.event {
                Event::ToolOutcome(outcome) => Some(outcome),
                _ => None,
            })
            .expect("an outcome");

        assert_eq!(outcome.tool, "Bash");
        assert_eq!(outcome.tool_use_id, "toolu_slow");
        assert_eq!(outcome.duration_ms, Some(4_000));
        assert!(outcome.failed);
    }

    #[test]
    fn pairing_a_result_never_carries_its_output_through() {
        // `is_error` is the telemetry; the text beside it is not, and the type
        // that reads the block does not name it.
        let (events, _) = read_all(CALL_AND_RESULT);
        let encoded = serde_json::to_string(&events).expect("serializable");
        assert!(!encoded.contains("AKIA"), "{encoded}");
        assert!(!encoded.contains("could not compile"), "{encoded}");
    }

    #[test]
    fn a_call_still_awaiting_its_result_yields_no_outcome() {
        // What a transcript looks like while the tool is still running. The
        // call is recorded; inventing an outcome for it would be a lie.
        let (events, summary) = read_all(RESPONSE_WITH_TOOL_CALLS);
        assert_eq!(summary.tool_calls, 2);
        assert_eq!(summary.tool_outcomes, 0);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.event, Event::ToolOutcome(_)))
        );
    }

    #[test]
    fn outcomes_are_deterministic_and_do_not_collide_with_their_call() {
        let (first, _) = read_all(CALL_AND_RESULT);
        let (second, _) = read_all(CALL_AND_RESULT);
        assert_eq!(first, second, "re-reading must not produce new rows");

        let ids: std::collections::HashSet<_> = first.iter().map(|event| event.id).collect();
        assert_eq!(ids.len(), first.len(), "the outcome must not shadow the call");
    }

    #[test]
    fn a_result_with_no_matching_call_is_ignored() {
        // Resuming a session mid-flight: the transcript can open on a result
        // whose call was written to the previous file.
        let text = r#"
{"type":"user","timestamp":"2026-08-20T17:22:06.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_orphan","is_error":false}]}}
"#;
        let (events, summary) = read_all(text);
        assert_eq!(summary.tool_outcomes, 0);
        assert!(events.is_empty());
    }

    #[test]
    fn queue_movements_are_recorded_without_the_message() {
        let text = r#"
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-08-22T02:25:27.941Z","sessionId":"s-1"}
{"type":"queue-operation","operation":"dequeue","timestamp":"2026-08-22T02:25:38.000Z","sessionId":"s-1"}
"#;
        let (events, summary) = read_all(text);
        assert_eq!(summary.queue_operations, 2);

        let operations: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.event {
                Event::Queue(queue) => Some(queue.operation.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(operations, vec!["enqueue", "dequeue"]);

        // The gap between the two is the point: ten seconds the person spent
        // waiting on the agent.
        assert_eq!(
            events[1].timestamp.as_micros() - events[0].timestamp.as_micros(),
            10_059_000
        );
    }

    #[test]
    fn the_same_queue_operation_twice_is_two_events() {
        // `enqueue` recurs constantly; keying on the operation alone would
        // collapse a whole session's worth into one row.
        let text = r#"
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-08-22T02:25:27.000Z","sessionId":"s-1"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-08-22T02:25:29.000Z","sessionId":"s-1"}
"#;
        let (events, _) = read_all(text);
        let ids: std::collections::HashSet<_> = events.iter().map(|event| event.id).collect();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn only_changes_of_permission_posture_are_recorded() {
        // Four turns, two postures. Writing a row per turn would bury the two
        // moments that actually matter in noise.
        let text = r#"
{"type":"user","timestamp":"2026-08-20T17:22:01.000Z","sessionId":"s-1","permissionMode":"default","message":{"content":[]}}
{"type":"user","timestamp":"2026-08-20T17:22:02.000Z","sessionId":"s-1","permissionMode":"default","message":{"content":[]}}
{"type":"user","timestamp":"2026-08-20T17:22:03.000Z","sessionId":"s-1","permissionMode":"acceptEdits","message":{"content":[]}}
{"type":"user","timestamp":"2026-08-20T17:22:04.000Z","sessionId":"s-1","permissionMode":"acceptEdits","message":{"content":[]}}
"#;
        let (events, summary) = read_all(text);
        assert_eq!(summary.permission_changes, 2);

        let postures: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.event {
                Event::PermissionMode(mode) => Some((mode.mode.clone(), mode.previous.clone())),
                _ => None,
            })
            .collect();

        assert_eq!(
            postures,
            vec![
                ("default".to_owned(), None),
                ("acceptEdits".to_owned(), Some("default".to_owned())),
            ],
            "the first sighting is a starting state, not a change"
        );
    }

    #[test]
    fn a_posture_that_recurs_later_is_recorded_again() {
        // Going back to `default` after a spell of `acceptEdits` is a real
        // transition, so the id cannot be keyed on the posture alone.
        let text = r#"
{"type":"user","timestamp":"2026-08-20T17:22:01.000Z","sessionId":"s-1","permissionMode":"default","message":{"content":[]}}
{"type":"user","timestamp":"2026-08-20T17:22:02.000Z","sessionId":"s-1","permissionMode":"acceptEdits","message":{"content":[]}}
{"type":"user","timestamp":"2026-08-20T17:22:03.000Z","sessionId":"s-1","permissionMode":"default","message":{"content":[]}}
"#;
        let (events, summary) = read_all(text);
        assert_eq!(summary.permission_changes, 3);

        let ids: std::collections::HashSet<_> = events.iter().map(|event| event.id).collect();
        assert_eq!(ids.len(), 3, "each transition is its own row");
    }

    #[test]
    fn the_token_usage_reader_still_returns_only_usage() {
        // `verify` computes drift from this function and must not start seeing
        // activity events, or every transcript would look like it had drifted.
        let (events, _) = read(RESPONSE_WITH_TOOL_CALLS);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].event, Event::TokenUsage(_)));
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
