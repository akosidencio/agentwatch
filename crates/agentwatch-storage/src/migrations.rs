//! Schema migrations.
//!
//! Append-only: never edit a shipped migration, add a new one. The applied
//! version is recorded in `schema_migrations` and each step runs in its own
//! transaction.

use rusqlite::Connection;

/// One migration: a version number and the SQL that moves the schema to it.
pub(crate) struct Migration {
    /// Monotonically increasing version.
    pub(crate) version: i64,
    /// Human-readable name, recorded for debugging.
    pub(crate) name: &'static str,
    /// The statements to run.
    pub(crate) sql: &'static str,
}

/// Every migration, in order.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_event_spine",
        sql: r"
        CREATE TABLE agents (
            id            TEXT PRIMARY KEY,
            first_seen_us INTEGER NOT NULL,
            last_seen_us  INTEGER NOT NULL
        ) STRICT;

        CREATE TABLE projects (
            id            TEXT PRIMARY KEY,
            path          TEXT NOT NULL UNIQUE,
            first_seen_us INTEGER NOT NULL,
            last_seen_us  INTEGER NOT NULL
        ) STRICT;

        CREATE TABLE sessions (
            id                  TEXT PRIMARY KEY,
            agent_id            TEXT NOT NULL,
            external_session_id TEXT,
            project_id          TEXT,
            started_at_us       INTEGER,
            ended_at_us         INTEGER,
            duration_ms         INTEGER,
            status              TEXT NOT NULL,
            transcript_path     TEXT,
            created_at_us       INTEGER NOT NULL
        ) STRICT;

        CREATE INDEX sessions_agent_started
            ON sessions (agent_id, started_at_us);
        CREATE INDEX sessions_project_started
            ON sessions (project_id, started_at_us);

        CREATE TABLE events (
            id            TEXT PRIMARY KEY,
            timestamp_us  INTEGER NOT NULL,
            agent_id      TEXT NOT NULL,
            session_id    TEXT,
            project_id    TEXT,
            kind          TEXT NOT NULL,
            evidence      TEXT NOT NULL,
            confidence    REAL NOT NULL,
            payload       TEXT NOT NULL,
            created_at_us INTEGER NOT NULL
        ) STRICT;

        CREATE INDEX events_timestamp        ON events (timestamp_us);
        CREATE INDEX events_agent_timestamp  ON events (agent_id, timestamp_us);
        CREATE INDEX events_project_timestamp ON events (project_id, timestamp_us);
        CREATE INDEX events_session_timestamp ON events (session_id, timestamp_us);
        CREATE INDEX events_kind_timestamp   ON events (kind, timestamp_us);
    ",
    },
    Migration {
        version: 2,
        name: "typed_tables_and_token_usage",
        sql: r"
        ALTER TABLE sessions ADD COLUMN git_branch TEXT;
        ALTER TABLE sessions ADD COLUMN reconciled_at_us INTEGER;

        CREATE TABLE token_usage (
            id                          TEXT PRIMARY KEY,
            timestamp_us                INTEGER NOT NULL,
            agent_id                    TEXT NOT NULL,
            session_id                  TEXT,
            project_id                  TEXT,
            provider                    TEXT NOT NULL,
            model                       TEXT,
            request_id                  TEXT NOT NULL,
            input_tokens                INTEGER NOT NULL,
            cache_creation_input_tokens INTEGER NOT NULL,
            cache_read_input_tokens     INTEGER NOT NULL,
            output_tokens               INTEGER NOT NULL,
            is_subagent                 INTEGER NOT NULL,
            provider_usage              TEXT NOT NULL,
            created_at_us               INTEGER NOT NULL
        ) STRICT;

        -- The idempotency guarantee. One row per model response, so the
        -- reconcile pass can run as many times as it likes over the same
        -- transcript without inflating a single total.
        CREATE UNIQUE INDEX token_usage_response
            ON token_usage (agent_id, request_id);

        CREATE INDEX token_usage_timestamp         ON token_usage (timestamp_us);
        CREATE INDEX token_usage_session           ON token_usage (session_id);
        CREATE INDEX token_usage_project_timestamp ON token_usage (project_id, timestamp_us);
        CREATE INDEX token_usage_model_timestamp   ON token_usage (model, timestamp_us);

        CREATE TABLE file_events (
            id            TEXT PRIMARY KEY,
            timestamp_us  INTEGER NOT NULL,
            agent_id      TEXT NOT NULL,
            session_id    TEXT,
            project_id    TEXT,
            operation     TEXT NOT NULL,
            path          TEXT NOT NULL,
            tool          TEXT NOT NULL,
            created_at_us INTEGER NOT NULL
        ) STRICT;

        CREATE INDEX file_events_timestamp         ON file_events (timestamp_us);
        CREATE INDEX file_events_path              ON file_events (path);
        CREATE INDEX file_events_project_timestamp ON file_events (project_id, timestamp_us);

        CREATE TABLE command_events (
            id            TEXT PRIMARY KEY,
            timestamp_us  INTEGER NOT NULL,
            agent_id      TEXT NOT NULL,
            session_id    TEXT,
            project_id    TEXT,
            command       TEXT NOT NULL,
            description   TEXT,
            created_at_us INTEGER NOT NULL
        ) STRICT;

        CREATE INDEX command_events_timestamp         ON command_events (timestamp_us);
        CREATE INDEX command_events_project_timestamp ON command_events (project_id, timestamp_us);

        CREATE TABLE mcp_events (
            id            TEXT PRIMARY KEY,
            timestamp_us  INTEGER NOT NULL,
            agent_id      TEXT NOT NULL,
            session_id    TEXT,
            project_id    TEXT,
            server        TEXT NOT NULL,
            tool          TEXT NOT NULL,
            created_at_us INTEGER NOT NULL
        ) STRICT;

        CREATE INDEX mcp_events_timestamp         ON mcp_events (timestamp_us);
        CREATE INDEX mcp_events_server_tool       ON mcp_events (server, tool);
        CREATE INDEX mcp_events_project_timestamp ON mcp_events (project_id, timestamp_us);
    ",
    },
];

/// Applies every migration the database has not seen yet.
///
/// # Errors
///
/// Returns an error if a migration fails; the failing step is rolled back.
pub(crate) fn apply(connection: &mut Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version     INTEGER PRIMARY KEY,
            name        TEXT NOT NULL,
            applied_at  TEXT NOT NULL DEFAULT (datetime('now'))
        ) STRICT;",
    )?;

    let current: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;

    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        let transaction = connection.transaction()?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![migration.version, migration.name],
        )?;
        transaction.commit()?;
        tracing::info!(
            version = migration.version,
            name = migration.name,
            "applied migration"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_unique_and_ascending() {
        let mut previous = 0;
        for migration in MIGRATIONS {
            assert!(
                migration.version > previous,
                "migration versions must ascend"
            );
            previous = migration.version;
        }
    }

    #[test]
    fn applying_twice_is_a_no_op() {
        let mut connection = Connection::open_in_memory().expect("in-memory database");
        apply(&mut connection).expect("first apply");
        apply(&mut connection).expect("second apply");

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(count, MIGRATIONS.len() as i64);
    }
}
