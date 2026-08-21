//! Installing and removing our hook entries in an agent's settings file.
//!
//! Editing the configuration of the thing you monitor is the most invasive act
//! this tool performs, so the rules here are deliberately strict:
//!
//! - Our entries go in their **own matcher group**, never appended to someone
//!   else's. Removal is then exact, and a co-installed tool's hooks cannot be
//!   damaged by our uninstall.
//! - Planning is pure. [`plan_install`] and [`plan_uninstall`] take settings and
//!   return settings, so the merge logic is tested without a filesystem and the
//!   diff shown to the user is generated from the same value that gets written.
//! - Nothing is written without showing the exact diff and being told yes.

use serde_json::{Map, Value, json};

/// Substring identifying a hook entry as ours.
///
/// Matched against the command string. Anything containing it is ours to
/// remove; anything else is left alone no matter how it got there.
pub(crate) const MARKER: &str = "agentwatch-hook";

/// Hook events we register for.
pub(crate) const HOOK_EVENTS: [&str; 4] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PostToolUse",
];

/// What a plan would change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Change {
    /// Hook entries that would be added.
    pub(crate) added: usize,
    /// Hook entries that would be removed.
    pub(crate) removed: usize,
}

impl Change {
    /// Whether anything would change.
    pub(crate) const fn is_empty(self) -> bool {
        self.added == 0 && self.removed == 0
    }
}

/// Returns settings with our hooks present, leaving everything else untouched.
///
/// Idempotent: an event that already carries our command is not touched, so
/// running this twice produces no second entry.
pub(crate) fn plan_install(settings: &Value, binary: &str) -> (Value, Change) {
    let mut updated = settings.clone();
    let mut change = Change::default();

    // Every failure below leaves the settings untouched and reports no change,
    // which the caller renders as "nothing to install". Refusing to act on a
    // shape we do not understand is the only safe response when the file is
    // someone's live agent configuration.
    let Some(root) = updated.as_object_mut() else {
        return (updated, change);
    };

    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        return (settings.clone(), change);
    };

    for event in HOOK_EVENTS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };

        if groups.iter().any(group_contains_ours) {
            continue;
        }

        // Our own group, never appended to an existing one: an uninstall then
        // removes exactly what we added.
        groups.push(json!({
            "hooks": [ { "type": "command", "command": binary } ]
        }));
        change.added += 1;
    }

    (updated, change)
}

/// Returns settings with our hooks removed and nothing else changed.
///
/// Empty containers left behind by the removal are pruned, so uninstalling
/// from a file we installed into restores it to its original shape rather than
/// leaving `"hooks": {}` behind.
pub(crate) fn plan_uninstall(settings: &Value) -> (Value, Change) {
    let mut updated = settings.clone();
    let mut change = Change::default();

    let Some(root) = updated.as_object_mut() else {
        return (updated, change);
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return (updated, change);
    };

    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };

        for group in groups.iter_mut() {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = entries.len();
            entries.retain(|entry| !is_ours(entry));
            change.removed += before - entries.len();
        }

        // A group whose commands were all ours is ours too. One that still has
        // someone else's commands stays exactly as it is.
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|entries| !entries.is_empty())
        });
    }

    hooks.retain(|_, groups| groups.as_array().is_none_or(|groups| !groups.is_empty()));

    if hooks.is_empty() {
        root.remove("hooks");
    }

    (updated, change)
}

/// Whether a matcher group contains one of our commands.
fn group_contains_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.iter().any(is_ours))
}

/// Whether a single hook entry is ours.
fn is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(MARKER))
}

