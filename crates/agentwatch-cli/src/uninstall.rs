//! Taking it all back off the machine.
//!
//! The counterpart to [`crate::init`], and deliberately its mirror image: work
//! out everything that would be removed, show it, ask once, then do it. A tool
//! that installs itself in four places with one command and needs five to come
//! back off is a tool people stop trusting with the first command.
//!
//! Two things are treated as more serious than the rest:
//!
//! - **Collected data is kept unless asked for.** Binaries can be downloaded
//!   again; months of history cannot. `--purge` deletes it, and until then the
//!   closing summary says where it still is.
//! - **The order is the reverse of setup.** Hooks first, so nothing new is
//!   recorded; the executables last, because one of them is running this.

use std::path::{Path, PathBuf};

use agentwatch_types::Paths;
use anyhow::{Context as _, Result, bail};

use crate::{install, service, theme};

/// What to take off.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    /// Remove without asking.
    pub(crate) assume_yes: bool,
    /// Show the plan and exit.
    pub(crate) dry_run: bool,
    /// Also delete the database and everything else collected.
    pub(crate) purge: bool,
    /// Leave the executables where they are.
    pub(crate) keep_binaries: bool,
}

/// Marker the installer writes above the line it adds to a shell profile.
///
/// Must match `install.sh`. Only a block carrying it is ever touched, so a
/// `PATH` line somebody wrote by hand is left alone.
const PROFILE_MARKER: &str = "# added by the AgentWatch installer";

/// What a step would do.
enum Action {
    /// Write these settings back, with our hooks gone.
    Hooks {
        /// The settings file.
        path: PathBuf,
        /// The settings without our entries.
        updated: serde_json::Value,
        /// Diff against what is there now.
        diff: String,
        /// Entries being removed.
        removed: usize,
    },
    /// Unload a launchd job and delete its definition.
    Job {
        /// Which job.
        job: service::Job,
        /// Its definition on disk, if it is there.
        plist: Option<PathBuf>,
    },
    /// Strip the installer's block out of a shell profile.
    Profile {
        /// The profile file.
        path: PathBuf,
        /// Its contents with the block gone.
        updated: String,
    },
    /// Delete the data directory.
    Data,
    /// Delete the executables.
    Binaries {
        /// Files to remove.
        files: Vec<PathBuf>,
    },
    /// Nothing to do, because it is already not there.
    Absent(String),
    /// Deliberately not doing it.
    Kept(String),
}

/// One line of the plan.
struct Step {
    /// Column label.
    label: &'static str,
    /// What it would remove, in the user's terms.
    what: String,
    /// The single command that does just this step.
    retry: &'static str,
    /// What doing it means mechanically.
    action: Action,
}

impl Step {
    /// Whether this step would remove anything.
    const fn is_pending(&self) -> bool {
        matches!(
            self.action,
            Action::Hooks { .. }
                | Action::Job { .. }
                | Action::Profile { .. }
                | Action::Data
                | Action::Binaries { .. }
        )
    }

    /// Whether this step is unsafe after an earlier prerequisite failed.
    const fn requires_clean_run(&self) -> bool {
        matches!(self.action, Action::Data | Action::Binaries { .. })
    }
}

