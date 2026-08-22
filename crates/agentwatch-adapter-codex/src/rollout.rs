//! Parsing Codex's append-only rollout JSONL.

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use agentwatch_events::{
    AgentEvent, CommandEvent, Event, EvidenceSource, FileEvent, SessionEnded, SessionStarted,
    TokenUsageEvent, ToolCallEvent, ToolOutcomeEvent,
};
use agentwatch_types::{AgentId, EventId, ExternalSessionId, Timestamp};
use serde::Deserialize;

const PROVIDER: &str = "openai";
const MAX_SEARCH_DEPTH: usize = 12;

/// A rollout that could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RolloutError {
    /// The file could not be opened or read.
    #[error("could not read the Codex rollout")]
    Io(#[from] std::io::Error),
}

/// What one rollout pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RolloutSummary {
    /// JSONL records examined.
    pub lines: u64,
    /// Invalid or partially-written records skipped.
    pub unparseable_lines: u64,
    /// Distinct non-zero model responses.
    pub responses: u64,
    /// Shell commands recovered from Codex exec calls.
    pub commands: u64,
    /// Changed file paths recovered without reading patch bodies.
    pub file_writes: u64,
    /// Tool calls paired with the output that answered them.
    pub tool_outcomes: u64,
}

/// Returns `$CODEX_HOME/sessions`, or `~/.codex/sessions` by default.
///
/// # Errors
///
/// Returns an error when neither `CODEX_HOME` nor `HOME` is available.
pub fn rollout_root() -> Result<PathBuf, RolloutError> {
    if let Some(root) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(root).join("sessions"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| std::io::Error::other("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".codex").join("sessions"))
}

/// Finds every rollout below `root`, in stable path order.
#[must_use]
pub fn find_rollouts(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_rollouts(root, MAX_SEARCH_DEPTH, &mut found);
    found.sort();
    found
}

fn collect_rollouts(directory: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if let Some(remaining) = depth.checked_sub(1) {
                collect_rollouts(&path, remaining, found);
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

/// Reads normalized metadata events from a rollout file.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read. A malformed final
/// line is counted and skipped because active rollouts are appended in place.
pub fn read_rollout(
    path: impl AsRef<Path>,
) -> Result<(Vec<AgentEvent>, RolloutSummary), RolloutError> {
    let path = path.as_ref();
    let file = std::fs::File::open(path)?;
    read_rollout_from(BufReader::new(file), Some(path))
}

/// Reads a rollout from any buffered source.
///
/// The optional path is stored only as session provenance.
///
/// # Errors
///
/// Returns an error if the underlying reader fails.
pub fn read_rollout_from<R: BufRead>(
    reader: R,
    transcript_path: Option<&Path>,
) -> Result<(Vec<AgentEvent>, RolloutSummary), RolloutError> {
    let mut state = State::default();
    let mut events = Vec::new();
    let mut summary = RolloutSummary::default();

    for (line_index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        summary.lines += 1;
        let Ok(record) = serde_json::from_str::<Record>(&line) else {
            summary.unparseable_lines += 1;
            continue;
        };
        let timestamp =
            Timestamp::parse_rfc3339(&record.timestamp).unwrap_or_else(|_| Timestamp::now());
        state.consume(
            record.payload,
            timestamp,
            line_index,
            transcript_path,
            &mut events,
            &mut summary,
        );
    }

    Ok((events, summary))
}

#[derive(Debug, Default)]
struct State {
    session: Option<ExternalSessionId>,
    session_id_text: Option<String>,
    parent_session: Option<ExternalSessionId>,
    project: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    surface: Option<String>,
    is_subagent: bool,
    seen_usage: HashSet<String>,
    /// Calls awaiting the output that answers them: the tool and when it began.
    pending_tools: std::collections::HashMap<String, (String, Timestamp)>,
}

impl State {
    #[allow(clippy::too_many_arguments)]
    fn consume(
        &mut self,
        payload: Payload,
        timestamp: Timestamp,
        line_index: usize,
        transcript_path: Option<&Path>,
        events: &mut Vec<AgentEvent>,
        summary: &mut RolloutSummary,
    ) {
        match payload {
            Payload::SessionMeta(meta) => {
                self.is_subagent = meta.source.as_ref().is_some_and(Source::is_subagent);
                self.parent_session = meta
                    .session_id
                    .filter(|session_id| session_id != &meta.id)
                    .map(ExternalSessionId::from);
                self.session_id_text = Some(meta.id.clone());
                self.session = Some(ExternalSessionId::from(meta.id));
                self.project = Some(meta.cwd);
                self.surface = meta.originator.or(meta.source.and_then(Source::surface));
                let event = self.event(
                    Event::SessionStarted(SessionStarted {
                        trigger: Some("startup".to_owned()),
                        transcript_path: transcript_path.map(|path| path.display().to_string()),
                    }),
                    timestamp,
                    &format!("line:{line_index}:session"),
                );
                events.push(event);
            }
            Payload::TurnContext(context) => {
                self.model = context.model;
                self.effort = context.effort;
                if let Some(cwd) = context.cwd {
                    self.project = Some(cwd);
                }
            }
            Payload::EventMsg(message) => match message {
                EventMessage::TaskStarted => events.push(self.event(
                    Event::SessionStarted(SessionStarted {
                        trigger: Some("turn".to_owned()),
                        transcript_path: transcript_path.map(|path| path.display().to_string()),
                    }),
                    timestamp,
                    &format!("line:{line_index}:turn-start"),
                )),
                EventMessage::TaskComplete => events.push(self.event(
                    Event::SessionEnded(SessionEnded {
                        reason: Some("turn_complete".to_owned()),
                    }),
                    timestamp,
                    &format!("line:{line_index}:turn-end"),
                )),
                EventMessage::TurnAborted { reason } => events.push(self.event(
                    Event::SessionEnded(SessionEnded {
                        reason: reason.or_else(|| Some("turn_aborted".to_owned())),
                    }),
                    timestamp,
                    &format!("line:{line_index}:turn-aborted"),
                )),
                EventMessage::TokenCount { info } => {
                    let Some(info) = info else {
                        return;
                    };
                    let snapshot = info
                        .total_token_usage
                        .as_ref()
                        .or(info.last_token_usage.as_ref())
                        .map(CodexUsage::snapshot_key);
                    let Some(usage) = info.last_token_usage else {
                        return;
                    };
                    if usage.total_tokens == 0 {
                        return;
                    }
                    let snapshot = snapshot.unwrap_or_else(|| format!("line:{line_index}"));
                    if !self.seen_usage.insert(snapshot.clone()) {
                        return;
                    }
                    let request_id = format!(
                        "{}:{snapshot}",
                        self.session_id_text.as_deref().unwrap_or("unknown")
                    );
                    let input_tokens = usage.input_tokens.saturating_sub(usage.cached_input_tokens);
                    let mut provider_usage = serde_json::Map::new();
                    provider_usage.insert(
                        "reasoning_output_tokens".to_owned(),
                        serde_json::Value::from(usage.reasoning_output_tokens),
                    );
                    if let Some(effort) = &self.effort {
                        provider_usage.insert("reasoning_effort".to_owned(), effort.clone().into());
                    }
                    let event = self.event(
                        Event::TokenUsage(TokenUsageEvent {
                            provider: PROVIDER.to_owned(),
                            model: self.model.clone(),
                            request_id: Some(request_id.clone()),
                            input_tokens,
                            cache_creation_input_tokens: usage.cache_write_input_tokens,
                            cache_read_input_tokens: usage.cached_input_tokens,
                            output_tokens: usage.output_tokens,
                            is_subagent: self.is_subagent,
                            provider_usage,
                        }),
                        timestamp,
                        &format!("usage:{request_id}"),
                    );
                    events.push(event);
                    summary.responses += 1;
                }
                EventMessage::PatchApplyEnd { success, changes } => {
                    if !success {
                        return;
                    }
                    for (change_index, (path, change)) in changes.into_iter().enumerate() {
                        let project = repository_root(&path)
                            .or_else(|| self.project.clone())
                            .or_else(|| parent_project(&path));
                        if let Some(project) = project {
                            self.project = Some(project);
                        }
                        let tool = change.kind.map_or_else(
                            || "apply_patch".to_owned(),
                            |kind| format!("apply_patch:{kind}"),
                        );
                        events.push(self.event(
                            Event::FileWrite(FileEvent { path, tool }),
                            timestamp,
                            &format!("line:{line_index}:file:{change_index}"),
                        ));
                        summary.file_writes += 1;
                    }
                }
                EventMessage::Other => {}
            },
            Payload::ResponseItem(item) => match item {
                ResponseItem::CustomToolCall {
                    name,
                    input,
                    call_id,
                } if name == "exec" => {
                    // Remembered so the output that answers it can be paired
                    // back into a duration and a pass or fail.
                    if let Some(call_id) = call_id {
                        self.pending_tools
                            .insert(call_id, (name.clone(), timestamp));
                    }
                    for (call_index, call) in exec_calls(&input).into_iter().enumerate() {
                        if let Some(workdir) = call.workdir {
                            self.project = Some(workdir);
                        }
                        events.push(self.event(
                            Event::Command(CommandEvent {
                                command: call.cmd,
                                description: None,
                            }),
                            timestamp,
                            &format!("line:{line_index}:command:{call_index}"),
                        ));
                        summary.commands += 1;
                    }
                }
                ResponseItem::CustomToolCall { name, call_id, .. }
                | ResponseItem::FunctionCall { name, call_id } => {
                    if let Some(call_id) = call_id {
                        self.pending_tools
                            .insert(call_id, (name.clone(), timestamp));
                    }
                    events.push(self.event(
                        Event::ToolCall(ToolCallEvent { tool: name }),
                        timestamp,
                        &format!("line:{line_index}:tool"),
                    ))
                }
                ResponseItem::CustomToolCallOutput { call_id, output }
                | ResponseItem::FunctionCallOutput { call_id, output } => {
                    let Some(call_id) = call_id else { return };
                    let Some((tool, started)) = self.pending_tools.remove(&call_id) else {
                        return;
                    };

                    // Only the first chunk is consulted. It is the runner's own
                    // status line; everything after it is the command's output.
                    let failed = output.first().is_some_and(OutputChunk::reports_failure);
                    let duration_ms = u64::try_from(
                        timestamp
                            .as_micros()
                            .saturating_sub(started.as_micros())
                            .max(0),
                    )
                    .ok()
                    .map(|micros| micros / 1_000);

                    events.push(self.event(
                        Event::ToolOutcome(ToolOutcomeEvent {
                            tool,
                            tool_use_id: call_id.clone(),
                            duration_ms,
                            failed,
                        }),
                        timestamp,
                        &format!("outcome:{call_id}"),
                    ));
                    summary.tool_outcomes += 1;
                }
                ResponseItem::Other => {}
            },
            Payload::Other => {}
        }
    }

    fn event(&self, event: Event, timestamp: Timestamp, key: &str) -> AgentEvent {
        let key = format!(
            "{}:{key}",
            self.session_id_text.as_deref().unwrap_or("unknown")
        );
        let mut normalized =
            AgentEvent::observed(AgentId::CODEX, EvidenceSource::Transcript, event)
                .with_id(EventId::from_key(&AgentId::CODEX, &key))
                .at(timestamp)
                .with_surface(self.surface.clone())
                .with_parent_session(self.parent_session.clone());
        if let Some(session) = self.session.clone() {
            normalized = normalized.with_session(session);
        }
        if let Some(project) = self.project.clone() {
            normalized = normalized.with_project_path(project);
        }
        normalized
    }
}

fn parent_project(path: &str) -> Option<String> {
    Path::new(path)
        .parent()
        .map(|path| path.display().to_string())
}

fn repository_root(path: &str) -> Option<String> {
    let mut directory = Path::new(path).parent()?;
    loop {
        if directory.join(".git").exists() {
            return Some(directory.display().to_string());
        }
        directory = directory.parent()?;
    }
}

#[derive(Debug, Deserialize)]
struct Record {
    timestamp: String,
    #[serde(flatten)]
    payload: Payload,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
enum Payload {
    SessionMeta(SessionMeta),
    TurnContext(TurnContext),
    EventMsg(EventMessage),
    ResponseItem(ResponseItem),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct SessionMeta {
    id: String,
    #[serde(default)]
    session_id: Option<String>,
    cwd: String,
    #[serde(default)]
    originator: Option<String>,
    #[serde(default)]
    source: Option<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Source {
    Name(String),
    Detail(BTreeMap<String, serde_json::Value>),
}

impl Source {
    fn is_subagent(&self) -> bool {
        matches!(self, Self::Detail(detail) if detail.contains_key("subagent"))
    }

    fn surface(self) -> Option<String> {
        match self {
            Self::Name(name) => Some(format!("codex-{name}")),
            Self::Detail(detail) if detail.contains_key("subagent") => {
                Some("codex-subagent".to_owned())
            }
            Self::Detail(_) => Some("codex".to_owned()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TurnContext {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EventMessage {
    TaskStarted,
    TaskComplete,
    TurnAborted {
        #[serde(default)]
        reason: Option<String>,
    },
    TokenCount {
        #[serde(default)]
        info: Option<TokenInfo>,
    },
    PatchApplyEnd {
        #[serde(default)]
        success: bool,
        #[serde(default)]
        changes: BTreeMap<String, Change>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct Change {
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenInfo {
    #[serde(default)]
    last_token_usage: Option<CodexUsage>,
    #[serde(default)]
    total_token_usage: Option<CodexUsage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl CodexUsage {
    fn snapshot_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.input_tokens,
            self.cached_input_tokens,
            self.cache_write_input_tokens,
            self.output_tokens,
            self.reasoning_output_tokens,
            self.total_tokens
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseItem {
    CustomToolCall {
        name: String,
        input: String,
        #[serde(default)]
        call_id: Option<String>,
    },
    FunctionCall {
        name: String,
        #[serde(default)]
        call_id: Option<String>,
    },
    CustomToolCallOutput {
        #[serde(default)]
        call_id: Option<String>,
        #[serde(default)]
        output: Vec<OutputChunk>,
    },
    FunctionCallOutput {
        #[serde(default)]
        call_id: Option<String>,
        #[serde(default)]
        output: Vec<OutputChunk>,
    },
    #[serde(other)]
    Other,
}

/// One piece of a tool's reported output.
///
/// The first piece is a status preamble the runner writes — `Script completed`
/// or `Script failed`, followed by a wall time. The pieces after it are the
/// command's actual output, and that is not ours to keep.
#[derive(Debug, Deserialize)]
struct OutputChunk {
    #[serde(default)]
    text: Option<StatusPrefix>,
}

/// At most [`STATUS_PREFIX_BYTES`] of a text field, truncated during parsing.
///
/// The status a tool reports and the output it produced arrive in the same
/// array of strings, so the status cannot be read without the parser touching
/// the output. What it can be denied is the chance to *keep* it: the visitor
/// below truncates on the way in, so the longest value this type can ever hold
/// is a few dozen bytes — enough for `Script failed`, nowhere near enough for a
/// command's output. serde hands the visitor a borrowed slice for a string with
/// no escapes, so in the ordinary case the full text is never allocated at all.
#[derive(Debug)]
struct StatusPrefix(String);

/// How much of a text field is retained.
///
/// Sized for the longest status line the runner is known to write, with room
/// for one that is longer. Deliberately far too small to hold output.
const STATUS_PREFIX_BYTES: usize = 48;

impl<'de> Deserialize<'de> for StatusPrefix {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = StatusPrefix;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                // Truncated on a character boundary: slicing a multi-byte
                // sequence in half would panic, and a status line is not worth
                // a crash in a reader whose whole job is to tolerate whatever
                // it is handed.
                let end = value
                    .char_indices()
                    .map(|(index, _)| index)
                    .take_while(|index| *index <= STATUS_PREFIX_BYTES)
                    .last()
                    .unwrap_or(0);
                Ok(StatusPrefix(value[..end].to_owned()))
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

impl OutputChunk {
    /// Whether this status preamble says the tool failed.
    fn reports_failure(&self) -> bool {
        self.text
            .as_ref()
            .is_some_and(|text| text.0.starts_with("Script failed"))
    }
}

#[derive(Debug, Deserialize)]
struct ExecCall {
    cmd: String,
    #[serde(default)]
    workdir: Option<String>,
}

fn exec_calls(input: &str) -> Vec<ExecCall> {
    const NEEDLE: &str = "tools.exec_command(";
    let mut calls = Vec::new();
    let mut remainder = input;
    while let Some(index) = remainder.find(NEEDLE) {
        let json = &remainder[index + NEEDLE.len()..];
        let mut deserializer = serde_json::Deserializer::from_str(json);
        if let Ok(call) = ExecCall::deserialize(&mut deserializer) {
            calls.push(call);
        }
        remainder = &json[json.len().min(1)..];
    }
    calls
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use agentwatch_types::SessionId;

    use super::*;

    const ROLLOUT: &str = r#"
{"timestamp":"2026-08-21T12:00:00Z","type":"session_meta","payload":{"id":"s-1","cwd":"/work","originator":"codex_vscode","source":"vscode"}}
{"timestamp":"2026-08-21T12:00:01Z","type":"turn_context","payload":{"cwd":"/work","model":"gpt-5.6-sol","effort":"high"}}
{"timestamp":"2026-08-21T12:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"cargo test\",\"workdir\":\"/work/agentwatch\"}); text(r.output);"}}
{"timestamp":"2026-08-21T12:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":70,"cache_write_input_tokens":5,"output_tokens":20,"reasoning_output_tokens":10,"total_tokens":125}}}}
{"timestamp":"2026-08-21T12:00:04Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":70,"cache_write_input_tokens":5,"output_tokens":20,"reasoning_output_tokens":10,"total_tokens":125}}}}
{"timestamp":"2026-08-21T12:00:05Z","type":"event_msg","payload":{"type":"patch_apply_end","success":true,"changes":{"/work/agentwatch/src/main.rs":{"type":"update","unified_diff":"secret source text"}}}}
{"timestamp":"2026-08-21T12:00:06Z","type":"event_msg","payload":{"type":"task_complete","last_agent_message":"not retained"}}
"#;

    /// A call and the output answering it, six seconds later, reporting failure.
    const CALL_AND_OUTPUT: &str = r#"
{"timestamp":"2026-08-21T12:00:00Z","type":"session_meta","payload":{"id":"s-1","cwd":"/work"}}
{"timestamp":"2026-08-21T12:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_x","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"cargo test\"}); text(r.output);"}}
{"timestamp":"2026-08-21T12:00:08Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_x","output":[{"type":"input_text","text":"Script failed\nWall time 6.0 seconds\nOutput:\n"},{"type":"input_text","text":"AKIAIOSFODNN7EXAMPLE leaked from the command"}]}}
"#;

    #[test]
    fn a_codex_tool_call_is_paired_with_its_output() {
        // Reliability was Claude-only until this: the same report covering one
        // agent and silently omitting the other is worse than not having it.
        let (events, summary) =
            read_rollout_from(Cursor::new(CALL_AND_OUTPUT), None).expect("read");
        assert_eq!(summary.tool_outcomes, 1);

        let outcome = events
            .iter()
            .find_map(|event| match &event.event {
                Event::ToolOutcome(outcome) => Some(outcome),
                _ => None,
            })
            .expect("an outcome");

        assert_eq!(outcome.tool, "exec");
        assert_eq!(outcome.tool_use_id, "call_x");
        assert_eq!(outcome.duration_ms, Some(6_000));
        assert!(outcome.failed, "the runner said the script failed");
    }

    #[test]
    fn the_output_beside_the_status_line_is_never_retained() {
        // Codex puts the runner's status and the command's own output in the
        // same array of strings, so the parser cannot avoid touching the
        // output — only avoid keeping it. Everything it reads is truncated to
        // a few dozen bytes on the way in.
        let (events, _) = read_rollout_from(Cursor::new(CALL_AND_OUTPUT), None).expect("read");
        let encoded = serde_json::to_string(&events).expect("serializable");
        assert!(!encoded.contains("AKIA"), "{encoded}");
        assert!(!encoded.contains("leaked"), "{encoded}");
    }

    #[test]
    fn a_status_line_is_truncated_rather_than_kept_whole() {
        let long = "x".repeat(4_000);
        let json = format!("\"{long}\"");
        let prefix: StatusPrefix = serde_json::from_str(&json).expect("parses");
        assert!(
            prefix.0.len() <= STATUS_PREFIX_BYTES + 1,
            "kept {} bytes",
            prefix.0.len()
        );
    }

    #[test]
    fn an_output_with_no_matching_call_is_ignored() {
        let text = r#"
{"timestamp":"2026-08-21T12:00:00Z","type":"session_meta","payload":{"id":"s-1","cwd":"/work"}}
{"timestamp":"2026-08-21T12:00:08Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"orphan","output":[]}}
"#;
        let (_, summary) = read_rollout_from(Cursor::new(text), None).expect("read");
        assert_eq!(summary.tool_outcomes, 0);
    }

    #[test]
    fn captures_real_metadata_without_content_and_deduplicates_usage() {
        let (events, summary) = read_rollout_from(Cursor::new(ROLLOUT), None).expect("read");
        assert_eq!(summary.responses, 1);
        assert_eq!(summary.commands, 1);
        assert_eq!(summary.file_writes, 1);

        let usage = events
            .iter()
            .find_map(|event| match &event.event {
                Event::TokenUsage(usage) => Some(usage),
                _ => None,
            })
            .expect("usage");
        assert_eq!(usage.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(usage.input_tokens, 30, "cached input is not counted twice");
        assert_eq!(usage.cache_read_input_tokens, 70);
        assert_eq!(usage.cache_creation_input_tokens, 5);
        assert_eq!(usage.total(), 125);

        let encoded = serde_json::to_string(&events).expect("encode");
        assert!(!encoded.contains("secret source text"));
        assert!(!encoded.contains("not retained"));
        assert!(
            events
                .iter()
                .any(|event| event.project_path.as_deref() == Some("/work/agentwatch"))
        );
    }

    #[test]
    fn extracts_every_exec_call_from_parallel_javascript() {
        let calls = exec_calls(
            r#"const [a,b]=await Promise.all([tools.exec_command({"cmd":"cargo test","workdir":"/a"}),tools.exec_command({"cmd":"cargo clippy","workdir":"/b"})]);"#,
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].workdir.as_deref(), Some("/b"));
    }

    #[test]
    fn cumulative_snapshot_distinguishes_equal_sized_responses() {
        let text = r#"
{"timestamp":"2026-08-21T12:00:00Z","type":"session_meta","payload":{"id":"s-2","cwd":"/work","source":"cli"}}
{"timestamp":"2026-08-21T12:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15},"total_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}}
{"timestamp":"2026-08-21T12:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15},"total_token_usage":{"input_tokens":20,"output_tokens":10,"total_tokens":30}}}}
{"timestamp":"2026-08-21T12:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15},"total_token_usage":{"input_tokens":20,"output_tokens":10,"total_tokens":30}}}}
"#;
        let (_, summary) = read_rollout_from(Cursor::new(text), None).expect("read");
        assert_eq!(
            summary.responses, 2,
            "only the repeated cumulative snapshot is a duplicate"
        );
    }

    #[test]
    fn links_a_subagent_thread_to_its_parent_without_merging_lifecycles() {
        let text = r#"
{"timestamp":"2026-08-21T12:00:00Z","type":"session_meta","payload":{"id":"child-thread","session_id":"parent-thread","cwd":"/work","source":{"subagent":{"other":"reviewer"}},"thread_source":"subagent"}}
{"timestamp":"2026-08-21T12:00:01Z","type":"turn_context","payload":{"model":"gpt-5.6-sol"}}
{"timestamp":"2026-08-21T12:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}}
"#;
        let (events, _) = read_rollout_from(Cursor::new(text), None).expect("read");
        let expected_parent = SessionId::from_external(
            &AgentId::CODEX,
            &ExternalSessionId::from("parent-thread".to_owned()),
        );
        let expected_child = SessionId::from_external(
            &AgentId::CODEX,
            &ExternalSessionId::from("child-thread".to_owned()),
        );

        assert!(
            events
                .iter()
                .all(|event| event.session_id == Some(expected_child))
        );
        assert!(
            events
                .iter()
                .all(|event| event.parent_session_id == Some(expected_parent))
        );
        let usage = events
            .iter()
            .find_map(|event| match &event.event {
                Event::TokenUsage(usage) => Some(usage),
                _ => None,
            })
            .expect("usage");
        assert!(usage.is_subagent);
    }
}
