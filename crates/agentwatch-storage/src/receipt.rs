//! Detailed, session-scoped data used to build a session receipt.

use rusqlite::params;

use crate::{
    Notable, TokenTotals,
    store::{Store, StoreError},
};

/// One file path touched during a session, with repeated accesses combined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptFile {
    /// The path reported by the agent tool.
    pub path: String,
    /// Number of observed reads.
    pub reads: i64,
    /// Number of observed writes.
    pub writes: i64,
    /// First observed access.
    pub first_seen_us: i64,
    /// Last observed access.
    pub last_seen_us: i64,
    /// Distinct tools that reported the access.
    pub tools: String,
    /// Repository or working directory associated with the access.
    pub project: Option<String>,
}

/// One shell command observed during a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptCommand {
    /// When the command ran.
    pub timestamp_us: i64,
    /// Sanitized command line stored by AgentWatch.
    pub command: String,
    /// Optional description supplied by the agent.
    pub description: Option<String>,
    /// Highest sensitivity of a path referenced by the command.
    pub sensitivity: String,
    /// Repository or working directory associated with the command.
    pub project: Option<String>,
}

/// Token usage for one model and execution role within a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptTokenGroup {
    /// Provider model identifier, or `(unknown)` when it was not reported.
    pub model: String,
    /// Whether these responses came from a spawned subagent.
    pub is_subagent: bool,
    /// Token and response totals for this model/role pair.
    pub totals: TokenTotals,
}