/// Removes AgentWatch from the machine, or reports what that would take.
pub(crate) fn run(paths: &Paths, options: Options) -> Result<()> {
    let settings_path = install::file::default_settings_path();
    let steps = plan(paths, &settings_path, options)?;

    theme::heading("AgentWatch uninstall");
    let field = |name: &str| theme::paint(&format!("{name:<16}"), theme::MUTED);
    println!("  {}{}", field("settings file"), settings_path.display());
    println!("  {}{}", field("data directory"), paths.root().display());

    print_plan(&steps);

    if !steps.iter().any(Step::is_pending) {
        println!();
        println!(
            "  {}",
            theme::paint("Nothing installed. Nothing to remove.", theme::MUTED)
        );
        return Ok(());
    }

    if options.dry_run {
        println!();
        println!(
            "  {}",
            theme::paint("Dry run — nothing was removed.", theme::MUTED)
        );
        return Ok(());
    }

    if !options.assume_yes && !crate::confirm("Remove all this?")? {
        println!(
            "\n  {}",
            theme::paint("Cancelled. Nothing was removed.", theme::MUTED)
        );
        return Ok(());
    }

    theme::heading("Removing");
    println!();

    let mut failures = Vec::new();
    for step in &steps {
        if !step.is_pending() {
            continue;
        }
        // Keep the executable available for the retry commands, and never purge
        // a database while a job that should have been stopped may still own it.
        if !failures.is_empty() && step.requires_clean_run() {
            println!(
                "  {}{}{}",
                theme::paint("· ", theme::FAINT),
                theme::label(step.label),
                theme::paint("skipped while an earlier step is unresolved", theme::WARN)
            );
            continue;
        }
        match apply(paths, &step.action) {
            Ok(note) => println!(
                "  {}{}{}",
                theme::paint("✓ ", theme::GOOD),
                theme::label(step.label),
                theme::paint(&note, theme::MUTED)
            ),
            Err(error) => {
                println!(
                    "  {}{}{}",
                    theme::paint("✗ ", theme::BAD),
                    theme::label(step.label),
                    theme::paint(&format!("{error:#}"), theme::BAD)
                );
                failures.push(format!("{:<9} `{}`", step.label, step.retry));
            }
        }
    }

    report(paths, options, &failures);

    if failures.is_empty() {
        Ok(())
    } else {
        bail!(
            "{} of {} steps failed",
            failures.len(),
            steps.iter().filter(|step| step.is_pending()).count()
        )
    }
}

/// Works out what is there to remove, without removing anything.
fn plan(paths: &Paths, settings_path: &Path, options: Options) -> Result<Vec<Step>> {
    let mut steps = Vec::new();

    // Hooks first: everything below stops collection, but this is the one that
    // stops the agent from spawning a hook that has nowhere to report to.
    let current = install::file::read(settings_path)?;
    let (updated, change) = install::plan_uninstall(&current);
    steps.push(Step {
        label: "hooks",
        retry: "agentwatch install-hooks --uninstall",
        what: format!(
            "stop the agent reporting activity ({} entries)",
            change.removed
        ),
        action: if change.is_empty() {
            Action::Absent("no hooks of ours registered".to_owned())
        } else {
            let before = format!("{}\n", serde_json::to_string_pretty(&current)?);
            let after = format!("{}\n", serde_json::to_string_pretty(&updated)?);
            Action::Hooks {
                path: settings_path.to_path_buf(),
                updated,
                diff: install::unified_diff(&before, &after),
                removed: change.removed,
            }
        },
    });

    for (job, label, retry) in [
        (
            service::Job::Daemon,
            "collector",
            "agentwatch service uninstall",
        ),
        (
            service::Job::MenuBar,
            "menu bar",
            "agentwatch service uninstall --menu-bar",
        ),
    ] {
        let path = service::plist_path(job);
        let installed = path.exists();
        let loaded = service::is_loaded(job);
        steps.push(Step {
            label,
            retry,
            what: format!("stop and remove {}", job.label()),
            action: if installed || loaded {
                Action::Job {
                    job,
                    plist: installed.then_some(path),
                }
            } else {
                Action::Absent("not installed".to_owned())
            },
        });
    }

    // The line `install.sh` appended. Every candidate profile is checked rather
    // than the current shell's, because an uninstall may well be typed in a
    // different shell than the install was.
    steps.push(profile_step());

    steps.push(Step {
        label: "data",
        retry: "rm -rf",
        what: format!("delete everything collected in {}", paths.root().display()),
        action: if !paths.root().exists() {
            Action::Absent("no data directory".to_owned())
        } else if options.purge {
            Action::Data
        } else {
            Action::Kept("kept — pass --purge to delete it".to_owned())
        },
    });

    // Last. One of these is the process doing the removing: macOS is happy to
    // unlink a running executable, but anything that needed to read it again
    // would be reading a file that no longer has a name.
    let binaries = installed_binaries();
    steps.push(Step {
        label: "binaries",
        retry: "rm",
        // Named with their directory: `uninstall` removes the binary it is
        // running from, the same way `init` wires up the one it is running
        // from. Run out of a checkout, that is the build tree — which is
        // correct, and worth being able to see before agreeing to it.
        what: format!(
            "remove {} from {}",
            binaries
                .iter()
                .filter_map(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" and "),
            binaries
                .first()
                .and_then(|path| path.parent())
                .unwrap_or(Path::new("?"))
                .display()
        ),
        action: if options.keep_binaries {
            Action::Kept("kept — --keep-binaries".to_owned())
        } else if binaries.is_empty() {
            Action::Absent("nothing to remove".to_owned())
        } else {
            Action::Binaries { files: binaries }
        },
    });

    Ok(steps)
}

