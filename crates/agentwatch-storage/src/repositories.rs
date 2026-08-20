//! Rolling directories up to the repositories that contain them.

use agentwatch_types::{ProjectId, RepositoryResolver, Timestamp, display_name};
use rusqlite::params;

use crate::store::{Store, StoreError};

/// What a backfill pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Project directories examined.
    pub projects: u64,
    /// Distinct repositories they resolved to.
    pub repositories: u64,
    /// Projects that are not inside any repository, or whose directory is gone.
    pub unresolved: u64,
}

impl Store {
    /// Resolves every project directory to its repository and links the rows.
    ///
    /// Idempotent and cheap to repeat: projects already carrying a repository
    /// are skipped, so a steady-state run examines only newly seen directories.
    ///
    /// Run after ingestion rather than during it. Resolution touches the
    /// filesystem, and the write path is not where a stat storm belongs.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be read or written.
    pub fn backfill_repositories(
        &mut self,
        resolver: &mut RepositoryResolver,
    ) -> Result<BackfillReport, StoreError> {
        let unlinked = self.unlinked_projects()?;
        let mut report = BackfillReport::default();

        // The steady state is nothing to do. Returning before opening a
        // transaction is what makes this cheap enough to call after every write
        // batch, so a newly seen directory rolls up in milliseconds rather than
        // waiting for the next sweep.
        if unlinked.is_empty() {
            return Ok(report);
        }

        let now = Timestamp::now().as_micros();
        let transaction = self.connection.transaction()?;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (project_id, directory) in &unlinked {
            report.projects += 1;

            let Some(root) = resolver.resolve(directory) else {
                report.unresolved += 1;
                continue;
            };

            let repository_id = ProjectId::from_path(&root).to_string();
            if seen.insert(repository_id.clone()) {
                report.repositories += 1;
            }

            transaction.execute(
                "INSERT INTO repositories (id, root, name, first_seen_us, last_seen_us)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(id) DO UPDATE SET last_seen_us = MAX(last_seen_us, excluded.last_seen_us)",
                params![repository_id, root, display_name(&root), now],
            )?;

            for table in ["projects", "sessions", "events", "token_usage"] {
                let column = if table == "projects" {
                    "id"
                } else {
                    "project_id"
                };
                transaction.execute(
                    &format!(
                        "UPDATE {table} SET repository_id = ?2
                          WHERE {column} = ?1 AND repository_id IS NULL"
                    ),
                    params![project_id, repository_id],
                )?;
            }
        }

        transaction.commit()?;
        Ok(report)
    }

    /// Project directories that have not been resolved yet.
    fn unlinked_projects(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut statement = self
            .connection()
            .prepare("SELECT id, path FROM projects WHERE repository_id IS NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;

        let mut projects = Vec::new();
        for row in rows {
            projects.push(row?);
        }
        Ok(projects)
    }
}

#[cfg(test)]
mod tests {
    use agentwatch_events::{AgentEvent, CommandEvent, Event, EvidenceSource};
    use agentwatch_types::{AgentId, ExternalSessionId};

    use super::*;

    /// A repository root with two nested directories inside it.
    fn repository() -> (tempfile::TempDir, String, String, String) {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("acme-api");
        let core = root.join("packages").join("core");
        let web = root.join("apps").join("web");
        std::fs::create_dir_all(&core).expect("create");
        std::fs::create_dir_all(&web).expect("create");
        std::fs::create_dir_all(root.join(".git")).expect("marker");

        (
            directory,
            root.to_string_lossy().into_owned(),
            core.to_string_lossy().into_owned(),
            web.to_string_lossy().into_owned(),
        )
    }

    fn event_in(directory: &str, session: &str) -> AgentEvent {
        AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Hook,
            Event::Command(CommandEvent {
                command: "cargo test".into(),
                description: None,
            }),
        )
        .with_session(ExternalSessionId::from(session.to_owned()))
        .with_project_path(directory.to_owned())
    }

    #[test]
    fn three_directories_in_one_repository_become_one_repository() {
        let (_guard, root, core, web) = repository();
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[
                event_in(&root, "s-1"),
                event_in(&core, "s-2"),
                event_in(&web, "s-3"),
            ])
            .expect("insert");

        let report = store
            .backfill_repositories(&mut RepositoryResolver::new())
            .expect("backfill");

        assert_eq!(report.projects, 3);
        assert_eq!(report.repositories, 1, "one repository, not three");
        assert_eq!(report.unresolved, 0);
    }

    #[test]
    fn backfill_links_rows_that_were_stored_before_it_existed() {
        let (_guard, root, core, _web) = repository();
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[event_in(&root, "s-1"), event_in(&core, "s-2")])
            .expect("insert");

        store
            .backfill_repositories(&mut RepositoryResolver::new())
            .expect("backfill");

        let distinct: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(DISTINCT repository_id) FROM events WHERE repository_id IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(
            distinct, 1,
            "both events should point at the same repository"
        );
    }

    #[test]
    fn backfilling_an_empty_database_does_no_work() {
        let mut store = Store::open_in_memory().expect("schema");
        let report = store
            .backfill_repositories(&mut RepositoryResolver::new())
            .expect("backfill");
        assert_eq!(report, BackfillReport::default());
    }

    #[test]
    fn a_second_backfill_examines_nothing() {
        let (_guard, root, _core, _web) = repository();
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[event_in(&root, "s-1")])
            .expect("insert");

        let mut resolver = RepositoryResolver::new();
        store.backfill_repositories(&mut resolver).expect("first");
        let second = store.backfill_repositories(&mut resolver).expect("second");

        assert_eq!(
            second.projects, 0,
            "already linked projects should be skipped"
        );
    }

    #[test]
    fn a_directory_outside_a_repository_is_counted_not_dropped() {
        let directory = tempfile::tempdir().expect("temp dir");
        let plain = directory
            .path()
            .join("loose")
            .to_string_lossy()
            .into_owned();
        std::fs::create_dir_all(&plain).expect("create");

        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[event_in(&plain, "s-1")])
            .expect("insert");

        let report = store
            .backfill_repositories(&mut RepositoryResolver::new())
            .expect("backfill");
        assert_eq!(report.unresolved, 1);
        assert_eq!(report.repositories, 0);
    }

    #[test]
    fn the_repository_is_named_after_its_directory() {
        let (_guard, root, _core, _web) = repository();
        let mut store = Store::open_in_memory().expect("schema");
        store
            .insert_events(&[event_in(&root, "s-1")])
            .expect("insert");
        store
            .backfill_repositories(&mut RepositoryResolver::new())
            .expect("backfill");

        let name: String = store
            .connection()
            .query_row("SELECT name FROM repositories", [], |row| row.get(0))
            .expect("query");
        assert_eq!(name, "acme-api");
    }
}
