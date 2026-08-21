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
/// A file that is missing or unparseable fingerprints as *no hooks*, the same
/// as a file with no `hooks` section. That is the honest reading: Claude Code
/// will not run hooks it cannot load, so deleting or corrupting the settings
/// stops collection exactly as removing the entries would. Treating those cases
/// as "nothing to see" made the most direct way to disable monitoring the one
/// way that left no trace.
///
/// Returns `None` only when the file exists but cannot be read at all — a
/// permissions problem is not evidence about the hooks either way.
fn fingerprint(path: &Path) -> Option<(String, bool)> {
    let hooks = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|settings| settings.get("hooks").cloned())
            .unwrap_or(serde_json::Value::Null),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Value::Null,
        Err(_) => return None,
    };

    let canonical = serde_json::to_string(&sort_keys(&hooks)).ok()?;
    let present = canonical.contains(MARKER);

    let digest = <Sha256 as Digest>::digest(canonical.as_bytes());
    Some((format!("{digest:x}"), present))
}

/// Rebuilds a value with every object's keys in sorted order.
///
/// `serde_json` is built with `preserve_order` here, so its maps keep insertion
/// order and re-serializing preserves whatever order the file happened to use.
/// Without this, any tool that rewrites `settings.json` with its keys in a
/// different order reads as a hook configuration change — and a tamper alarm
/// that fires on formatting is one people learn to ignore.
fn sort_keys(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();

            let mut sorted = serde_json::Map::with_capacity(map.len());
            for key in keys {
                if let Some(nested) = map.get(key) {
                    sorted.insert(key.clone(), sort_keys(nested));
                }
            }
            serde_json::Value::Object(sorted)
        }
        // Arrays keep their order: in a hooks block the order of matcher groups
        // is meaningful, so reordering them really is a change.
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(sort_keys).collect())
        }
        other => other.clone(),
    }
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
    fn a_missing_file_fingerprints_as_having_no_hooks() {
        let (_, present) = fingerprint(Path::new("/nonexistent/settings.json")).expect("absent");
        assert!(!present);
    }

    #[test]
    fn a_file_that_never_existed_matches_one_with_no_hooks_section() {
        let directory = tempfile::tempdir().expect("temp dir");
        let absent = fingerprint(Path::new("/nonexistent/settings.json")).expect("absent");
        let empty = fingerprint(&write(directory.path(), r#"{"model":"opus"}"#)).expect("empty");

        assert_eq!(
            absent.0, empty.0,
            "a file appearing without hooks is not a hook change"
        );
    }

    /// The gap this closes: deleting the settings file is the simplest way to
    /// stop collection, and it used to leave no event behind at all.
    #[test]
    fn deleting_the_settings_file_reads_as_our_hooks_being_removed() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = write(
            directory.path(),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"command":"/x/agentwatch-hook"}]}]}}"#,
        );
        let mut store = Store::open_in_memory().expect("schema");
        let label = path.display().to_string();

        let (first, present) = fingerprint(&path).expect("fingerprint");
        assert!(present);
        store.check_config(&label, &first, present).expect("check");

        std::fs::remove_file(&path).expect("delete");
        let (second, present) = fingerprint(&path).expect("still fingerprintable");

        assert!(!present);
        assert!(matches!(
            store.check_config(&label, &second, present).expect("check"),
            ConfigCheck::Changed {
                previously_present: true,
                ..
            }
        ));
    }

    #[test]
    fn a_malformed_file_reads_as_hooks_that_will_not_load() {
        let directory = tempfile::tempdir().expect("temp dir");
        let (_, present) = fingerprint(&write(directory.path(), "{ not json")).expect("malformed");
        assert!(!present, "the agent cannot run hooks it cannot parse");
    }

    #[test]
    fn reordering_keys_is_not_a_configuration_change() {
        let directory = tempfile::tempdir().expect("temp dir");
        let entry = r#"{"hooks":[{"type":"command","command":"/x/agentwatch-hook"}]}"#;

        let first = fingerprint(&write(
            directory.path(),
            &format!(r#"{{"hooks":{{"SessionStart":[{entry}],"SessionEnd":[{entry}]}}}}"#),
        ))
        .expect("one");
        let second = fingerprint(&write(
            directory.path(),
            &format!(r#"{{"hooks":{{"SessionEnd":[{entry}],"SessionStart":[{entry}]}}}}"#),
        ))
        .expect("two");

        assert_eq!(
            first.0, second.0,
            "a tamper alarm that fires on key order is one people learn to ignore"
        );
    }

    #[test]
    fn reordering_matcher_groups_is_a_change() {
        let directory = tempfile::tempdir().expect("temp dir");
        let ours = r#"{"hooks":[{"type":"command","command":"/x/agentwatch-hook"}]}"#;
        let theirs = r#"{"hooks":[{"type":"command","command":"/other/tool"}]}"#;

        let first = fingerprint(&write(
            directory.path(),
            &format!(r#"{{"hooks":{{"SessionStart":[{ours},{theirs}]}}}}"#),
        ))
        .expect("one");
        let second = fingerprint(&write(
            directory.path(),
            &format!(r#"{{"hooks":{{"SessionStart":[{theirs},{ours}]}}}}"#),
        ))
        .expect("two");

        assert_ne!(first.0, second.0, "group order is meaningful");
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
