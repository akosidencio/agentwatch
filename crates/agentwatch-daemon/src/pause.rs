//! Honouring a user-requested pause.
//!
//! Enforced on the write path rather than at the socket, so a paused daemon
//! still accepts and drains connections. A hook that cannot connect retries and
//! slows the agent down; a hook whose event is accepted and dropped costs
//! nothing and keeps the pause invisible to the thing being monitored.

use agentwatch_events::{AgentEvent, CollectionEvent, Event, EvidenceSource};
use agentwatch_storage::Store;
use agentwatch_types::{AgentId, Paths};

/// Tracks pause state so each transition is recorded exactly once.
pub(crate) struct Pause {
    /// Where the marker lives.
    paths: Paths,
    /// What we last saw, so a steady state writes nothing.
    was_paused: bool,
}

impl Pause {
    /// Starts tracking, taking the current state as the baseline.
    pub(crate) fn new(paths: Paths) -> Self {
        let was_paused = paths.is_paused();
        Self { paths, was_paused }
    }

    /// Returns whether collection is paused, recording any transition.
    ///
    /// The transition events are written even while paused: a gap in the
    /// timeline that does not say why it exists is indistinguishable from an
    /// idle agent, which is the failure this whole tool exists to avoid.
    pub(crate) fn check(&mut self, store: &mut Store) -> bool {
        let paused = self.paths.is_paused();
        if paused == self.was_paused {
            return paused;
        }
        self.was_paused = paused;

        if paused {
            tracing::warn!("collection paused — events will be discarded until resumed");
        } else {
            tracing::info!("collection resumed");
        }

        let event = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Derived,
            Event::Collection(CollectionEvent { paused }),
        );
        if let Err(error) = store.insert_events(&[event]) {
            tracing::error!(?error, "recording the pause transition failed");
        }

        paused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paused_events(store: &Store) -> Vec<String> {
        let mut statement = store
            .connection_for_test()
            .prepare("SELECT kind FROM events ORDER BY timestamp_us, id")
            .expect("prepare");
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .filter_map(Result::ok)
            .filter(|kind| kind.starts_with("collection."))
            .collect()
    }

    #[test]
    fn an_unpaused_daemon_reports_false_and_writes_nothing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let paths = Paths::with_root(directory.path().to_path_buf());
        let mut store = Store::open_in_memory().expect("schema");
        let mut pause = Pause::new(paths);

        assert!(!pause.check(&mut store));
        assert!(paused_events(&store).is_empty());
    }

    #[test]
    fn pausing_is_recorded_once_not_on_every_check() {
        let directory = tempfile::tempdir().expect("temp dir");
        let paths = Paths::with_root(directory.path().to_path_buf());
        let mut store = Store::open_in_memory().expect("schema");
        let mut pause = Pause::new(paths.clone());

        std::fs::write(paths.pause_marker(), "").expect("write");
        assert!(pause.check(&mut store));
        assert!(pause.check(&mut store));
        assert!(pause.check(&mut store));

        assert_eq!(paused_events(&store), ["collection.paused"]);
    }

    #[test]
    fn resuming_is_recorded_too() {
        let directory = tempfile::tempdir().expect("temp dir");
        let paths = Paths::with_root(directory.path().to_path_buf());
        let mut store = Store::open_in_memory().expect("schema");
        let mut pause = Pause::new(paths.clone());

        std::fs::write(paths.pause_marker(), "").expect("write");
        pause.check(&mut store);
        std::fs::remove_file(paths.pause_marker()).expect("remove");
        assert!(!pause.check(&mut store));

        assert_eq!(
            paused_events(&store),
            ["collection.paused", "collection.resumed"]
        );
    }

    #[test]
    fn a_daemon_starting_while_already_paused_does_not_re_announce_it() {
        let directory = tempfile::tempdir().expect("temp dir");
        let paths = Paths::with_root(directory.path().to_path_buf());
        std::fs::write(paths.pause_marker(), "").expect("write");

        let mut store = Store::open_in_memory().expect("schema");
        let mut pause = Pause::new(paths);

        assert!(pause.check(&mut store), "the pause must survive a restart");
        assert!(paused_events(&store).is_empty(), "no transition happened");
    }
}
