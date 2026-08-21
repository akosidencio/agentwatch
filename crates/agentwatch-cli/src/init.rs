//! One-command setup.
//!
//! Everything a fresh install needs — hooks, the collector, the menu bar, and
//! the history Claude Code has already written — done in one pass.
//!
//! The individual commands (`install-hooks`, `service install`, `import`) still
//! exist and still do exactly what they say; this is the front door that calls
//! them in the right order so nobody has to know that order exists.
//!
//! Two rules shape the design:
//!
//! - **One plan, one prompt.** Everything that would change is worked out and
//!   shown before anything is written, so the answer to "yes" covers the whole
//!   of it rather than the first of four questions.
//! - **Idempotent.** Steps already done are reported as done and skipped, which
//!   makes this equally a repair command: run it again after an upgrade and it
//!   re-points the launchd jobs at the new binaries and leaves the rest alone.

use std::path::{Path, PathBuf};

use agentwatch_storage::Store;
use agentwatch_types::Paths;
use anyhow::{Context as _, Result, bail};

use crate::{install, render, service, sync, theme, welcome};

/// What setup was asked to do.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    /// Write without asking.
    pub(crate) assume_yes: bool,
    /// Show the plan and exit.
    pub(crate) dry_run: bool,
    /// Include the menu bar status item.
    pub(crate) menu_bar: bool,
    /// Read existing transcripts once set up.
    pub(crate) import: bool,
}

/// Width the step labels are padded to, so the column lines up.
const LABEL: usize = 12;

/// What a step would do, once we have looked at the machine.
enum Action {
    /// Write these settings, whose diff against the current file is `diff`.
    Hooks {
        /// The settings file to write.
        path: PathBuf,
        /// The full settings value to write.
        updated: serde_json::Value,
        /// Diff of the current file against `updated`.
        diff: String,
        /// Hook entries being added or repointed.
        entries: usize,
    },
    /// Write and load this launchd job definition.
    Job {
        /// Which job.
        job: service::Job,
        /// The rendered plist.
        definition: String,
    },
    /// Read the agent's own transcripts.
    Import,
    /// Delete binaries an older version installed.
    Cleanup {
        /// Files to remove.
        files: Vec<PathBuf>,
    },
    /// Already satisfied. Nothing to do, and that is the good case.
    Done(String),
    /// Cannot be done on this machine, and that is not a failure.
    Unavailable(String),
}

/// One line of the plan.
struct Step {
    /// Column label, e.g. `hooks`.
    label: &'static str,
    /// What it would do, in the user's terms.
    what: String,
    /// The single command that retries just this step.
    retry: &'static str,
    /// What doing it means mechanically.
    action: Action,
}

impl Step {
    /// Whether this step would change anything.
    const fn is_pending(&self) -> bool {
        matches!(
            self.action,
            Action::Hooks { .. } | Action::Job { .. } | Action::Import | Action::Cleanup { .. }
        )
    }
}

