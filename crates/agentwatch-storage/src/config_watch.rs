//! Noticing when our own hook configuration changes.

use rusqlite::params;

use crate::store::{Store, StoreError};

/// What a fingerprint check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigCheck {
    /// This file had not been seen before. Recorded, not reported as a change.
    FirstSight,
    /// The fingerprint matches what we last saw.
    Unchanged,
    /// The configuration changed since we last looked.
    Changed {
        /// What it was.
        previous: String,
        /// Whether our hooks were present before.
        previously_present: bool,
    },
}

impl Store {
    /// Records the current fingerprint of a settings file and reports movement.
    ///
    /// First sight is deliberately not a change: installing AgentWatch would
    /// otherwise announce that AgentWatch's configuration had been tampered
    /// with, which trains people to ignore the one event that matters.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be written.
    pub fn check_config(
        &mut self,
        path: &str,
        fingerprint: &str,
        hooks_present: bool,
    ) -> Result<ConfigCheck, StoreError> {
        // A genuine query failure is raised, not folded into "never seen
        // before": reporting a broken database as first sight would silently
        // swallow the one change this exists to announce.
        let previous: Option<(String, bool)> = match self.connection().query_row(
            "SELECT fingerprint, hooks_present FROM config_watch WHERE path = ?1",
            params![path],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
        ) {
            Ok(row) => Some(row),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(error) => return Err(error.into()),
        };

        let now = agentwatch_types::Timestamp::now().as_micros();
        self.connection().execute(
            "INSERT INTO config_watch (path, fingerprint, hooks_present, updated_at_us)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                fingerprint   = excluded.fingerprint,
                hooks_present = excluded.hooks_present,
                updated_at_us = excluded.updated_at_us",
            params![path, fingerprint, i64::from(hooks_present), now],
        )?;

        Ok(match previous {
            None => ConfigCheck::FirstSight,
            Some((seen, _)) if seen == fingerprint => ConfigCheck::Unchanged,
            Some((seen, present)) => ConfigCheck::Changed {
                previous: seen,
                previously_present: present,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_look_at_a_file_is_not_a_change() {
        let mut store = Store::open_in_memory().expect("schema");
        let check = store.check_config("/s.json", "abc", true).expect("check");
        assert_eq!(check, ConfigCheck::FirstSight);
    }

    #[test]
    fn an_unchanged_file_reports_no_movement() {
        let mut store = Store::open_in_memory().expect("schema");
        store.check_config("/s.json", "abc", true).expect("check");
        assert_eq!(
            store.check_config("/s.json", "abc", true).expect("check"),
            ConfigCheck::Unchanged
        );
    }

    #[test]
    fn removing_our_hooks_is_reported_as_a_change() {
        let mut store = Store::open_in_memory().expect("schema");
        store
            .check_config("/s.json", "with-hooks", true)
            .expect("check");

        let check = store
            .check_config("/s.json", "without-hooks", false)
            .expect("check");
        assert_eq!(
            check,
            ConfigCheck::Changed {
                previous: "with-hooks".to_owned(),
                previously_present: true
            }
        );
    }

    #[test]
    fn a_change_is_reported_once_not_on_every_check() {
        let mut store = Store::open_in_memory().expect("schema");
        store.check_config("/s.json", "a", true).expect("check");
        store.check_config("/s.json", "b", false).expect("check");

        assert_eq!(
            store.check_config("/s.json", "b", false).expect("check"),
            ConfigCheck::Unchanged,
            "a persistent change must not re-alert forever"
        );
    }

    #[test]
    fn files_are_tracked_independently() {
        let mut store = Store::open_in_memory().expect("schema");
        store.check_config("/a.json", "x", true).expect("check");
        assert_eq!(
            store.check_config("/b.json", "y", true).expect("check"),
            ConfigCheck::FirstSight
        );
    }
}
