//! Custom command-redaction configuration and retroactive database scrubbing.

use std::path::Path;

use agentwatch_events::CommandRedactor;
use rusqlite::{TransactionBehavior, params};

use crate::{Store, StoreError};

/// Result of checking or applying a retroactive command scrub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubReport {
    /// Command records examined.
    pub scanned: usize,
    /// Command records whose projection or raw event payload needed changes.
    pub changed: usize,
    /// Custom expressions active during the scrub.
    pub custom_patterns: usize,
    /// Whether changes were reported without being written.
    pub dry_run: bool,
}

pub(crate) fn read_patterns(path: &Path) -> Result<String, StoreError> {
    match std::fs::read_to_string(path) {
        Ok(source) => Ok(source),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(StoreError::RedactionConfigIo {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn compile_patterns(path: &Path, source: &str) -> Result<CommandRedactor, StoreError> {
    let mut redactor = CommandRedactor::new();
    for (index, raw) in source.lines().enumerate() {
        let pattern = raw.trim();
        if pattern.is_empty() || pattern.starts_with('#') {
            continue;
        }
        redactor
            .add_pattern(pattern)
            .map_err(|source| StoreError::RedactionPattern {
                path: path.to_path_buf(),
                line: index + 1,
                source,
            })?;
    }
    Ok(redactor)
}

impl Store {
    /// Re-applies the current redaction policy to every stored command.
    ///
    /// The typed command projection and the raw event JSON are updated in the
    /// same transaction so exports, receipts, and activity queries cannot
    /// disagree. Re-running the operation is safe and reports zero changes.
    ///
    /// # Errors
    ///
    /// Returns an error if the custom rules are invalid, an event payload is
    /// corrupt, or SQLite cannot finish the transaction. No rows are changed
    /// if any command fails.
    pub fn scrub_commands(&mut self, dry_run: bool) -> Result<ScrubReport, StoreError> {
        self.refresh_redaction_patterns()?;
        let redactor = self.command_redactor.clone();
        // Reserve the single SQLite writer up front. Once this snapshot is
        // read, no collector batch can slip an older command in behind it.
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let rows = {
            let mut statement = transaction.prepare(
                "SELECT command_events.id, command_events.command, events.payload
                   FROM command_events
                   JOIN events ON events.id = command_events.id
                  ORDER BY command_events.id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut changed = 0;
        for (id, stored_command, payload) in &rows {
            let safe_command = redactor.redact(stored_command);
            let mut event: serde_json::Value = serde_json::from_str(payload)?;
            if event.get("kind").and_then(serde_json::Value::as_str) != Some("command") {
                return Err(StoreError::CommandPayloadMismatch { id: id.clone() });
            }
            let Some(payload_command) = event
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
            else {
                return Err(StoreError::CommandPayloadMismatch { id: id.clone() });
            };
            let safe_payload_command = redactor.redact(&payload_command);

            let command_changed = safe_command != *stored_command;
            let payload_changed = safe_payload_command != payload_command;
            if !command_changed && !payload_changed {
                continue;
            }
            changed += 1;

            if dry_run {
                continue;
            }
            if command_changed {
                transaction.execute(
                    "UPDATE command_events SET command = ?2 WHERE id = ?1",
                    params![id, safe_command],
                )?;
            }
            if payload_changed {
                event["command"] = serde_json::Value::String(safe_payload_command);
                transaction.execute(
                    "UPDATE events SET payload = ?2 WHERE id = ?1",
                    params![id, serde_json::to_string(&event)?],
                )?;
            }
        }

        let report = ScrubReport {
            scanned: rows.len(),
            changed,
            custom_patterns: redactor.custom_pattern_count(),
            dry_run,
        };
        if dry_run {
            transaction.rollback()?;
        } else {
            transaction.commit()?;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{AgentEvent, CommandEvent, Event, EvidenceSource};
    use agentwatch_types::AgentId;

    use super::*;

    #[test]
    fn pattern_files_ignore_comments_and_report_the_source_line() {
        let path = Path::new("redaction-patterns.txt");
        let redactor =
            compile_patterns(path, "# private formats\n\nACME-[0-9]+\n").expect("valid config");
        assert_eq!(redactor.custom_pattern_count(), 1);

        let error =
            compile_patterns(path, "# comment\nvalid\n(unterminated").expect_err("invalid config");
        assert!(matches!(
            error,
            StoreError::RedactionPattern { line: 3, .. }
        ));
    }

    fn insert_command(store: &mut Store) -> String {
        let event = AgentEvent::observed(
            AgentId::CODEX,
            EvidenceSource::Transcript,
            Event::Command(CommandEvent {
                command: "curl https://example.test/health".to_owned(),
                description: None,
            }),
        );
        let id = event.id.to_string();
        store.insert_events(&[event]).expect("insert command");
        id
    }

    fn expose_legacy_secret(store: &Store, id: &str, secret: &str) {
        let event = Event::Command(CommandEvent {
            command: secret.to_owned(),
            description: None,
        });
        store
            .connection()
            .execute(
                "UPDATE command_events SET command = ?2 WHERE id = ?1",
                params![id, secret],
            )
            .expect("seed projection");
        store
            .connection()
            .execute(
                "UPDATE events SET payload = ?2 WHERE id = ?1",
                params![id, serde_json::to_string(&event).expect("payload")],
            )
            .expect("seed payload");
    }

    #[test]
    fn scrub_updates_projection_and_raw_payload_atomically_and_idempotently() {
        let mut store = Store::open_in_memory().expect("schema");
        let id = insert_command(&mut store);
        let secret = "curl -H 'X: Bearer abcdefghijklmnop' postgres://me:hunter2@db/app";
        expose_legacy_secret(&store, &id, secret);

        let report = store.scrub_commands(false).expect("scrub");
        assert_eq!(report.scanned, 1);
        assert_eq!(report.changed, 1);

        let (command, payload): (String, String) = store
            .connection()
            .query_row(
                "SELECT command_events.command, events.payload
                   FROM command_events JOIN events USING (id)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("stored command");
        assert!(!command.contains("abcdefghijklmnop"));
        assert!(!command.contains("hunter2"));
        assert!(!payload.contains("abcdefghijklmnop"));
        assert!(!payload.contains("hunter2"));
        assert_eq!(store.scrub_commands(false).expect("again").changed, 0);
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let mut store = Store::open_in_memory().expect("schema");
        let id = insert_command(&mut store);
        let secret = "TOKEN=do-not-store cargo test";
        expose_legacy_secret(&store, &id, secret);

        let report = store.scrub_commands(true).expect("dry run");
        assert_eq!(report.changed, 1);
        let command: String = store
            .connection()
            .query_row("SELECT command FROM command_events", [], |row| row.get(0))
            .expect("command");
        assert_eq!(command, secret);
    }
}
