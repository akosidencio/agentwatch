//! The write side.

use agentwatch_events::{AgentEvent, Event, FileEvent, classify, scan_command, worst_in_command};
use agentwatch_types::Timestamp;
use rusqlite::{Connection, OpenFlags, Transaction, params};
use std::path::Path;

use crate::migrations;

/// Something went wrong talking to SQLite.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The database rejected a statement.
    #[error("database error")]
    Sqlite(#[from] rusqlite::Error),
    /// The database was written by an older build than this one.
    #[error(
        "database is at schema version {found}, this build expects {expected}\n\
         Run `agentwatch import`, or start the collector with `agentwatch daemon`, to migrate it."
    )]
    SchemaTooOld {
        /// Version the database is at.
        found: i64,
        /// Version this build needs.
        expected: i64,
    },
    /// An event could not be encoded for storage.
    #[error("could not encode event payload")]
    Encode(#[from] serde_json::Error),
}

/// A connection to the event database.
#[derive(Debug)]
pub struct Store {
    pub(crate) connection: Connection,
}

impl Store {
    /// Opens the database for writing, creating and migrating it if needed.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or a migration fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let mut connection = Connection::open(path)?;
        Self::configure(&connection)?;
        migrations::apply(&mut connection)?;
        Ok(Self { connection })
    }

    /// Opens an in-memory database, for tests.
    ///
    /// # Errors
    ///
    /// Returns an error if the schema cannot be created.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let mut connection = Connection::open_in_memory()?;
        migrations::apply(&mut connection)?;
        Ok(Self { connection })
    }

    /// Opens the database for reading, for the CLI.
    ///
    /// Deliberately *not* `SQLITE_OPEN_READ_ONLY`: a WAL database needs to
    /// create its `-shm` companion on open, which a read-only handle cannot do,
    /// so that flag fails whenever the daemon is not already running. Opening
    /// normally and setting `query_only` gives the same guarantee without the
    /// failure mode.
    ///
    /// Does not migrate: a reader must never race the writer's schema changes.
    ///
    /// # Errors
    ///
    /// Returns an error if the file does not exist or cannot be opened.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI;
        let connection = Connection::open_with_flags(path, flags)?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;

        // Checked before `query_only` is set, and before any query runs: a
        // reader that skips this hits a missing table and reports it as a
        // database error, which tells the user nothing about what to do.
        let found = migrations::current_version(&connection)?;
        let expected = migrations::expected_version();
        if found < expected {
            return Err(StoreError::SchemaTooOld { found, expected });
        }

        connection.pragma_update(None, "query_only", "ON")?;
        Ok(Self { connection })
    }

    /// Applies the pragmas the daemon depends on.
    fn configure(connection: &Connection) -> rusqlite::Result<()> {
        // WAL lets the CLI read while the daemon writes. NORMAL synchronous is
        // the standard WAL pairing: a crash can lose the last commits, which for
        // analytics events is an acceptable trade for not fsyncing per batch.
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;
        Ok(())
    }

    /// Borrows the underlying connection for reads.
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Borrows the connection so tests in other crates can set up awkward states.
    ///
    /// Not part of the supported surface: reach for a real method first.
    #[doc(hidden)]
    pub fn connection_for_test(&self) -> &Connection {
        &self.connection
    }

    /// Writes a batch of events in one transaction.
    ///
    /// Idempotent: re-inserting an event id is ignored, so a replayed batch
    /// after a crash cannot duplicate rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the transaction cannot be committed. Nothing in the
    /// batch is written in that case.
    pub fn insert_events(&mut self, events: &[AgentEvent]) -> Result<usize, StoreError> {
        if events.is_empty() {
            return Ok(0);
        }

        let transaction = self.connection.transaction()?;
        let mut written = 0;
        for event in events {
            written += insert_one(&transaction, event)?;
        }
        transaction.commit()?;
        Ok(written)
    }
}