/// The step for the `PATH` line the installer added.
fn profile_step() -> Step {
    let edited = profile_candidates().into_iter().find_map(|path| {
        let text = std::fs::read_to_string(&path).ok()?;
        let updated = without_marked_block(&text)?;
        Some((path, updated))
    });

    Step {
        label: "PATH",
        retry: "edit it by hand",
        what: "remove the line the installer added to your shell profile".to_owned(),
        action: match edited {
            Some((path, updated)) => Action::Profile { path, updated },
            None => Action::Absent("no installer line in a shell profile".to_owned()),
        },
    }
}

/// Profiles the installer could have written to.
fn profile_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
    [
        ".zshrc",
        ".bash_profile",
        ".bashrc",
        ".config/fish/config.fish",
    ]
    .into_iter()
    .map(|name| home.join(name))
    .collect()
}

/// Removes the installer's marker line and the line it introduces.
///
/// Returns `None` when there is nothing of ours in the text, so a file we never
/// touched is never rewritten. Only the line *directly after* the marker goes
/// with it: anything else in the file is somebody's own configuration.
fn without_marked_block(text: &str) -> Option<String> {
    if !text.contains(PROFILE_MARKER) {
        return None;
    }

    let mut kept: Vec<&str> = Vec::new();
    let mut skip_next = false;
    for line in text.lines() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if line.trim() == PROFILE_MARKER {
            skip_next = true;
            // The installer writes a blank line before the marker. Taking it
            // with the block keeps the file from collecting blank lines every
            // install/uninstall cycle.
            if kept.last().is_some_and(|last| last.trim().is_empty()) {
                kept.pop();
            }
            continue;
        }
        kept.push(line);
    }

    let mut updated = kept.join("\n");
    // A trailing newline the original had, and that `lines` does not report.
    if text.ends_with('\n') && !updated.is_empty() {
        updated.push('\n');
    }
    Some(updated)
}