/// Renders a unified diff between two texts.
///
/// Written out rather than pulled in as a dependency: this is the one thing the
/// user reads before allowing us to rewrite their configuration, and a hundred
/// lines of longest-common-subsequence is a smaller thing to audit than another
/// crate in the tree.
pub(crate) fn unified_diff(before: &str, after: &str) -> String {
    let before: Vec<&str> = before.lines().collect();
    let after: Vec<&str> = after.lines().collect();

    // lengths[i][j] = length of the longest common subsequence of the suffixes.
    let mut lengths = vec![vec![0_usize; after.len() + 1]; before.len() + 1];
    for i in (0..before.len()).rev() {
        for j in (0..after.len()).rev() {
            lengths[i][j] = if before[i] == after[j] {
                lengths[i + 1][j + 1] + 1
            } else {
                lengths[i + 1][j].max(lengths[i][j + 1])
            };
        }
    }

    let mut diff = String::new();
    let (mut i, mut j) = (0, 0);
    while i < before.len() && j < after.len() {
        if before[i] == after[j] {
            diff.push_str(&format!("  {}\n", before[i]));
            i += 1;
            j += 1;
        } else if lengths[i + 1][j] >= lengths[i][j + 1] {
            diff.push_str(&format!("- {}\n", before[i]));
            i += 1;
        } else {
            diff.push_str(&format!("+ {}\n", after[j]));
            j += 1;
        }
    }
    for line in &before[i..] {
        diff.push_str(&format!("- {line}\n"));
    }
    for line in &after[j..] {
        diff.push_str(&format!("+ {line}\n"));
    }

    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings shaped like a real one that already has another tool's hooks.
    fn with_foreign_hooks() -> Value {
        json!({
            "permissions": { "allow": ["Bash"] },
            "model": "opus",
            "hooks": {
                "SessionStart": [
                    { "hooks": [ { "type": "command", "command": "/other/tool-session" } ] }
                ],
                "Stop": [
                    { "hooks": [ { "type": "command", "command": "/other/tool-stop" } ] }
                ]
            }
        })
    }

    #[test]
    fn a_settings_file_with_a_hooks_value_we_do_not_understand_is_left_alone() {
        let odd = json!({ "hooks": "not an object" });
        let (updated, change) = plan_install(&odd, "/bin/agentwatch-hook");

        assert!(change.is_empty(), "we should decline rather than guess");
        assert_eq!(updated, odd, "a file we cannot parse must not be rewritten");
    }

    #[test]
    fn a_hook_event_holding_something_other_than_an_array_is_skipped() {
        let odd = json!({ "hooks": { "SessionStart": "nonsense" } });
        let (updated, change) = plan_install(&odd, "/bin/agentwatch-hook");

        assert_eq!(
            updated["hooks"]["SessionStart"], "nonsense",
            "left as found"
        );
        assert_eq!(
            change.added,
            HOOK_EVENTS.len() - 1,
            "the others still install"
        );
    }

    #[test]
    fn installing_adds_every_hook_event() {
        let (updated, change) = plan_install(&json!({}), "/bin/agentwatch-hook");

        assert_eq!(change.added, HOOK_EVENTS.len());
        let hooks = updated
            .get("hooks")
            .and_then(Value::as_object)
            .expect("hooks");
        for event in HOOK_EVENTS {
            assert!(hooks.contains_key(event), "missing {event}");
        }
    }

    #[test]
    fn installing_twice_changes_nothing_the_second_time() {
        let (once, _) = plan_install(&json!({}), "/bin/agentwatch-hook");
        let (twice, change) = plan_install(&once, "/bin/agentwatch-hook");

        assert!(change.is_empty(), "a second install should be a no-op");
        assert_eq!(once, twice);
    }

    #[test]
    fn installing_preserves_another_tools_hooks() {
        let original = with_foreign_hooks();
        let (updated, _) = plan_install(&original, "/bin/agentwatch-hook");

        let session_start = updated["hooks"]["SessionStart"].as_array().expect("array");
        assert_eq!(session_start.len(), 2, "ours should be a separate group");
        assert_eq!(session_start[0], original["hooks"]["SessionStart"][0]);
        assert_eq!(updated["hooks"]["Stop"], original["hooks"]["Stop"]);
    }

    #[test]
    fn installing_preserves_unrelated_settings() {
        let original = with_foreign_hooks();
        let (updated, _) = plan_install(&original, "/bin/agentwatch-hook");

        assert_eq!(updated["permissions"], original["permissions"]);
        assert_eq!(updated["model"], original["model"]);
    }

    #[test]
    fn installing_never_appends_into_a_foreign_group() {
        let (updated, _) = plan_install(&with_foreign_hooks(), "/bin/agentwatch-hook");

        let foreign = &updated["hooks"]["SessionStart"][0]["hooks"];
        assert_eq!(foreign.as_array().map(Vec::len), Some(1));
        assert!(
            !foreign[0]["command"]
                .as_str()
                .expect("command")
                .contains(MARKER)
        );
    }

    #[test]
    fn uninstalling_removes_exactly_what_install_added() {
        let original = with_foreign_hooks();
        let (installed, _) = plan_install(&original, "/bin/agentwatch-hook");
        let (removed, change) = plan_uninstall(&installed);

        assert_eq!(change.removed, HOOK_EVENTS.len());
        assert_eq!(
            removed, original,
            "uninstall should restore the original exactly"
        );
    }

    #[test]
    fn uninstalling_from_a_file_we_never_touched_changes_nothing() {
        let original = with_foreign_hooks();
        let (updated, change) = plan_uninstall(&original);

        assert!(change.is_empty());
        assert_eq!(updated, original);
    }

    #[test]
    fn uninstalling_prunes_the_hooks_key_it_created() {
        let (installed, _) = plan_install(&json!({ "model": "opus" }), "/bin/agentwatch-hook");
        let (removed, _) = plan_uninstall(&installed);

        assert_eq!(
            removed,
            json!({ "model": "opus" }),
            "no empty scaffolding left behind"
        );
    }

    #[test]
    fn uninstalling_keeps_a_group_that_still_has_someone_elses_command() {
        let shared = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [
                        { "type": "command", "command": "/other/tool" },
                        { "type": "command", "command": "/bin/agentwatch-hook" }
                    ] }
                ]
            }
        });

        let (removed, change) = plan_uninstall(&shared);

        assert_eq!(change.removed, 1);
        let entries = removed["hooks"]["SessionStart"][0]["hooks"]
            .as_array()
            .expect("array");
        assert_eq!(entries.len(), 1, "the co-tenant's command must survive");
        assert_eq!(entries[0]["command"], "/other/tool");
    }

    #[test]
    fn pretooluse_is_still_not_installed() {
        let (updated, _) = plan_install(&json!({}), "/bin/agentwatch-hook");
        assert!(updated["hooks"].get("PreToolUse").is_none());
    }

    #[test]
    fn key_order_survives_a_round_trip() {
        let text = r#"{"permissions":{},"model":"opus","hooks":{}}"#;
        let settings: Value = serde_json::from_str(text).expect("parse");
        let (updated, _) = plan_install(&settings, "/bin/agentwatch-hook");

        let keys: Vec<&str> = updated
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["permissions", "model", "hooks"],
            "rewriting a config must not reorder the user's keys"
        );
    }

    #[test]
    fn a_diff_marks_added_lines() {
        let diff = unified_diff("a\nb\n", "a\nx\nb\n");
        assert!(diff.contains("+ x"), "{diff}");
        assert!(diff.contains("  a"), "{diff}");
        assert!(diff.contains("  b"), "{diff}");
    }

    #[test]
    fn a_diff_marks_removed_lines() {
        let diff = unified_diff("a\nb\nc\n", "a\nc\n");
        assert!(diff.contains("- b"), "{diff}");
    }

    #[test]
    fn an_unchanged_file_produces_no_markers() {
        let diff = unified_diff("a\nb\n", "a\nb\n");
        assert!(!diff.contains('+'), "{diff}");
        assert!(!diff.contains('-'), "{diff}");
    }
}

