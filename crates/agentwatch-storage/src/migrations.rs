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
    Migration {
        version: 3,
        name: "repositories",
        sql: r"
        -- Repositories sit alongside projects rather than replacing them. A
        -- project is the directory the session was started in, which is the
        -- honest answer to 'where'; a repository is what it was working on.
        -- Keeping both means this migration invalidates nothing already stored,
        -- and existing rows can be repaired by backfill rather than re-import.
        CREATE TABLE repositories (
            id            TEXT PRIMARY KEY,
            root          TEXT NOT NULL UNIQUE,
            name          TEXT NOT NULL,
            first_seen_us INTEGER NOT NULL,
            last_seen_us  INTEGER NOT NULL
        ) STRICT;

        ALTER TABLE projects    ADD COLUMN repository_id TEXT;
        ALTER TABLE sessions    ADD COLUMN repository_id TEXT;
        ALTER TABLE events      ADD COLUMN repository_id TEXT;
        ALTER TABLE token_usage ADD COLUMN repository_id TEXT;

        CREATE INDEX projects_repository    ON projects (repository_id);
        CREATE INDEX events_repository      ON events (repository_id, timestamp_us);
        CREATE INDEX token_usage_repository ON token_usage (repository_id, timestamp_us);
    ",
    },
    Migration {
        version: 4,
        name: "sensitivity",
        sql: r"
        -- Classification is computed once at ingest rather than at query time:
        -- the rules are cheap but the answer is what alerts key off, and a rule
        -- change should not silently rewrite what was reported yesterday.
        ALTER TABLE file_events    ADD COLUMN sensitivity TEXT NOT NULL DEFAULT 'normal';
        ALTER TABLE command_events ADD COLUMN sensitivity TEXT NOT NULL DEFAULT 'normal';

        -- Paths a command line referred to. Inference, not observation: kept in
        -- its own table so it can never be mistaken for a file the agent's own
        -- tools reported touching.
        CREATE TABLE command_path_references (
            id            TEXT PRIMARY KEY,
            command_id    TEXT NOT NULL,
            timestamp_us  INTEGER NOT NULL,
            agent_id      TEXT NOT NULL,
            session_id    TEXT,
            project_id    TEXT,
            path          TEXT NOT NULL,
            sensitivity   TEXT NOT NULL,
            created_at_us INTEGER NOT NULL
        ) STRICT;

        CREATE INDEX command_refs_command     ON command_path_references (command_id);
        CREATE INDEX command_refs_timestamp   ON command_path_references (timestamp_us);
        CREATE INDEX command_refs_sensitivity ON command_path_references (sensitivity, timestamp_us);

        CREATE INDEX file_events_sensitivity    ON file_events (sensitivity, timestamp_us);
        CREATE INDEX command_events_sensitivity ON command_events (sensitivity, timestamp_us);
    ",
    },
    Migration {
        version: 5,
        name: "honest_session_status",
        sql: r"
        -- Earlier builds marked every session 'active' on first sight, so a
        -- session discovered by reading a transcript looked like one that was
        -- running. Only an observed start can support that claim; everything
        -- else becomes 'unknown'.
        UPDATE sessions
           SET status = 'unknown'
         WHERE status = 'active'
           AND id NOT IN (
               SELECT session_id FROM events
                WHERE kind = 'session.started' AND session_id IS NOT NULL
           );
    ",
    },
    Migration {
        version: 6,
        name: "config_watch",
        sql: r"
        -- Last known fingerprint of each settings file we watch, so a change
        -- made while the daemon was not running is still noticed at next start
        -- rather than only while we happen to be looking.
        CREATE TABLE config_watch (
            path          TEXT PRIMARY KEY,
            fingerprint   TEXT NOT NULL,
            hooks_present INTEGER NOT NULL,
            updated_at_us INTEGER NOT NULL
        ) STRICT;
    ",
    },
];

/// The schema version this build expects.
pub(crate) fn expected_version() -> i64 {
    MIGRATIONS.last().map_or(0, |migration| migration.version)
}

/// Reads the version a database is currently at.
pub(crate) fn current_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .or(Ok(0))
}

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
    fn the_status_migration_only_demotes_unobserved_sessions() {
        let mut connection = Connection::open_in_memory().expect("in-memory database");

        // Stop before the fix, seed both shapes, then apply the rest.
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY, name TEXT NOT NULL,
                    applied_at TEXT NOT NULL DEFAULT (datetime('now'))) STRICT;",
            )
            .expect("bootstrap");
        for migration in MIGRATIONS.iter().filter(|m| m.version < 5) {
            connection.execute_batch(migration.sql).expect("migrate");
            connection
                .execute(
                    "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
                    rusqlite::params![migration.version, migration.name],
                )
                .expect("record");
        }

        connection
            .execute_batch(
                "INSERT INTO sessions (id, agent_id, status, created_at_us)
                   VALUES ('watched', 'claude-code', 'active', 0),
                          ('imported', 'claude-code', 'active', 0);
                 INSERT INTO events
                   (id, timestamp_us, agent_id, session_id, kind, evidence, confidence, payload, created_at_us)
                   VALUES ('e1', 0, 'claude-code', 'watched', 'session.started', 'hook', 1.0, '{}', 0);",
            )
            .expect("seed");

        apply(&mut connection).expect("apply the rest");

        let watched: String = connection
            .query_row(
                "SELECT status FROM sessions WHERE id = 'watched'",
                [],
                |r| r.get(0),
            )
            .expect("query");
        let imported: String = connection
            .query_row(
                "SELECT status FROM sessions WHERE id = 'imported'",
                [],
                |r| r.get(0),
            )
            .expect("query");

        assert_eq!(watched, "active", "an observed start should survive");
        assert_eq!(
            imported, "unknown",
            "an unobserved session should be demoted"
        );
    }

    #[test]
    fn expected_version_tracks_the_last_migration() {
        assert_eq!(
            expected_version(),
            MIGRATIONS.last().expect("migrations").version
        );
    }

    #[test]
    fn a_migrated_database_reports_the_expected_version() {
        let mut connection = Connection::open_in_memory().expect("in-memory database");
        apply(&mut connection).expect("apply");
        assert_eq!(
            current_version(&connection).expect("version"),
            expected_version()
        );
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