/// Writes a single event and the identity rows it implies.
///
/// The spine row goes in first, and its result decides whether the rest runs.
/// An event id that is already stored means every row this event implies was
/// written by the same transaction that stored it, so a replay — a crashed
/// batch, a second reconcile pass over the same transcript — costs one
/// statement rather than five per event and cannot re-apply a session's
/// lifecycle updates on top of themselves.
fn insert_one(transaction: &Transaction<'_>, event: &AgentEvent) -> Result<usize, StoreError> {
    let now = Timestamp::now().as_micros();
    let timestamp = event.timestamp.as_micros();

    let payload = serde_json::to_string(&event.event)?;
    let inserted = transaction.execute(
        "INSERT OR IGNORE INTO events
            (id, timestamp_us, agent_id, session_id, project_id, kind, evidence, confidence, payload, created_at_us)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            event.id.to_string(),
            timestamp,
            event.agent_id.as_str(),
            event.session_id.map(|id| id.to_string()),
            event.project_id.map(|id| id.to_string()),
            event.kind(),
            event.evidence.as_str(),
            f64::from(event.confidence.value()),
            payload,
            now,
        ],
    )?;

    if inserted == 0 {
        return Ok(0);
    }

    transaction.execute(
        "INSERT INTO agents (id, first_seen_us, last_seen_us)
         VALUES (?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET last_seen_us = MAX(last_seen_us, excluded.last_seen_us)",
        params![event.agent_id.as_str(), timestamp],
    )?;

    if let (Some(project_id), Some(path)) = (event.project_id, event.project_path.as_deref()) {
        transaction.execute(
            "INSERT INTO projects (id, path, first_seen_us, last_seen_us)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(id) DO UPDATE SET last_seen_us = MAX(last_seen_us, excluded.last_seen_us)",
            params![project_id.to_string(), path, timestamp],
        )?;
    }

    if let Some(session_id) = event.session_id {
        upsert_session(transaction, event, &session_id.to_string(), timestamp, now)?;
    }

    write_projection(transaction, event, now)?;

    Ok(inserted)
}

/// Writes the typed row for events that have one.
///
/// The `events` table stays the append-only spine — every event lands there
/// whatever its kind. These tables are the queryable projection of the kinds
/// that analytics actually asks about, so a token query never has to parse
/// JSON out of a payload column.
fn write_projection(
    transaction: &Transaction<'_>,
    event: &AgentEvent,
    now: i64,
) -> Result<(), StoreError> {
    let id = event.id.to_string();
    let timestamp = event.timestamp.as_micros();
    let agent = event.agent_id.as_str();
    let session = event.session_id.map(|id| id.to_string());
    let project = event.project_id.map(|id| id.to_string());

    match &event.event {
        Event::TokenUsage(usage) => {
            let provider_usage = serde_json::to_string(&usage.provider_usage)?;
            // OR IGNORE plus the unique index on (agent_id, request_id) is what
            // makes the reconcile pass safe to run repeatedly.
            transaction.execute(
                "INSERT OR IGNORE INTO token_usage
                    (id, timestamp_us, agent_id, session_id, project_id, provider, model,
                     request_id, input_tokens, cache_creation_input_tokens,
                     cache_read_input_tokens, output_tokens, is_subagent, provider_usage,
                     created_at_us)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    id,
                    timestamp,
                    agent,
                    session,
                    project,
                    usage.provider,
                    usage.model,
                    usage.request_id.as_deref().unwrap_or(id.as_str()),
                    usage.input_tokens,
                    usage.cache_creation_input_tokens,
                    usage.cache_read_input_tokens,
                    usage.output_tokens,
                    i64::from(usage.is_subagent),
                    provider_usage,
                    now,
                ],
            )?;
        }
        Event::FileRead(file) => write_file(
            transaction,
            &id,
            timestamp,
            agent,
            &session,
            &project,
            "read",
            file,
            now,
        )?,
        Event::FileWrite(file) => write_file(
            transaction,
            &id,
            timestamp,
            agent,
            &session,
            &project,
            "write",
            file,
            now,
        )?,
        Event::Command(command) => {
            transaction.execute(
                "INSERT OR IGNORE INTO command_events
                    (id, timestamp_us, agent_id, session_id, project_id, command, description, sensitivity, created_at_us)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    timestamp,
                    agent,
                    session,
                    project,
                    command.command,
                    command.description,
                    worst_in_command(&command.command).as_str(),
                    now,
                ],
            )?;

            // A shell command is opaque to tool-level reporting, so anything it
            // referred to is recorded separately as inference.
            for (index, reference) in scan_command(&command.command).into_iter().enumerate() {
                transaction.execute(
                    "INSERT OR IGNORE INTO command_path_references
                        (id, command_id, timestamp_us, agent_id, session_id, project_id, path, sensitivity, created_at_us)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        format!("{id}:{index}"),
                        id,
                        timestamp,
                        agent,
                        session,
                        project,
                        reference.path,
                        reference.sensitivity.as_str(),
                        now,
                    ],
                )?;
            }
        }
        Event::McpCall(mcp) => {
            transaction.execute(
                "INSERT OR IGNORE INTO mcp_events
                    (id, timestamp_us, agent_id, session_id, project_id, server, tool, created_at_us)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, timestamp, agent, session, project, mcp.server, mcp.tool, now],
            )?;
        }
        // No side table: the generic `events` row carries all of these, and
        // none has a per-kind dimension worth indexing separately.
        Event::SessionStarted(_)
        | Event::SessionEnded(_)
        | Event::Prompt(_)
        | Event::ToolCall(_)
        | Event::ConfigChanged(_)
        | Event::Collection(_)
        | Event::Unknown(_) => {}
    }

    Ok(())
}