/// Where an agent's settings live, and how to write them safely.
pub(crate) mod file {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use anyhow::{Context as _, Result, bail};
    use serde_json::Value;

    /// Default settings path for Claude Code.
    pub(crate) fn default_settings_path() -> PathBuf {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .map_or_else(
                || {
                    let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
                    home.join(".claude")
                },
                PathBuf::from,
            )
            .join("settings.json")
    }

    /// Best guess at where our hook binary lives.
    ///
    /// Prefers a sibling of the running executable, because someone running
    /// `./agentwatch install-hooks` out of a build directory means that build.
    pub(crate) fn default_hook_binary() -> PathBuf {
        if let Ok(current) = std::env::current_exe()
            && let Some(directory) = current.parent()
        {
            let sibling = directory.join("agentwatch-hook");
            if sibling.is_file() {
                return sibling;
            }
        }

        let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
        home.join(".local/bin/agentwatch-hook")
    }

    /// Reads settings, treating a missing file as an empty object.
    ///
    /// A malformed file is an error rather than something to overwrite: the
    /// alternative is destroying a configuration we could not understand.
    pub(crate) fn read(path: &Path) -> Result<Value> {
        if !path.exists() {
            return Ok(Value::Object(serde_json::Map::new()));
        }

        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(Value::Object(serde_json::Map::new()));
        }

        let settings: Value = serde_json::from_str(&text).with_context(|| {
            format!(
                "{} is not valid JSON — refusing to overwrite it",
                path.display()
            )
        })?;

