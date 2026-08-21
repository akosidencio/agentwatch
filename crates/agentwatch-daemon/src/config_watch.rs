//! Watching our own hook configuration for changes.
//!
//! A monitor whose collection can be disabled without leaving a trace is not
//! much of a monitor. This does not prevent that — nothing running as the same
//! user could — but it makes the gap visible afterwards, so a quiet stretch in
//! the timeline can be read as "collection stopped here" rather than "the agent
//! was idle".

use std::path::{Path, PathBuf};

use agentwatch_events::{AgentEvent, ConfigChangedEvent, Event, EvidenceSource};
use agentwatch_storage::{ConfigCheck, Store};
use agentwatch_types::AgentId;
use sha2::{Digest, Sha256};

/// Marker identifying a hook entry as ours.
const MARKER: &str = "agentwatch-hook";

/// Settings files worth watching.
///
/// Only Claude Code's for now, matching the only agent this version observes.
pub(crate) fn watched_paths() -> Vec<PathBuf> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR").map_or_else(
        || {
            let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
            home.join(".claude")
        },
        PathBuf::from,
    );

    vec![base.join("settings.json"), base.join("settings.local.json")]
}

/// Fingerprints the hook-relevant part of a settings file.
///
/// Deliberately narrow: the whole file changes whenever the user edits an
/// unrelated permission, and alerting on that would bury the signal. Only the
/// `hooks` section is hashed.
///
/// Returns `None` for a file that does not exist or cannot be parsed — an
/// absent file is not a change, and a malformed one is the user's business.
fn fingerprint(path: &Path) -> Option<(String, bool)> {
    let text = std::fs::read_to_string(path).ok()?;
    let settings: serde_json::Value = serde_json::from_str(&text).ok()?;

    let hooks = settings
        .get("hooks")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    // Serialized with sorted keys via canonical re-parse so that a formatting
    // change alone does not read as a configuration change.
    let canonical = serde_json::to_string(&hooks).ok()?;
    let present = canonical.contains(MARKER);

    let digest = <Sha256 as Digest>::digest(canonical.as_bytes());
    Some((format!("{digest:x}"), present))
}

/// Checks every watched file and records an event for anything that moved.
///
/// Errors are logged rather than propagated: a configuration check failing must
/// never take the daemon down with it.
pub(crate) fn sweep(store: &mut Store) {
    for path in watched_paths() {
        let Some((digest, present)) = fingerprint(&path) else {
            continue;
        };
        let label = path.display().to_string();

        let check = match store.check_config(&label, &digest, present) {
            Ok(check) => check,
            Err(error) => {
                tracing::error!(?error, path = %label, "config check failed");
                continue;
            }
        };

        let ConfigCheck::Changed {
            previous,
            previously_present,
        } = check
        else {
            continue;
        };

        if previously_present && !present {
            tracing::warn!(path = %label, "our hooks were removed — collection has stopped");
        } else {
            tracing::info!(path = %label, "hook configuration changed");
        }

        let event = AgentEvent::observed(
            AgentId::CLAUDE_CODE,
            EvidenceSource::Derived,
            Event::ConfigChanged(ConfigChangedEvent {
                path: label.clone(),
                hooks_present: present,
                fingerprint: digest,
                previous_fingerprint: Some(previous),
            }),
        );

        if let Err(error) = store.insert_events(&[event]) {
            tracing::error!(?error, path = %label, "recording config change failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(directory: &Path, contents: &str) -> PathBuf {
        let path = directory.join("settings.json");
        std::fs::write(&path, contents).expect("write");
        path
    }

    #[test]
    fn a_missing_file_has_no_fingerprint() {
        assert!(fingerprint(Path::new("/nonexistent/settings.json")).is_none());
    }

    #[test]
    fn a_malformed_file_has_no_fingerprint() {
        let directory = tempfile::tempdir().expect("temp dir");
        assert!(fingerprint(&write(directory.path(), "{ not json")).is_none());
    }

    #[test]
    fn our_hooks_are_detected() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = write(
            directory.path(),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"command":"/x/agentwatch-hook"}]}]}}"#,
        );
        let (_, present) = fingerprint(&path).expect("fingerprint");
        assert!(present);
    }

    #[test]
    fn an_unrelated_setting_does_not_change_the_fingerprint() {
        let directory = tempfile::tempdir().expect("temp dir");
        let hooks = r#""hooks":{"SessionStart":[{"hooks":[{"command":"/x/agentwatch-hook"}]}]}"#;

        let first = fingerprint(&write(directory.path(), &format!("{{{hooks}}}"))).expect("one");
        let second = fingerprint(&write(
            directory.path(),
            &format!(r#"{{"model":"opus",{hooks}}}"#),
        ))
        .expect("two");

        assert_eq!(
            first.0, second.0,
            "only the hooks section should be fingerprinted"
        );
    }

    #[test]
    fn removing_our_hook_changes_the_fingerprint_and_clears_the_flag() {
        let directory = tempfile::tempdir().expect("temp dir");
        let with = fingerprint(&write(
            directory.path(),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"command":"/x/agentwatch-hook"}]}]}}"#,
        ))
        .expect("with");
        let without = fingerprint(&write(directory.path(), r#"{"hooks":{}}"#)).expect("without");

        assert_ne!(with.0, without.0);
        assert!(with.1 && !without.1);
    }

    #[test]
    fn a_sweep_records_an_event_only_on_the_second_look() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = write(
            directory.path(),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"command":"/x/agentwatch-hook"}]}]}}"#,
        );
        let mut store = Store::open_in_memory().expect("schema");
        let label = path.display().to_string();

        let (first, present) = fingerprint(&path).expect("fingerprint");
        assert_eq!(
            store.check_config(&label, &first, present).expect("check"),
            ConfigCheck::FirstSight,
            "installing must not announce itself as tampering"
        );

        std::fs::write(&path, r#"{"hooks":{}}"#).expect("rewrite");
        let (second, present) = fingerprint(&path).expect("fingerprint");
        assert!(matches!(
            store.check_config(&label, &second, present).expect("check"),
            ConfigCheck::Changed {
                previously_present: true,
                ..
            }
        ));
    }
}