impl Store {
    /// Lists the distinct files touched by one session, busiest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn receipt_files(&self, session_id: &str) -> Result<Vec<ReceiptFile>, StoreError> {
        let mut statement = self.connection().prepare(
            "WITH RECURSIVE receipt_sessions(id) AS (
                 SELECT ?1
                 UNION
                 SELECT s.id
                   FROM sessions s
                   JOIN receipt_sessions parent ON s.parent_session_id = parent.id
             )
             SELECT f.path,
                    SUM(CASE WHEN f.operation = 'read'  THEN 1 ELSE 0 END),
                    SUM(CASE WHEN f.operation = 'write' THEN 1 ELSE 0 END),
                    MIN(f.timestamp_us), MAX(f.timestamp_us),
                    GROUP_CONCAT(DISTINCT f.tool),
                    COALESCE(r.root, p.path)
               FROM file_events f
               LEFT JOIN projects p     ON p.id = f.project_id
               LEFT JOIN repositories r ON r.id = p.repository_id
              WHERE f.session_id IN (SELECT id FROM receipt_sessions)
              GROUP BY f.path, COALESCE(r.root, p.path)
              ORDER BY COUNT(*) DESC, MAX(f.timestamp_us) DESC, f.path",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok(ReceiptFile {
                path: row.get(0)?,
                reads: row.get(1)?,
                writes: row.get(2)?,
                first_seen_us: row.get(3)?,
                last_seen_us: row.get(4)?,
                tools: row.get(5)?,
                project: row.get(6)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Lists commands for one session in execution order.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn receipt_commands(&self, session_id: &str) -> Result<Vec<ReceiptCommand>, StoreError> {
        let mut statement = self.connection().prepare(
            "WITH RECURSIVE receipt_sessions(id) AS (
                 SELECT ?1
                 UNION
                 SELECT s.id
                   FROM sessions s
                   JOIN receipt_sessions parent ON s.parent_session_id = parent.id
             )
             SELECT c.timestamp_us, c.command, c.description, c.sensitivity,
                    COALESCE(r.root, p.path)
               FROM command_events c
               LEFT JOIN projects p     ON p.id = c.project_id
               LEFT JOIN repositories r ON r.id = p.repository_id
              WHERE c.session_id IN (SELECT id FROM receipt_sessions)
              ORDER BY c.timestamp_us, c.id",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok(ReceiptCommand {
                timestamp_us: row.get(0)?,
                command: row.get(1)?,
                description: row.get(2)?,
                sensitivity: row.get(3)?,
                project: row.get(4)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Breaks one session's token usage down by model and main/subagent role.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn receipt_tokens(&self, session_id: &str) -> Result<Vec<ReceiptTokenGroup>, StoreError> {
        let mut statement = self.connection().prepare(
            "WITH RECURSIVE receipt_sessions(id) AS (
                 SELECT ?1
                 UNION
                 SELECT s.id
                   FROM sessions s
                   JOIN receipt_sessions parent ON s.parent_session_id = parent.id
             )
             SELECT COALESCE(model, '(unknown)'), is_subagent,
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(cache_creation_input_tokens), 0),
                    COALESCE(SUM(cache_read_input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COUNT(*)
               FROM token_usage
              WHERE session_id IN (SELECT id FROM receipt_sessions)
              GROUP BY model, is_subagent
              ORDER BY SUM(input_tokens + cache_creation_input_tokens
                           + cache_read_input_tokens + output_tokens) DESC,
                       model, is_subagent",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok(ReceiptTokenGroup {
                model: row.get(0)?,
                is_subagent: row.get(1)?,
                totals: TokenTotals {
                    input: row.get(2)?,
                    cache_creation: row.get(3)?,
                    cache_read: row.get(4)?,
                    output: row.get(5)?,
                    responses: row.get(6)?,
                },
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Lists sensitive file access and command-path references for one session.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn receipt_notable_access(&self, session_id: &str) -> Result<Vec<Notable>, StoreError> {
        let mut statement = self.connection().prepare(
            "WITH RECURSIVE receipt_sessions(id) AS (
                 SELECT ?1
                 UNION
                 SELECT s.id
                   FROM sessions s
                   JOIN receipt_sessions parent ON s.parent_session_id = parent.id
             )
             SELECT timestamp_us, sensitivity, kind, path, evidence, project
               FROM (
                 SELECT f.timestamp_us, f.sensitivity, f.operation AS kind,
                        f.path, e.evidence, COALESCE(r.root, p.path) AS project
                   FROM file_events f
                   JOIN events e       ON e.id = f.id
                   LEFT JOIN projects p     ON p.id = f.project_id
                   LEFT JOIN repositories r ON r.id = p.repository_id
                  WHERE f.session_id IN (SELECT id FROM receipt_sessions)
                    AND f.sensitivity != 'normal'
                 UNION ALL
                 SELECT c.timestamp_us, c.sensitivity, 'command', c.path, 'derived',
                        COALESCE(r.root, p.path)
                   FROM command_path_references c
                   LEFT JOIN projects p     ON p.id = c.project_id
                   LEFT JOIN repositories r ON r.id = p.repository_id
                  WHERE c.session_id IN (SELECT id FROM receipt_sessions)
                    AND c.sensitivity != 'normal'
               )
              ORDER BY timestamp_us, kind, path",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok(Notable {
                timestamp_us: row.get(0)?,
                sensitivity: row.get(1)?,
                kind: row.get(2)?,
                path: row.get(3)?,
                evidence: row.get(4)?,
                project: row.get(5)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{
        AgentEvent, CommandEvent, Event, EvidenceSource, FileEvent, TokenUsageEvent,
    };
    use agentwatch_types::{AgentId, ExternalSessionId, Timestamp};

    use super::*;

    fn seeded() -> (Store, String) {
        let mut store = Store::open_in_memory().expect("schema");
        let external = ExternalSessionId::from("receipt-session".to_owned());
        let child_external = ExternalSessionId::from("receipt-child".to_owned());
        let base = |event: Event, micros: i64| {
            AgentEvent::observed(AgentId::CODEX, EvidenceSource::Transcript, event)
                .with_session(external.clone())
                .with_project_path("/work/agentwatch".to_owned())
                .at(Timestamp::from_micros(micros))
        };
        let token = |request: &str, model: &str, is_subagent: bool, output: u64, micros: i64| {
            base(
                Event::TokenUsage(TokenUsageEvent {
                    provider: "openai".to_owned(),
                    model: Some(model.to_owned()),
                    request_id: Some(request.to_owned()),
                    input_tokens: 10,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 20,
                    output_tokens: output,
                    is_subagent,
                    provider_usage: serde_json::Map::new(),
                }),
                micros,
            )
        };
        let child_token = AgentEvent::observed(
            AgentId::CODEX,
            EvidenceSource::Transcript,
            Event::TokenUsage(TokenUsageEvent {
                provider: "openai".to_owned(),
                model: Some("gpt-child".to_owned()),
                request_id: Some("response-child".to_owned()),
                input_tokens: 10,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 20,
                output_tokens: 40,
                is_subagent: true,
                provider_usage: serde_json::Map::new(),
            }),
        )
        .with_session(child_external.clone())
        .with_parent_session(Some(external.clone()))
        .with_project_path("/work/agentwatch".to_owned())
        .at(Timestamp::from_micros(6));
        let grandchild_token = AgentEvent::observed(
            AgentId::CODEX,
            EvidenceSource::Transcript,
            Event::TokenUsage(TokenUsageEvent {
                provider: "openai".to_owned(),
                model: Some("gpt-grandchild".to_owned()),
                request_id: Some("response-grandchild".to_owned()),
                input_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 2,
                output_tokens: 3,
                is_subagent: true,
                provider_usage: serde_json::Map::new(),
            }),
        )
        .with_session(ExternalSessionId::from("receipt-grandchild".to_owned()))
        .with_parent_session(Some(child_external.clone()))
        .with_project_path("/work/nested".to_owned())
        .at(Timestamp::from_micros(7));
        let grandchild_command = AgentEvent::observed(
            AgentId::CODEX,
            EvidenceSource::Transcript,
            Event::Command(CommandEvent {
                command: "cargo clippy".to_owned(),
                description: None,
            }),
        )
        .with_session(ExternalSessionId::from("receipt-grandchild".to_owned()))
        .with_parent_session(Some(child_external))
        .with_project_path("/work/nested".to_owned())
        .at(Timestamp::from_micros(8));

        store
            .insert_events(&[
                base(
                    Event::FileRead(FileEvent {
                        path: "/work/agentwatch/src/main.rs".to_owned(),
                        tool: "read_file".to_owned(),
                    }),
                    1,
                ),
                base(
                    Event::FileWrite(FileEvent {
                        path: "/work/agentwatch/src/main.rs".to_owned(),
                        tool: "apply_patch".to_owned(),
                    }),
                    2,
                ),
                base(
                    Event::FileRead(FileEvent {
                        path: "/Users/dev/.ssh/id_ed25519".to_owned(),
                        tool: "read_file".to_owned(),
                    }),
                    3,
                ),
                base(
                    Event::Command(CommandEvent {
                        command: "cargo test".to_owned(),
                        description: Some("run tests".to_owned()),
                    }),
                    4,
                ),
                token("response-main", "gpt-main", false, 30, 5),
                child_token,
                grandchild_token,
                grandchild_command,
            ])
            .expect("insert");

        let session_id = store
            .sessions(0, i64::MAX, 10)
            .expect("sessions")
            .into_iter()
            .find(|session| !session.is_subagent)
            .expect("main session")
            .id;
        (store, session_id)
    }

    #[test]
    fn files_are_combined_by_path_with_read_and_write_counts() {
        let (store, session_id) = seeded();
        let files = store.receipt_files(&session_id).expect("files");
        let source = files
            .iter()
            .find(|file| file.path.ends_with("src/main.rs"))
            .expect("source file");

        assert_eq!((source.reads, source.writes), (1, 1));
        assert!(source.tools.contains("read_file"));
        assert!(source.tools.contains("apply_patch"));
    }

    #[test]
    fn commands_and_sensitive_access_are_scoped_to_the_session() {
        let (store, session_id) = seeded();
        let commands = store.receipt_commands(&session_id).expect("commands");
        let notable = store
            .receipt_notable_access(&session_id)
            .expect("notable access");

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].command, "cargo test");
        assert_eq!(commands[1].command, "cargo clippy");
        assert_eq!(notable.len(), 1);
        assert!(notable[0].path.ends_with(".ssh/id_ed25519"));
        assert_eq!(notable[0].evidence, "transcript");
    }

    #[test]
    fn tokens_are_split_by_model_and_execution_role() {
        let (store, session_id) = seeded();
        let groups = store.receipt_tokens(&session_id).expect("tokens");

        assert_eq!(groups.len(), 3);
        let main = groups
            .iter()
            .find(|group| !group.is_subagent)
            .expect("main");
        let child = groups
            .iter()
            .find(|group| group.is_subagent)
            .expect("child");
        assert_eq!((main.model.as_str(), main.totals.total()), ("gpt-main", 60));
        assert_eq!(
            (child.model.as_str(), child.totals.total()),
            ("gpt-child", 70)
        );
        let grandchild = groups
            .iter()
            .find(|group| group.model == "gpt-grandchild")
            .expect("grandchild");
        assert!(grandchild.is_subagent);
        assert_eq!(grandchild.totals.total(), 6);
    }

    #[test]
    fn timeline_projects_and_coverage_include_nested_subagents() {
        let (store, session_id) = seeded();
        let timeline = store
            .activity(
                0,
                i64::MAX,
                &crate::ActivityFilter {
                    session: Some(session_id.clone()),
                    include_subagents: true,
                    ..crate::ActivityFilter::default()
                },
                u32::MAX,
            )
            .expect("timeline");
        let projects = store.projects_for_session(&session_id).expect("projects");
        let coverage = store.coverage(&session_id).expect("coverage");

        assert!(
            timeline
                .iter()
                .any(|event| event.project_path.as_deref() == Some("/work/nested"))
        );
        assert!(projects.iter().any(|project| project == "/work/nested"));
        assert!(coverage.tokens);
        assert!(coverage.commands);
    }

    #[test]
    fn a_child_session_without_token_usage_is_still_identified_as_a_subagent() {
        let mut store = Store::open_in_memory().expect("schema");
        let event = AgentEvent::observed(
            AgentId::CODEX,
            EvidenceSource::Transcript,
            Event::Command(CommandEvent {
                command: "git status".to_owned(),
                description: None,
            }),
        )
        .with_session(ExternalSessionId::from("command-only-child".to_owned()))
        .with_parent_session(Some(ExternalSessionId::from("parent".to_owned())));
        store.insert_events(&[event]).expect("insert");

        let child = store.sessions(0, i64::MAX, 10).expect("sessions");
        assert_eq!(child.len(), 1);
        assert!(child[0].is_subagent);
    }
}