/// Sets the machine up, or reports what it would take to.
pub(crate) fn run(paths: &Paths, options: Options) -> Result<()> {
    let settings_path = install::file::default_settings_path();
    let steps = plan(paths, &settings_path, options)?;

    heading("AgentWatch setup");
    let field = |name: &str| theme::paint(&format!("{name:<16}"), theme::MUTED);
    println!("  {}{}", field("binaries"), binary_directory().display());
    println!("  {}{}", field("settings file"), settings_path.display());
    println!("  {}{}", field("data directory"), paths.root().display());

    print_plan(&steps);

    if !steps.iter().any(Step::is_pending) {
        println!();
        println!(
            "  {}",
            theme::paint("Already set up. Nothing to do.", theme::MUTED)
        );
        report(paths, &[]);
        return Ok(());
    }

    if options.dry_run {
        println!();
        println!(
            "  {}",
            theme::paint("Dry run — nothing was written.", theme::MUTED)
        );
        return Ok(());
    }

    if !options.assume_yes && !crate::confirm("Set all this up?")? {
        println!(
            "\n  {}",
            theme::paint("Cancelled. Nothing was written.", theme::MUTED)
        );
        return Ok(());
    }

    heading("Setting up");

    let mut failures = Vec::new();
    for step in &steps {
        if !step.is_pending() {
            continue;
        }
        // Deleting the old collector while the job that runs it failed to be
        // rewritten would turn a recoverable failure into a stopped collector.
        if matches!(step.action, Action::Cleanup { .. }) && !failures.is_empty() {
            println!(
                "  {}{}{}",
                theme::paint("· ", theme::FAINT),
                label(step.label),
                theme::paint("skipped while something above is unresolved", theme::WARN)
            );
            continue;
        }
        match apply(paths, &step.action) {
            Ok(note) => println!(
                "  {}{}{}",
                theme::paint("✓ ", theme::GOOD),
                label(step.label),
                theme::paint(&note, theme::MUTED)
            ),
            Err(error) => {
                println!(
                    "  {}{}{}",
                    theme::paint("✗ ", theme::BAD),
                    label(step.label),
                    theme::paint(&format!("{error:#}"), theme::BAD)
                );
                failures.push(format!("{:<9} `{}`", step.label, step.retry));
            }
        }
    }

    report(paths, &failures);

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

/// Works out what needs doing without touching anything.
fn plan(paths: &Paths, settings_path: &Path, options: Options) -> Result<Vec<Step>> {
    let mut steps = Vec::new();

    // Everything — the hook, the collector, the status item — is this one
    // executable, so this is the only path that has to be resolved.
    let executable = install::file::resolve_executable(None)?;

    let current = install::file::read(settings_path)?;
    let (updated, change) =
        install::plan_install(&current, &agentwatch_types::hook_command(&executable));
    steps.push(Step {
        label: "hooks",
        retry: "agentwatch install-hooks",
        what: format!(
            "tell the agent to report activity ({} {})",
            change.total(),
            if change.total() == 1 {
                "entry"
            } else {
                "entries"
            }
        ),
        action: if change.is_empty() {
            Action::Done("already registered".to_owned())
        } else {
            let before = format!("{}\n", serde_json::to_string_pretty(&current)?);
            let after = format!("{}\n", serde_json::to_string_pretty(&updated)?);
            Action::Hooks {
                path: settings_path.to_path_buf(),
                updated,
                diff: install::unified_diff(&before, &after),
                entries: change.total(),
            }
        },
    });

    steps.push(job_step(
        paths,
        service::Job::Daemon,
        "collector",
        "collect events in the background",
        "agentwatch service install",
    ));

    if options.menu_bar {
        // Its own binary, and outside the workspace's default members, so a
        // build from source legitimately may not have it. `job_step` reports
        // that as unavailable rather than failing setup over a status icon.
        steps.push(job_step(
            paths,
            service::Job::MenuBar,
            "menu bar",
            "show activity in the menu bar",
            "agentwatch service install --menu-bar",
        ));
    }

    if options.import {
        steps.push(Step {
            label: "history",
            retry: "agentwatch import",
            what: "read the token usage already on disk".to_owned(),
            action: Action::Import,
        });
    }

    // Last, deliberately. These are the binaries 0.1 installed, and the launchd
    // job above is what still points at one of them: removing them before the
    // job has been rewritten would stop collection until the next run.
    let leftovers = leftovers_from_0_1(&executable);
    if !leftovers.is_empty() {
        steps.push(Step {
            label: "cleanup",
            retry: "rm",
            what: format!(
                "remove {} left by an older version",
                leftovers
                    .iter()
                    .filter_map(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            action: Action::Cleanup { files: leftovers },
        });
    }

    Ok(steps)
}

/// Binaries 0.1 installed that this version folds into the executable.
///
/// Dead weight at best. At worst actively misleading: someone debugging a hook
/// would find `agentwatch-hook` still sitting on their PATH and read an old
/// build's behaviour as the current one.
fn leftovers_from_0_1(executable: &Path) -> Vec<PathBuf> {
    let Some(directory) = executable.parent() else {
        return Vec::new();
    };

    ["agentwatch-daemon", "agentwatch-hook"]
        .into_iter()
        .map(|name| directory.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// Builds the step for one launchd job.
fn job_step(
    paths: &Paths,
    job: service::Job,
    label: &'static str,
    what: &str,
    retry: &'static str,
) -> Step {
    let action = match service::resolve_binary(job, None) {
        // Missing menu bar binary is expected in a build from source, where it
        // is outside `default-members`. Saying so beats failing setup over an
        // optional status icon.
        Err(error) => Action::Unavailable(format!("{error}")),
        Ok(binary) => {
            let definition =
                service::plist(job, &binary, paths.root(), &crate::environment_overrides());
            let path = service::plist_path(job);
            // Reinstall when the definition on disk differs — that is how an
            // upgrade re-points the job at a new binary — but leave a loaded,
            // identical job completely alone.
            let installed = std::fs::read_to_string(&path).unwrap_or_default();
            if installed == definition && service::is_loaded(job) {
                Action::Done("already running".to_owned())
            } else {
                Action::Job { job, definition }
            }
        }
    };

    Step {
        label,
        retry,
        what: what.to_owned(),
        action,
    }
}

/// Prints the plan, numbering only the steps that would do something.
fn print_plan(steps: &[Step]) {
    heading("Plan");

    let mut number = 0;
    for step in steps {
        // Padded before painting: `format!("{painted:<3}")` counts the escape
        // bytes as width and silently loses the alignment.
        let marker = if step.is_pending() {
            number += 1;
            theme::paint(&format!("{:<3}", format!("{number}.")), theme::ACCENT)
        } else {
            // A dot, not a number: skipped steps are not part of the sequence
            // the user is about to approve.
            theme::paint(&format!("{:<3}", "·"), theme::FAINT)
        };

        let detail = match &step.action {
            Action::Done(reason) => theme::paint(reason, theme::GOOD),
            Action::Unavailable(reason) => theme::paint(reason, theme::WARN),
            _ => step.what.clone(),
        };
        println!("  {marker}{}{detail}", label(step.label));
    }

    for step in steps {
        if let Action::Hooks { path, diff, .. } = &step.action {
            heading(&format!("Change to {}", path.display()));
            print!("{diff}");
        }
    }
}

/// Carries out one step, returning the one-line result to print.
fn apply(paths: &Paths, action: &Action) -> Result<String> {
    match action {
        Action::Hooks {
            path,
            updated,
            entries,
            ..
        } => {
            let backup = install::file::write(path, updated)?;
            let mut note = format!(
                "{entries} {} written to {}",
                if *entries == 1 { "entry" } else { "entries" },
                path.display()
            );
            if let Some(backup) = backup {
                note.push_str(&format!(" (backup: {})", backup.display()));
            }
            Ok(note)
        }

        Action::Job { job, definition } => {
            service::install_job(*job, definition)?;
            Ok(format!("{} started", job.label()))
        }

        Action::Import => {
            paths.ensure_root().context("creating the data directory")?;
            let mut store = Store::open(paths.database()).context("opening the database")?;
            let report = sync::import(&mut store, None)?;
            store
                .backfill_repositories(&mut agentwatch_types::RepositoryResolver::new())
                .context("resolving repositories")?;
            // Repository count is deliberately left out: `backfill_repositories`
            // reports what it resolved *this run*, so a re-import prints zero
            // and "0 repositories" reads as a failure rather than as nothing
            // left to do.
            Ok(format!(
                "{} transcripts, {} rows added",
                report.files,
                render::thousands(report.written as i64)
            ))
        }

        Action::Cleanup { files } => {
            for file in files {
                std::fs::remove_file(file)
                    .with_context(|| format!("removing {}", file.display()))?;
            }
            Ok(format!(
                "{} old {} removed",
                files.len(),
                if files.len() == 1 {
                    "binary"
                } else {
                    "binaries"
                }
            ))
        }

        // Never reached: only pending steps are applied.
        Action::Done(_) | Action::Unavailable(_) => Ok("nothing to do".to_owned()),
    }
}

/// Prints the closing summary: what is running, and what to do next.
fn report(paths: &Paths, failures: &[String]) {
    println!();
    if failures.is_empty() {
        print!("{}", welcome::banner(welcome::WELCOME));
    } else {
        println!("{}", theme::paint("Finished with problems", theme::BAD));
    }
    println!();

    let running = daemon_came_up(paths);
    let (dot, word, colour) = if running {
        ("●", "running", theme::GOOD)
    } else {
        ("○", "not running", theme::BAD)
    };
    println!(
        "  {}{}",
        label("collector"),
        theme::paint(&format!("{dot} {word}"), colour)
    );

    if let Ok(store) = Store::open_read_only(paths.database())
        && let Ok(totals) = store.totals()
    {
        println!("  {}{}", label("events"), render::thousands(totals.events));
    }

    if !failures.is_empty() {
        println!();
        println!("  Retry the steps that failed:");
        for failure in failures {
            println!("    {failure}");
        }
    }

    if !running {
        println!();
        println!(
            "  {}",
            theme::paint(
                &format!(
                    "The collector is not answering on {}.",
                    paths.socket().display()
                ),
                theme::BAD
            )
        );
        println!("  Check {}/daemon.log.", paths.root().display());
    }

    if let Some(directory) = path_warning() {
        println!();
        println!(
            "  {}",
            theme::paint(
                &format!("{directory} is not on your PATH. Add this to your shell profile:"),
                theme::WARN
            )
        );
        println!();
        println!("    export PATH=\"$PATH:{directory}\"");
    }

    println!();
    println!(
        "  {}",
        theme::paint(
            "Hooks are read when a session starts, so open a new agent session",
            theme::MUTED
        )
    );
    println!(
        "  {}",
        theme::paint(
            "(or restart your editor) before expecting live activity.",
            theme::MUTED
        )
    );
    println!();
    let hint = |command: &str, text: &str| {
        println!(
            "  {}{}",
            theme::paint(&format!("{command:<22}"), theme::ACCENT),
            theme::paint(text, theme::MUTED)
        );
    };
    hint("agentwatch status", "what has been collected");
    hint("agentwatch watch", "live dashboard");
    hint("agentwatch tokens", "token usage by project");
}

/// Waits briefly for the collector to answer.
///
/// `launchctl bootstrap` returns as soon as launchd has accepted the job, not
/// when the process is listening, so checking immediately reports a healthy
/// install as broken.
fn daemon_came_up(paths: &Paths) -> bool {
    let socket = paths.socket();
    for _ in 0..30 {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// The directory the installed binaries live in.
fn binary_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("?"))
}

/// The binary directory, if it is not on `PATH`.
///
/// Worth saying at the end of setup rather than only in the installer: the
/// installer's warning scrolls past in a `curl | sh` pipe, and every command
/// printed as a next step assumes `agentwatch` resolves.
fn path_warning() -> Option<String> {
    let directory = binary_directory();
    let directory = directory.to_str()?;
    let path = std::env::var("PATH").ok()?;
    if path.split(':').any(|entry| entry == directory) {
        return None;
    }
    Some(directory.to_owned())
}

/// Prints a section heading.
///
/// The rule is what makes this read as a section in a wall of output. Drawn to
/// the heading's own width rather than the terminal's, so it frames the title
/// instead of cutting the screen in half.
fn heading(title: &str) {
    println!();
    println!("  {}", theme::bold(title));
    println!(
        "  {}",
        theme::paint(&"─".repeat(title.chars().count()), theme::FAINT)
    );
}

/// Pads a step label to the shared column width.
fn label(text: &str) -> String {
    theme::paint(&format!("{text:<LABEL$}"), theme::MUTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            assume_yes: true,
            dry_run: true,
            menu_bar: true,
            import: true,
        }
    }

    #[test]
    fn a_fresh_machine_plans_every_step() {
        let home = tempfile::tempdir().expect("temp dir");
        let paths = Paths::with_root(home.path().join("data"));
        let settings = home.path().join("settings.json");

        // Resolving the executable reads the real filesystem, so this only
        // runs where one exists. Skipping beats a test that fails for reasons
        // unrelated to the planner.
        if install::file::resolve_executable(None).is_err() {
            return;
        }

        let steps = plan(&paths, &settings, options()).expect("planning");
        assert_eq!(steps.len(), 4, "hooks, collector, menu bar, history");
        assert_eq!(steps[2].label, "menu bar");
        assert_eq!(steps[0].label, "hooks");
        assert!(steps[0].is_pending(), "a missing settings file needs hooks");
        assert_eq!(steps[1].label, "collector");
        assert_eq!(steps[3].label, "history");
    }

    #[test]
    fn opting_out_drops_those_steps() {
        let home = tempfile::tempdir().expect("temp dir");
        let paths = Paths::with_root(home.path().join("data"));
        let settings = home.path().join("settings.json");

        if install::file::resolve_executable(None).is_err() {
            return;
        }

        let steps = plan(
            &paths,
            &settings,
            Options {
                menu_bar: false,
                import: false,
                ..options()
            },
        )
        .expect("planning");
        let labels: Vec<_> = steps.iter().map(|step| step.label).collect();
        assert_eq!(labels, ["hooks", "collector"]);
    }

    #[test]
    fn hooks_already_present_are_reported_as_done() {
        let home = tempfile::tempdir().expect("temp dir");
        let paths = Paths::with_root(home.path().join("data"));
        let settings = home.path().join("settings.json");

        let Ok(executable) = install::file::resolve_executable(None) else {
            return;
        };
        let (installed, _) = install::plan_install(
            &serde_json::json!({}),
            &agentwatch_types::hook_command(&executable),
        );
        std::fs::write(
            &settings,
            serde_json::to_string_pretty(&installed).expect("serialising"),
        )
        .expect("writing settings");

        let steps = plan(&paths, &settings, options()).expect("planning");
        assert!(
            matches!(steps[0].action, Action::Done(_)),
            "a second run must not add a second hook entry"
        );
    }
}