        if !settings.is_object() {
            bail!("{} does not contain a JSON object", path.display());
        }
        Ok(settings)
    }

    /// Writes settings, keeping a timestamped backup of what was there.
    ///
    /// The write is a temp file plus a rename, so an interrupted run leaves the
    /// original intact rather than a half-written config for the agent to choke
    /// on at next launch.
    ///
    /// The original file's permissions are carried onto the replacement. A
    /// rename swaps the inode, so without this a settings file the user had
    /// deliberately locked down to `0600` would come back at whatever the
    /// process umask allows — a security tool has no business widening the
    /// permissions of the file it was asked to edit.
    pub(crate) fn write(path: &Path, settings: &Value) -> Result<Option<PathBuf>> {
        let directory = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;

        let existing_mode = std::fs::metadata(path).ok().map(|metadata| {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.permissions().mode()
        });

        let backup = if path.exists() {
            let backup = backup_path(path);
            std::fs::copy(path, &backup)
                .with_context(|| format!("backing up to {}", backup.display()))?;
            Some(backup)
        } else {
            None
        };

        let mut text = serde_json::to_string_pretty(settings).context("serializing settings")?;
        text.push('\n');

        let temporary = path.with_extension("json.agentwatch-tmp");
        {
            let mut file = std::fs::File::create(&temporary)
                .with_context(|| format!("creating {}", temporary.display()))?;
            file.write_all(text.as_bytes())
                .context("writing settings")?;

            if let Some(mode) = existing_mode {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(std::fs::Permissions::from_mode(mode))
                    .with_context(|| format!("restoring the mode of {}", path.display()))?;
            }

            file.sync_all().context("flushing settings")?;
        }
        std::fs::rename(&temporary, path)
            .with_context(|| format!("replacing {}", path.display()))?;

        Ok(backup)
    }

    /// Picks a backup name that is not already taken.
    ///
    /// Stamped to the second, then disambiguated: two runs within the same
    /// second would otherwise have the second one overwrite the first one's
    /// backup, which is precisely when having both matters.
    fn backup_path(path: &Path) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();

        let candidate = path.with_extension(format!("json.agentwatch-backup-{stamp}"));
        if !candidate.exists() {
            return candidate;
        }

        (1..)
            .map(|suffix| path.with_extension(format!("json.agentwatch-backup-{stamp}-{suffix}")))
            .find(|candidate| !candidate.exists())
            .unwrap_or(candidate)
    }
}

#[cfg(test)]
mod file_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_missing_file_reads_as_empty_settings() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("settings.json");
        assert_eq!(file::read(&path).expect("read"), json!({}));
    }

    #[test]
    fn malformed_json_is_refused_rather_than_overwritten() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, "{ not json").expect("write");

        let error = file::read(&path).expect_err("should refuse");
        assert!(format!("{error}").contains("refusing to overwrite"));
    }

    #[test]
    fn writing_keeps_a_backup_of_the_original() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, r#"{"model":"opus"}"#).expect("write");

        let backup = file::write(&path, &json!({ "model": "sonnet" }))
            .expect("write")
            .expect("a backup should exist");

        assert_eq!(
            std::fs::read_to_string(&backup).expect("read"),
            r#"{"model":"opus"}"#
        );
        assert!(
            std::fs::read_to_string(&path)
                .expect("read")
                .contains("sonnet")
        );
    }

    #[test]
    fn a_round_trip_through_disk_preserves_key_order() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, r#"{"permissions":{},"model":"opus"}"#).expect("write");

        let settings = file::read(&path).expect("read");
        let (updated, _) = plan_install(&settings, "/bin/agentwatch-hook");
        file::write(&path, &updated).expect("write");

        let text = std::fs::read_to_string(&path).expect("read");
        let permissions = text.find("permissions").expect("present");
        let model = text.find("\"model\"").expect("present");
        let hooks = text.find("\"hooks\"").expect("present");
        assert!(
            permissions < model && model < hooks,
            "order changed:\n{text}"
        );
    }

    #[test]
    fn a_restrictive_mode_survives_the_rewrite() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, r#"{"model":"opus"}"#).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        file::write(&path, &json!({ "model": "sonnet" })).expect("write");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "a rename must not widen the permissions of the file it replaces"
        );
    }

    #[test]
    fn two_writes_in_the_same_second_keep_both_backups() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, r#"{"step":0}"#).expect("write");

        let first = file::write(&path, &json!({ "step": 1 }))
            .expect("write")
            .expect("backup");
        let second = file::write(&path, &json!({ "step": 2 }))
            .expect("write")
            .expect("backup");

        assert_ne!(first, second, "the older backup must not be clobbered");
        assert!(
            std::fs::read_to_string(&first)
                .expect("read")
                .contains("\"step\": 0")
                || std::fs::read_to_string(&first)
                    .expect("read")
                    .contains("\"step\":0")
        );
        assert!(
            std::fs::read_to_string(&second)
                .expect("read")
                .contains('1')
        );
    }

    #[test]
    fn no_temp_file_is_left_behind() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("settings.json");
        file::write(&path, &json!({ "a": 1 })).expect("write");

        let leftovers: Vec<_> = std::fs::read_dir(directory.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind");
    }
}
