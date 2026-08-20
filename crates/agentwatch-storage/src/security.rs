//! The notable-access listing.

use rusqlite::params;

use crate::store::{Store, StoreError};

/// One access worth looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notable {
    /// When it happened.
    pub timestamp_us: i64,
    /// `normal`, `sensitive`, or `highly_sensitive`.
    pub sensitivity: String,
    /// What kind of access: `read`, `write`, or `command`.
    pub kind: String,
    /// The path involved.
    pub path: String,
    /// How AgentWatch knows: `hook` for tool reports, `derived` for inference.
    pub evidence: String,
    /// Repository or directory it happened in.
    pub project: Option<String>,
}

/// File events and command references are unioned but kept distinguishable by
/// `evidence`: a tool-reported read is observation, a path scraped out of a
/// command line is inference, and the difference matters when someone acts on
/// this list.
impl Store {
    /// Lists everything above `normal` in a range, most serious first.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried.
    pub fn notable_access(
        &self,
        from_us: i64,
        to_us: i64,
        limit: u32,
    ) -> Result<Vec<Notable>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT f.timestamp_us, f.sensitivity, f.operation, f.path, 'hook',
                COALESCE(r.root, p.path)
           FROM file_events f
           LEFT JOIN projects p     ON p.id = f.project_id
           LEFT JOIN repositories r ON r.id = p.repository_id
          WHERE f.sensitivity != 'normal'
            AND f.timestamp_us >= ?1 AND f.timestamp_us < ?2
         UNION ALL
         SELECT c.timestamp_us, c.sensitivity, 'command', c.path, 'derived',
                COALESCE(r.root, p.path)
           FROM command_path_references c
           LEFT JOIN projects p     ON p.id = c.project_id
           LEFT JOIN repositories r ON r.id = p.repository_id
          WHERE c.sensitivity != 'normal'
            AND c.timestamp_us >= ?1 AND c.timestamp_us < ?2
          ORDER BY 2 DESC, 1 DESC
          LIMIT ?3",
        )?;

        let rows = statement.query_map(params![from_us, to_us, limit], |row| {
            Ok(Notable {
                timestamp_us: row.get(0)?,
                sensitivity: row.get(1)?,
                kind: row.get(2)?,
                path: row.get(3)?,
                evidence: row.get(4)?,
                project: row.get(5)?,
            })
        })?;

        let mut found = Vec::new();
        for row in rows {
            found.push(row?);
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{AgentEvent, CommandEvent, Event, EvidenceSource, FileEvent};
    use agentwatch_types::{AgentId, ExternalSessionId};

    use super::*;

    fn seeded() -> Store {
        let mut store = Store::open_in_memory().expect("schema");
        let event = |event: Event| {
            AgentEvent::observed(AgentId::CLAUDE_CODE, EvidenceSource::Hook, event)
                .with_session(ExternalSessionId::from("s-1".to_owned()))
                .with_project_path("/work/acme".to_owned())
        };

        store
            .insert_events(&[
                event(Event::FileRead(FileEvent {
                    path: "/Users/dev/.aws/credentials".into(),
                    tool: "Read".into(),
                })),
                event(Event::FileRead(FileEvent {
                    path: "/work/acme/src/main.rs".into(),
                    tool: "Read".into(),
                })),
                event(Event::Command(CommandEvent {
                    command: "cat .env".into(),
                    description: None,
                })),
            ])
            .expect("insert");
        store
    }

    #[test]
    fn lists_only_notable_access() {
        let found = seeded().notable_access(0, i64::MAX, 50).expect("query");
        assert_eq!(found.len(), 2, "the ordinary source file should not appear");
    }

    #[test]
    fn orders_most_serious_first() {
        let found = seeded().notable_access(0, i64::MAX, 50).expect("query");
        assert_eq!(found[0].sensitivity, "sensitive");
        assert!(
            found
                .iter()
                .any(|row| row.sensitivity == "highly_sensitive")
        );
    }

    #[test]
    fn distinguishes_observation_from_inference() {
        let found = seeded().notable_access(0, i64::MAX, 50).expect("query");

        let credential = found
            .iter()
            .find(|row| row.path.contains("credentials"))
            .expect("found");
        let dotenv = found.iter().find(|row| row.path == ".env").expect("found");

        assert_eq!(credential.evidence, "hook", "a tool reported this one");
        assert_eq!(
            dotenv.evidence, "derived",
            "this one was scraped from a command line"
        );
    }
}