/// Executables the installer would have put in place.
fn installed_binaries() -> Vec<PathBuf> {
    let Ok(current) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(directory) = current.parent() else {
        return Vec::new();
    };

    // The companion first, so a failure part-way through does not leave the
    // status item behind with nothing to talk to.
    ["agentwatch-menubar", "agentwatch"]
        .into_iter()
        .map(|name| directory.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// Prints the plan, numbering only the steps that would remove something.
fn print_plan(steps: &[Step]) {
    theme::heading("Plan");

    let mut number = 0;
    for step in steps {
        let marker = if step.is_pending() {
            number += 1;
            theme::paint(&format!("{:<3}", format!("{number}.")), theme::ACCENT)
        } else {
            theme::paint(&format!("{:<3}", "·"), theme::FAINT)
        };

        let detail = match &step.action {
            Action::Absent(reason) => theme::paint(reason, theme::FAINT),
            Action::Kept(reason) => theme::paint(reason, theme::WARN),
            _ => step.what.clone(),
        };
        println!("  {marker}{}{detail}", theme::label(step.label));
    }

    for step in steps {
        if let Action::Hooks { path, diff, .. } = &step.action {
            theme::heading(&format!("Change to {}", path.display()));
            print!("{diff}");
        }
    }
}

/// Carries out one step, returning the line to print.
fn apply(paths: &Paths, action: &Action) -> Result<String> {
    match action {
        Action::Hooks {
            path,
            updated,
            removed,
            ..
        } => {
            let backup = install::file::write(path, updated)?;
            let mut note = format!(
                "{removed} {} removed from {}",
                if *removed == 1 { "entry" } else { "entries" },
                path.display()
            );
            if let Some(backup) = backup {
                note.push_str(&format!(" (backup: {})", backup.display()));
            }
            Ok(note)
        }

        Action::Job { job, plist } => {
            if service::is_loaded(*job) {
                service::bootout(*job).context("unloading the job")?;
            }
            if let Some(plist) = plist {
                std::fs::remove_file(plist)
                    .with_context(|| format!("removing {}", plist.display()))?;
            }
            Ok(format!("{} stopped and removed", job.label()))
        }

        Action::Profile { path, updated } => {
            // Written through a temp file and renamed, like every other file
            // this tool touches: an interrupted write must not leave someone
            // with a shell profile that half-parses.
            let temporary = path.with_extension("agentwatch-tmp");
            std::fs::write(&temporary, updated)
                .with_context(|| format!("writing {}", temporary.display()))?;
            std::fs::rename(&temporary, path)
                .with_context(|| format!("replacing {}", path.display()))?;
            Ok(format!("line removed from {}", path.display()))
        }

        Action::Data => {
            let root = paths.root();
            std::fs::remove_dir_all(root)
                .with_context(|| format!("removing {}", root.display()))?;
            Ok(format!("{} deleted", root.display()))
        }

        Action::Binaries { files } => {
            for file in files {
                std::fs::remove_file(file)
                    .with_context(|| format!("removing {}", file.display()))?;
            }
            Ok(format!(
                "{} {} removed",
                files.len(),
                if files.len() == 1 { "file" } else { "files" }
            ))
        }

        // Never reached: only pending steps are applied.
        Action::Absent(_) | Action::Kept(_) => Ok("nothing to do".to_owned()),
    }
}

/// Prints what is gone, and what deliberately is not.
fn report(paths: &Paths, options: Options, failures: &[String]) {
    println!();
    if failures.is_empty() {
        println!("  {}", theme::bold("Removed."));
    } else {
        println!("{}", theme::paint("  Finished with problems", theme::BAD));
        println!();
        println!("  Retry the steps that failed:");
        for failure in failures {
            println!("    {failure}");
        }
    }

    if !options.purge && paths.root().exists() {
        println!();
        println!(
            "  {}",
            theme::paint("Your collected data was kept:", theme::MUTED)
        );
        println!("    {}", paths.root().display());
        println!(
            "  {}",
            theme::paint(
                "Re-installing picks it up where it left off. `--purge` deletes it.",
                theme::MUTED
            )
        );
    }

    if !options.keep_binaries && failures.is_empty() {
        println!();
        println!(
            "  {}",
            theme::paint(
                "Open a new terminal — this shell still has the old PATH cached.",
                theme::MUTED
            )
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            assume_yes: true,
            dry_run: true,
            purge: false,
            keep_binaries: true,
        }
    }

    #[test]
    fn data_is_kept_unless_purge_is_asked_for() {
        let home = tempfile::tempdir().expect("temp dir");
        let root = home.path().join("data");
        std::fs::create_dir_all(&root).expect("data dir");
        let paths = Paths::with_root(&root);
        let settings = home.path().join("settings.json");

        let steps = plan(&paths, &settings, options()).expect("planning");
        let data = steps
            .iter()
            .find(|step| step.label == "data")
            .expect("data step");
        assert!(
            matches!(data.action, Action::Kept(_)),
            "months of history must not go without being asked for"
        );
        assert!(root.exists(), "planning must not delete anything");

        let steps = plan(
            &paths,
            &settings,
            Options {
                purge: true,
                ..options()
            },
        )
        .expect("planning");
        let data = steps
            .iter()
            .find(|step| step.label == "data")
            .expect("data step");
        assert!(matches!(data.action, Action::Data));
    }

    #[test]
    fn nothing_installed_means_nothing_to_remove() {
        let home = tempfile::tempdir().expect("temp dir");
        let paths = Paths::with_root(home.path().join("nowhere"));
        let settings = home.path().join("settings.json");

        // Only the settings file and the data directory are injectable: the
        // launchd jobs, the shell profile, and the executables are read off the
        // real machine, and this test would otherwise assert that whoever runs
        // it has AgentWatch uninstalled.
        let steps = plan(&paths, &settings, options()).expect("planning");
        for label in ["hooks", "data"] {
            let step = steps
                .iter()
                .find(|step| step.label == label)
                .expect("step present");
            assert!(
                !step.is_pending(),
                "{label} was planned for removal with nothing there to remove"
            );
            assert!(matches!(step.action, Action::Absent(_)), "{label}");
        }
    }

    #[test]
    fn hooks_are_planned_for_removal_when_they_are_there() {
        let home = tempfile::tempdir().expect("temp dir");
        let paths = Paths::with_root(home.path().join("data"));
        let settings = home.path().join("settings.json");

        let (installed, _) =
            install::plan_install(&serde_json::json!({}), "/opt/bin/agentwatch hook");
        std::fs::write(
            &settings,
            serde_json::to_string_pretty(&installed).expect("json"),
        )
        .expect("write");

        let steps = plan(&paths, &settings, options()).expect("planning");
        let hooks = steps
            .iter()
            .find(|step| step.label == "hooks")
            .expect("hooks step");
        // One entry per registered event: the count is derived rather than
        // written out, so growing `HOOK_EVENTS` cannot silently leave entries
        // behind at uninstall.
        assert!(matches!(
            hooks.action,
            Action::Hooks { removed, .. } if removed == install::HOOK_EVENTS.len()
        ));
    }

    #[test]
    fn destructive_steps_wait_for_prerequisites() {
        let data = Step {
            label: "data",
            retry: "retry",
            what: String::new(),
            action: Action::Data,
        };
        let binaries = Step {
            label: "binaries",
            retry: "retry",
            what: String::new(),
            action: Action::Binaries { files: Vec::new() },
        };
        let profile = Step {
            label: "PATH",
            retry: "retry",
            what: String::new(),
            action: Action::Profile {
                path: PathBuf::new(),
                updated: String::new(),
            },
        };

        assert!(data.requires_clean_run());
        assert!(binaries.requires_clean_run());
        assert!(!profile.requires_clean_run());
    }

    #[test]
    fn only_the_installers_own_line_comes_out_of_a_profile() {
        let profile = format!(
            "# my zshrc\nalias ll=\"ls -l\"\nexport PATH=\"$PATH:/my/own/bin\"\n\n{PROFILE_MARKER}\nexport PATH=\"$PATH:$HOME/.local/bin\"\n"
        );
        let updated = without_marked_block(&profile).expect("marker found");
        assert_eq!(
            updated,
            "# my zshrc\nalias ll=\"ls -l\"\nexport PATH=\"$PATH:/my/own/bin\"\n"
        );
    }

    #[test]
    fn a_profile_we_never_wrote_to_is_left_alone() {
        assert!(
            without_marked_block("# my zshrc\nexport PATH=\"$PATH:/my/own/bin\"\n").is_none(),
            "a file with no marker must not be rewritten at all"
        );
    }

    #[test]
    fn a_marked_block_in_the_middle_of_a_file_is_removed_cleanly() {
        let profile =
            format!("first\n\n{PROFILE_MARKER}\nexport PATH=\"$PATH:$HOME/.local/bin\"\nlast\n");
        assert_eq!(
            without_marked_block(&profile).expect("marker found"),
            "first\nlast\n"
        );
    }
}