/// Writes a file event row.
#[allow(clippy::too_many_arguments)]
fn write_file(
    transaction: &Transaction<'_>,
    id: &str,
    timestamp: i64,
    agent: &str,
    session: &Option<String>,
    project: &Option<String>,
    operation: &str,
    file: &FileEvent,
    now: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT OR IGNORE INTO file_events
            (id, timestamp_us, agent_id, session_id, project_id, operation, path, tool, sensitivity, created_at_us)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            timestamp,
            agent,
            session,
            project,
            operation,
            file.path,
            file.tool,
            classify(&file.path).as_str(),
            now,
        ],
    )?;
    Ok(())
}

/// Creates or advances the session row this event belongs to.
///
/// Session lifecycle is driven by the events themselves rather than by a
/// separate code path, so a session opened before the daemon started still
/// appears once any of its events arrive.
fn upsert_session(
    transaction: &Transaction<'_>,
    event: &AgentEvent,
    session_id: &str,
    timestamp: i64,
    now: i64,
) -> Result<(), StoreError> {
    let transcript_path = match &event.event {
        Event::SessionStarted(started) => started.transcript_path.as_deref(),
        _ => None,
    };

    // A session is only `active` if its start was actually observed. Sessions
    // discovered by reading a transcript were never watched running, and
    // reporting them as running would be a claim we cannot support.
    let status = if matches!(event.event, Event::SessionStarted(_)) {
        "active"
    } else {
        "unknown"
    };

    transaction.execute(
        "INSERT INTO sessions
            (id, agent_id, external_session_id, project_id, started_at_us, status, transcript_path, created_at_us, git_branch, surface)
         VALUES (?1, ?2, ?3, ?4, ?5, ?9, ?6, ?7, ?8, ?10)
         ON CONFLICT(id) DO UPDATE SET
            started_at_us   = MIN(COALESCE(started_at_us, excluded.started_at_us), excluded.started_at_us),
            project_id      = COALESCE(sessions.project_id, excluded.project_id),
            transcript_path = COALESCE(excluded.transcript_path, sessions.transcript_path),
            git_branch      = COALESCE(excluded.git_branch, sessions.git_branch),
            surface         = COALESCE(excluded.surface, sessions.surface),
            status          = CASE
                                WHEN sessions.status = 'unknown' THEN excluded.status
                                ELSE sessions.status
                              END",
        params![
            session_id,
            event.agent_id.as_str(),
            event.external_session_id.as_ref().map(|id| id.as_str()),
            event.project_id.map(|id| id.to_string()),
            timestamp,
            transcript_path,
            now,
            event.git_branch.as_deref(),
            status,
            event.surface.as_deref(),
        ],
    )?;

    if matches!(event.event, Event::SessionEnded(_)) {
        transaction.execute(
            "UPDATE sessions
                SET ended_at_us = ?2,
                    status      = 'ended',
                    duration_ms = CASE
                        WHEN started_at_us IS NULL THEN NULL
                        ELSE (?2 - started_at_us) / 1000
                    END
              WHERE id = ?1",
            params![session_id, timestamp],
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{
        AgentEvent, CommandEvent, EvidenceSource, SessionEnded, SessionStarted,
    };
    use agentwatch_types::{AgentId, ExternalSessionId};

    use super::*;

    fn command_event(session: &str) -> AgentEvent {
        AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::Command(CommandEvent {
                command: "cargo test".into(),
                description: None,
            }),
        )
        .with_session(ExternalSessionId::from(session.to_owned()))
        .with_project_path("/Users/dev/projects/acme".to_owned())
    }

    #[test]
    fn a_reader_refuses_a_database_from_an_older_build() {
        let directory = tempfile::tempdir().expect("temp dir");
        let database = directory.path().join("events.db");
        {
            let writer = Connection::open(&database).expect("open");
            writer
                .execute_batch(
                    "CREATE TABLE schema_migrations (
                        version INTEGER PRIMARY KEY, name TEXT NOT NULL,
                        applied_at TEXT NOT NULL DEFAULT (datetime('now'))) STRICT;
                     INSERT INTO schema_migrations (version, name) VALUES (1, 'old');",
                )
                .expect("seed an old schema");
        }

        let error = Store::open_read_only(&database).expect_err("should refuse");
        let message = format!("{error}");
        assert!(
            message.contains("agentwatch import"),
            "unhelpful message: {message}"
        );
    }

    #[test]
    fn a_reader_cannot_write() {
        let directory = tempfile::tempdir().expect("temp dir");
        let database = directory.path().join("events.db");
        {
            let mut writer = Store::open(&database).expect("open");
            writer
                .insert_events(&[command_event("s-1")])
                .expect("insert");
        }

        let mut reader = Store::open_read_only(&database).expect("reopen");
        assert!(
            reader.insert_events(&[command_event("s-2")]).is_err(),
            "query_only should reject writes"
        );
    }

    #[test]
    fn a_reader_can_open_a_wal_database_the_writer_has_closed() {
        let directory = tempfile::tempdir().expect("temp dir");
        let database = directory.path().join("events.db");
        {
            let mut writer = Store::open(&database).expect("open");
            writer
                .insert_events(&[command_event("s-1")])
                .expect("insert");
        }

        let reader = Store::open_read_only(&database).expect("reopen after the writer closed");
        assert_eq!(reader.totals().expect("totals").events, 1);
    }

    fn token_event(request_id: &str, output: u64) -> AgentEvent {
        use agentwatch_events::TokenUsageEvent;

        AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Transcript,
            Event::TokenUsage(TokenUsageEvent {
                provider: "anthropic".into(),
                model: Some("claude-opus-5".into()),
                request_id: Some(request_id.to_owned()),
                input_tokens: 2,
                cache_creation_input_tokens: 5_585,
                cache_read_input_tokens: 45_723,
                output_tokens: output,
                is_subagent: false,
                provider_usage: serde_json::Map::new(),
            }),
        )
        .with_id(agentwatch_types::EventId::from_key(
            &AgentId::CLAUDE_CODE,
            request_id,
        ))
        .with_session(ExternalSessionId::from("s-1".to_owned()))
    }

    #[test]
    fn a_repeated_reconcile_pass_does_not_double_count_tokens() {
        let mut store = Store::open_in_memory().expect("schema");

        store
            .insert_events(&[token_event("msg_1", 497)])
            .expect("first pass");
        store
            .insert_events(&[token_event("msg_1", 497)])
            .expect("second pass");
        store
            .insert_events(&[token_event("msg_1", 497)])
            .expect("third pass");

        let (rows, output): (i64, i64) = store
            .connection()
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(output_tokens), 0) FROM token_usage",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query");

        assert_eq!(rows, 1, "one response must produce exactly one row");
        assert_eq!(output, 497, "re-reconciling must not inflate totals");
    }

    #[test]
    fn distinct_responses_each_get_a_row() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[token_event("msg_1", 100), token_event("msg_2", 200)])
            .expect("insert");

        let output: i64 = store
            .connection()
            .query_row("SELECT SUM(output_tokens) FROM token_usage", [], |row| {
                row.get(0)
            })
            .expect("query");
        assert_eq!(output, 300);
    }

    #[test]
    fn the_four_token_counters_are_stored_separately() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[token_event("msg_1", 497)])
            .expect("insert");

        let (input, creation, read, output): (i64, i64, i64, i64) = store
            .connection()
            .query_row(
                "SELECT input_tokens, cache_creation_input_tokens,
                        cache_read_input_tokens, output_tokens FROM token_usage",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("query");

        assert_eq!((input, creation, read, output), (2, 5_585, 45_723, 497));
    }

    #[test]
    fn a_file_event_lands_in_the_typed_table() {
        use agentwatch_events::FileEvent as Fe;

        let mut store = Store::open_in_memory().expect("schema");
        let event = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::FileRead(Fe {
                path: "/src/auth.rs".into(),
                tool: "Read".into(),
            }),
        )
        .with_session(ExternalSessionId::from("s-1".to_owned()));

        store.insert_events(&[event]).expect("insert");

        let (operation, path): (String, String) = store
            .connection()
            .query_row("SELECT operation, path FROM file_events", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("query");
        assert_eq!(
            (operation.as_str(), path.as_str()),
            ("read", "/src/auth.rs")
        );
    }

    #[test]
    fn a_sensitive_file_read_is_classified_at_ingest() {
        use agentwatch_events::FileEvent as Fe;

        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[AgentEvent::observed(
                AgentId::CLAUDE_CODE,
                EvidenceSource::Hook,
                Event::FileRead(Fe {
                    path: "/Users/dev/.aws/credentials".into(),
                    tool: "Read".into(),
                }),
            )])
            .expect("insert");

        let sensitivity: String = store
            .connection()
            .query_row("SELECT sensitivity FROM file_events", [], |row| row.get(0))
            .expect("query");
        assert_eq!(sensitivity, "highly_sensitive");
    }

    #[test]
    fn a_command_reading_dotenv_records_the_reference_it_implies() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[AgentEvent::observed(
                AgentId::CLAUDE_CODE,
                EvidenceSource::Hook,
                Event::Command(CommandEvent {
                    command: "cat .env | grep KEY".into(),
                    description: None,
                }),
            )])
            .expect("insert");

        let (path, sensitivity): (String, String) = store
            .connection()
            .query_row(
                "SELECT path, sensitivity FROM command_path_references",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query");
        assert_eq!((path.as_str(), sensitivity.as_str()), (".env", "sensitive"));

        let command_sensitivity: String = store
            .connection()
            .query_row("SELECT sensitivity FROM command_events", [], |row| {
                row.get(0)
            })
            .expect("query");
        assert_eq!(command_sensitivity, "sensitive");
    }

    #[test]
    fn an_ordinary_command_records_no_path_references() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[command_event("s-1")])
            .expect("insert");

        let references: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM command_path_references", [], |row| {
                row.get(0)
            })
            .expect("query");
        assert_eq!(references, 0);
    }

    #[test]
    fn a_command_and_an_mcp_call_land_in_their_typed_tables() {
        use agentwatch_events::McpEvent;

        let mut store = Store::open_in_memory().expect("schema");
        let command = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::Command(CommandEvent {
                command: "cargo test".into(),
                description: None,
            }),
        );
        let mcp = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::McpCall(McpEvent {
                server: "github".into(),
                tool: "get_issue".into(),
            }),
        );
        store.insert_events(&[command, mcp]).expect("insert");

        let commands: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM command_events", [], |row| row.get(0))
            .expect("query");
        let calls: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM mcp_events", [], |row| row.get(0))
            .expect("query");
        assert_eq!((commands, calls), (1, 1));
    }

    #[test]
    fn the_session_absorbs_the_surface() {
        let mut store = Store::open_in_memory().expect("schema");
        let event = command_event("s-1").with_surface(Some("claude-vscode".to_owned()));
        store.insert_events(&[event]).expect("insert");

        let surface: Option<String> = store
            .connection()
            .query_row("SELECT surface FROM sessions", [], |row| row.get(0))
            .expect("query");
        assert_eq!(surface.as_deref(), Some("claude-vscode"));
    }

    #[test]
    fn a_session_seen_only_through_hooks_has_no_surface() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[command_event("s-1")])
            .expect("insert");

        let surface: Option<String> = store
            .connection()
            .query_row("SELECT surface FROM sessions", [], |row| row.get(0))
            .expect("query");
        assert_eq!(surface, None, "unknown must not be filled in with a guess");
    }

    #[test]
    fn a_later_event_without_a_surface_does_not_erase_the_known_one() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[
                command_event("s-1").with_surface(Some("claude-vscode".to_owned())),
                command_event("s-1"),
            ])
            .expect("insert");

        let surface: Option<String> = store
            .connection()
            .query_row("SELECT surface FROM sessions", [], |row| row.get(0))
            .expect("query");
        assert_eq!(surface.as_deref(), Some("claude-vscode"));
    }

    #[test]
    fn the_session_absorbs_the_git_branch() {
        let mut store = Store::open_in_memory().expect("schema");
        let event = command_event("s-1").with_git_branch(Some("main".to_owned()));
        store.insert_events(&[event]).expect("insert");

        let branch: Option<String> = store
            .connection()
            .query_row("SELECT git_branch FROM sessions", [], |row| row.get(0))
            .expect("query");
        assert_eq!(branch.as_deref(), Some("main"));
    }

    #[test]
    fn inserting_a_batch_writes_every_event() {
        let mut store = Store::open_in_memory().expect("schema");
        let written = store
            .insert_events(&[command_event("s-1"), command_event("s-1")])
            .expect("insert");
        assert_eq!(written, 2);
    }

    #[test]
    fn reinserting_the_same_event_is_ignored() {
        let mut store = Store::open_in_memory().expect("schema");
        let event = command_event("s-1");

        assert_eq!(
            store
                .insert_events(std::slice::from_ref(&event))
                .expect("first"),
            1
        );
        assert_eq!(store.insert_events(&[event]).expect("second"), 0);
    }

    #[test]
    fn an_event_creates_its_agent_project_and_session() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[command_event("s-1")])
            .expect("insert");

        let agents: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))
            .expect("count");
        let projects: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
            .expect("count");
        let sessions: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .expect("count");

        assert_eq!((agents, projects, sessions), (1, 1, 1));
    }

    #[test]
    fn a_session_discovered_from_a_transcript_is_not_reported_as_running() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[token_event("msg_1", 10)])
            .expect("insert");

        let status: String = store
            .connection()
            .query_row("SELECT status FROM sessions", [], |row| row.get(0))
            .expect("query");
        assert_eq!(status, "unknown");
    }

    #[test]
    fn an_observed_start_makes_a_session_active() {
        let mut store = Store::open_in_memory().expect("schema");
        let external = ExternalSessionId::from("s-1".to_owned());
        store
            .insert_events(&[
                command_event("s-1"),
                AgentEvent::observed(
                    AgentId::CLAUDE_CODE,
                    EvidenceSource::Hook,
                    Event::SessionStarted(SessionStarted::default()),
                )
                .with_session(external),
            ])
            .expect("insert");

        let status: String = store
            .connection()
            .query_row("SELECT status FROM sessions", [], |row| row.get(0))
            .expect("query");
        assert_eq!(status, "active", "an observed start should upgrade unknown");
    }

    #[test]
    fn a_later_event_does_not_reopen_an_ended_session() {
        let mut store = Store::open_in_memory().expect("schema");
        let external = ExternalSessionId::from("s-1".to_owned());
        store
            .insert_events(&[
                AgentEvent::observed(
                    AgentId::CLAUDE_CODE,
                    EvidenceSource::Hook,
                    Event::SessionEnded(SessionEnded::default()),
                )
                .with_session(external),
                command_event("s-1"),
            ])
            .expect("insert");

        let status: String = store
            .connection()
            .query_row("SELECT status FROM sessions", [], |row| row.get(0))
            .expect("query");
        assert_eq!(status, "ended");
    }

    #[test]
    fn session_end_closes_the_session_and_records_duration() {
        let mut store = Store::open_in_memory().expect("schema");
        let external = ExternalSessionId::from("s-1".to_owned());

        let started = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::SessionStarted(SessionStarted::default()),
        )
        .with_session(external.clone())
        .at(Timestamp::from_micros(1_000_000));

        let ended = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::SessionEnded(SessionEnded::default()),
        )
        .with_session(external)
        .at(Timestamp::from_micros(4_000_000));

        store.insert_events(&[started, ended]).expect("insert");

        let (status, duration): (String, Option<i64>) = store
            .connection()
            .query_row("SELECT status, duration_ms FROM sessions", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("session row");

        assert_eq!(status, "ended");
        assert_eq!(duration, Some(3_000));
    }

    #[test]
    fn an_out_of_order_event_does_not_move_the_session_start_forward() {
        let mut store = Store::open_in_memory().expect("schema");
        let external = ExternalSessionId::from("s-1".to_owned());

        let late = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::Command(CommandEvent {
                command: "ls".into(),
                description: None,
            }),
        )
        .with_session(external.clone())
        .at(Timestamp::from_micros(9_000_000));

        let early = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::SessionStarted(SessionStarted::default()),
        )
        .with_session(external)
        .at(Timestamp::from_micros(1_000_000));

        store.insert_events(&[late, early]).expect("insert");

        let started: i64 = store
            .connection()
            .query_row("SELECT started_at_us FROM sessions", [], |row| row.get(0))
            .expect("session row");
        assert_eq!(started, 1_000_000);
    }
}

#[cfg(test)]
mod file_backed_tests {
    use super::*;

    #[test]
    fn opening_a_fresh_file_applies_every_migration() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("events.db");

        let store = Store::open(&path).expect("first open should migrate cleanly");
        drop(store);

        let store = Store::open(&path).expect("re-opening must not re-apply");
        drop(store);
    }
}
