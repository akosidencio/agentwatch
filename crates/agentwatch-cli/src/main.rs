//! The AgentWatch CLI.
//!
//! Phase 1 is deliberately thin: enough to prove the pipeline works and to wire
//! the hooks up. The analytics commands arrive in phase 3.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod hook_config;
mod init;
mod install;
mod range;
mod render;
mod service;
mod sync;
mod theme;
mod uninstall;
mod update;
mod watch;
mod welcome;

use std::path::PathBuf;

use agentwatch_storage::{ActivityFilter, Coverage, Store, TokenTotals};
use agentwatch_types::Paths;
use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

/// Command line arguments.
#[derive(Debug, Parser)]
#[command(
    name = "agentwatch",
    version,
    about = "See what your AI agents are doing."
)]
struct Cli {
    /// What to do.
    ///
    /// Optional: a bare `agentwatch` is a welcome screen rather than a usage
    /// error, because the first thing most people type after installing
    /// something is its name.
    #[command(subcommand)]
    command: Option<Command>,
}

/// How to group a token breakdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Grouping {
    /// By repository, rolling up every directory inside it.
    Project,
    /// By the exact working directory the session started in.
    Directory,
    /// By the exact model identifier the provider reported.
    Model,
    /// By calendar day in your local timezone.
    Day,
}

/// Ways to manage the background service.
#[derive(Debug, Subcommand)]
enum ServiceAction {
    /// Install and start the LaunchAgent.
    Install {
        /// Path to the binary the job runs.
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Act on the menu bar status item instead of the collector.
        #[arg(long)]
        menu_bar: bool,
        /// Show the job definition and exit without writing.
        #[arg(long)]
        dry_run: bool,
        /// Write without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Stop and remove the LaunchAgent.
    Uninstall {
        /// Act on the menu bar status item instead of the collector.
        #[arg(long)]
        menu_bar: bool,
    },
    /// Report whether each job is installed and loaded.
    Status,
}

/// The available commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Set everything up: hooks, the background service, and existing history.
    ///
    /// Shows the whole plan and asks once. Safe to re-run: steps already done
    /// are skipped, so this doubles as the repair command after an upgrade.
    Init {
        /// Show the plan and exit without writing.
        #[arg(long)]
        dry_run: bool,
        /// Set up without asking.
        #[arg(long, short)]
        yes: bool,
        /// Leave the menu bar status item out.
        #[arg(long)]
        no_menu_bar: bool,
        /// Skip reading the history Claude Code has already written.
        #[arg(long)]
        no_import: bool,
    },
    /// Replace the installed binaries with a published release.
    ///
    /// Downloads the release for this architecture, verifies it against the
    /// published SHA256SUMS, puts it in place, and restarts whichever launchd
    /// jobs are running — which is the step that is easy to forget by hand, and
    /// without which the collector keeps running the previous build.
    Update {
        /// Release to install, e.g. `0.1.2`. Defaults to the latest.
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        /// Show what would be replaced and exit.
        #[arg(long)]
        dry_run: bool,
        /// Replace without asking.
        #[arg(long, short)]
        yes: bool,
    },
    /// Take AgentWatch back off this machine.
    ///
    /// The counterpart to `init`: hooks, both launchd jobs, the installer's
    /// PATH line, and the executables. Collected data is kept unless `--purge`
    /// says otherwise. Shows the whole plan and asks once.
    Uninstall {
        /// Show the plan and exit without removing anything.
        #[arg(long)]
        dry_run: bool,
        /// Remove without asking.
        #[arg(long, short)]
        yes: bool,
        /// Also delete the database and everything collected in it.
        #[arg(long)]
        purge: bool,
        /// Leave the executables in place.
        #[arg(long)]
        keep_binaries: bool,
    },
    /// Show whether the daemon is running and what it has collected.
    Status,
    /// List the most recent events.
    Events {
        /// How many events to show.
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
    },
    /// Show token usage.
    Tokens {
        /// Group the breakdown by project, model, or day.
        #[arg(long, value_enum, default_value_t = Grouping::Project)]
        by: Grouping,
        /// Number of calendar days to include, ending today.
        #[arg(long, default_value_t = 1, conflicts_with_all = ["from", "all"])]
        days: u32,
        /// Start date, `YYYY-MM-DD`, in your local timezone.
        #[arg(long, requires = "to", conflicts_with = "all")]
        from: Option<String>,
        /// End date, `YYYY-MM-DD`, inclusive.
        #[arg(long, requires = "from")]
        to: Option<String>,
        /// Ignore dates and count everything ever recorded.
        #[arg(long)]
        all: bool,
        /// Show at most this many rows in the breakdown.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Add our hooks to the agent's settings, after showing the diff.
    InstallHooks {
        /// Path to the agentwatch executable to register.
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Settings file to edit.
        #[arg(long)]
        settings: Option<PathBuf>,
        /// Remove our hooks instead of adding them.
        #[arg(long)]
        uninstall: bool,
        /// Write without asking.
        #[arg(long)]
        yes: bool,
        /// Show the diff and exit without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop recording new events until resumed.
    Pause,
    /// Resume recording after a pause.
    Resume,
    /// Manage the background service.
    Service {
        /// What to do.
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Watch activity live in a full-screen view.
    Watch,
    /// List sessions.
    Sessions {
        /// Number of calendar days to include, ending today.
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Show at most this many sessions.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Show what each session's data can and cannot answer.
        #[arg(long)]
        coverage: bool,
    },
    /// Show a timeline of what agents did.
    Activity {
        /// Number of calendar days to include, ending today.
        #[arg(long, default_value_t = 1)]
        days: u32,
        /// Show at most this many events.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Restrict to one agent.
        #[arg(long)]
        agent: Option<String>,
        /// Restrict to one session id.
        #[arg(long)]
        session: Option<String>,
        /// Restrict to repositories or directories under this path.
        #[arg(long)]
        project: Option<String>,
        /// Restrict to event kinds, for example `command` or `file.read`.
        #[arg(long, value_delimiter = ',')]
        kind: Vec<String>,
    },
    /// List access to sensitive paths.
    Security {
        /// Number of calendar days to include, ending today.
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Show at most this many entries.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Write events to stdout as JSON Lines.
    Export {
        /// Number of calendar days to include, ending today.
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Export at most this many events.
        #[arg(long, default_value_t = 100_000)]
        limit: u32,
        /// Restrict to event kinds.
        #[arg(long, value_delimiter = ',')]
        kind: Vec<String>,
    },
    /// Read historical token usage out of Claude Code's own transcripts.
    ///
    /// Safe to run repeatedly: nothing is double counted.
    Import {
        /// Read at most this many transcript files.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Re-derive totals from the transcripts and report any disagreement.
    Verify,
    /// Run the collector in the foreground.
    ///
    /// `agentwatch service install` runs this under launchd, which is what you
    /// normally want. Run it directly to watch it work, or to see why it will
    /// not start.
    Daemon,
    /// Forward one hook payload from stdin to the collector.
    ///
    /// The agent runs this, not you: it is the command `install-hooks` writes
    /// into your settings. It reads a JSON payload on stdin and exits 0 on
    /// every path, whatever goes wrong, because a monitor that can fail a tool
    /// call is worse than no monitor.
    #[command(hide = true)]
    Hook,
    /// Print the settings needed to enable monitoring.
    ///
    /// Prints only. Nothing is written to your Claude Code configuration; copy
    /// the output yourself, or wait for `install-hooks` in phase 4.
    HookConfig {
        /// Path to the agentwatch executable to reference.
        #[arg(long)]
        binary: Option<String>,
    },
}

fn main() -> Result<()> {
    quiet_broken_pipe();

    // The hook is dispatched before clap, and before anything else that can
    // fail. It runs in the agent's critical path and must exit 0 on every path;
    // clap exits 2 on an argument it does not recognise, and resolving the data
    // directory can fail outright. The hook resolves its own paths and swallows
    // its own failures, so it needs neither.
    if std::env::args_os()
        .nth(1)
        .is_some_and(|first| first == "hook")
    {
        return hook();
    }

    let cli = Cli::parse();
    let paths = Paths::from_env().context("resolving the data directory")?;

    let Some(command) = cli.command else {
        welcome::overview(&paths);
        return Ok(());
    };

    match command {
        Command::Init {
            dry_run,
            yes,
            no_menu_bar,
            no_import,
        } => init::run(
            &paths,
            init::Options {
                assume_yes: yes,
                dry_run,
                menu_bar: !no_menu_bar,
                import: !no_import,
            },
        ),
        Command::Update {
            version,
            dry_run,
            yes,
        } => update::run(&update::Options {
            version,
            dry_run,
            assume_yes: yes,
        }),
        Command::Uninstall {
            dry_run,
            yes,
            purge,
            keep_binaries,
        } => uninstall::run(
            &paths,
            uninstall::Options {
                assume_yes: yes,
                dry_run,
                purge,
                keep_binaries,
            },
        ),
        Command::Status => status(&paths),
        Command::Events { limit } => events(&paths, limit),
        Command::Tokens {
            by,
            days,
            from,
            to,
            all,
            limit,
        } => tokens(&paths, by, days, from.as_deref(), to.as_deref(), all, limit),
        Command::InstallHooks {
            binary,
            settings,
            uninstall,
            yes,
            dry_run,
        } => install_hooks(binary, settings, uninstall, yes, dry_run),
        Command::Pause => set_paused(&paths, true),
        Command::Resume => set_paused(&paths, false),
        Command::Service { action } => service_command(&paths, action),
        Command::Watch => watch::run(&paths),
        Command::Sessions {
            days,
            limit,
            coverage,
        } => sessions(&paths, days, limit, coverage),
        Command::Activity {
            days,
            limit,
            agent,
            session,
            project,
            kind,
        } => activity(
            &paths,
            days,
            limit,
            ActivityFilter {
                agent,
                session,
                project_prefix: project,
                kinds: kind,
            },
        ),
        Command::Security { days, limit } => security(&paths, days, limit),
        Command::Export { days, limit, kind } => export(&paths, days, limit, kind),
        Command::Import { limit } => import(&paths, limit),
        Command::Verify => verify(&paths),
        Command::Daemon => agentwatch_daemon::run(),
        // Unreachable in practice: `main` intercepts this before clap runs.
        // Kept so the dispatch matches the command list rather than relying on
        // the fast path being the only way in.
        Command::Hook => hook(),
        Command::HookConfig { binary } => {
            print!("{}", hook_config::snippet(binary.as_deref()));
            Ok(())
        }
    }
}

/// Exits quietly when the thing reading our output goes away.
///
/// `println!` panics if the pipe is closed, so `agentwatch export | head` ended
/// in a Rust panic message and exit 101 — on a tool whose one-shot commands are
/// documented as pipeable. The usual fix is to restore the default `SIGPIPE`
/// disposition, which needs `libc` and `unsafe`; this workspace forbids
/// `unsafe`, and that policy is worth more than the exit code, so the panic is
/// intercepted instead.
///
/// Exits 0: `head` closing its end is a normal way for a pipeline to finish,
/// not a failure of ours.
fn quiet_broken_pipe() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or_default();

        // Matched on the message because that is all a panic from `println!`
        // carries; every other panic still reports normally.
        if message.contains("Broken pipe") {
            std::process::exit(0);
        }
        previous(info);
    }));
}

/// Forwards one hook payload, and never fails doing it.
fn hook() -> Result<()> {
    use std::io::IsTerminal as _;

    // Typed by a human, this would otherwise block on a terminal forever
    // waiting for a payload nobody is going to type.
    if std::io::stdin().is_terminal() {
        eprintln!(
            "`agentwatch hook` reads a hook payload on stdin. The agent runs it \
             for you — see `agentwatch init`."
        );
        return Ok(());
    }

    agentwatch_hook::run();
    Ok(())
}

/// Prints daemon liveness and headline counts.
fn status(paths: &Paths) -> Result<()> {
    let socket = paths.socket();
    let running = std::os::unix::net::UnixStream::connect(&socket).is_ok();

    // A filled dot for running, hollow for not: the shape carries the state on
    // its own, so the line still reads correctly with colour stripped.
    let (dot, word, colour) = if running {
        ("●", "running", theme::GOOD)
    } else {
        ("○", "not running", theme::WARN)
    };
    let label = |text: &str| theme::paint(&format!("{text:<8}"), theme::MUTED);

    println!(
        "{}  {}",
        label("daemon"),
        theme::paint(&format!("{dot} {word}"), colour)
    );
    println!("{}  {}", label("socket"), socket.display());
    if paths.is_paused() {
        println!(
            "{}",
            theme::paint(
                "collection PAUSED — run `agentwatch resume` to record again",
                theme::BAD
            )
        );
    }
    println!("{}  {}", label("database"), paths.database().display());

    if !paths.database().exists() {
        println!("\nNo database yet. Run `agentwatch init` to set up.");
        return Ok(());
    }

    let store = Store::open_read_only(paths.database()).context("opening the database")?;
    let totals = store.totals().context("reading totals")?;

    println!();
    let counter = |label: &str, value: i64| {
        println!(
            "{}  {value}",
            theme::paint(&format!("{label:<16}"), theme::MUTED)
        );
    };
    counter("events", totals.events);
    counter("sessions", totals.sessions);
    counter("active sessions", totals.active_sessions);
    counter("projects", totals.projects);

    Ok(())
}

/// Prints token usage for a range, with a breakdown.
#[allow(clippy::too_many_arguments)]
fn tokens(
    paths: &Paths,
    by: Grouping,
    days: u32,
    from: Option<&str>,
    to: Option<&str>,
    all: bool,
    limit: usize,
) -> Result<()> {
    let store = open_for_reading(paths)?;
    let (offset, offset_is_local) = range::local_offset();

    let range = match (all, from, to) {
        (true, _, _) => range::all_time(offset),
        (_, Some(from), Some(to)) => range::explicit(from, to, offset)?,
        _ => range::last_days(days, offset),
    };

    let totals = store
        .token_totals(range.from_us, range.to_us)
        .context("reading totals")?;

    println!("{}", theme::bold(&format!("Token usage — {}", range.label)));
    if !offset_is_local {
        println!("(times in UTC: the local timezone could not be determined)");
    }
    println!();
    print_totals(&totals);

    let groups = match by {
        Grouping::Project => store.tokens_by_repository(range.from_us, range.to_us),
        Grouping::Directory => store.tokens_by_project(range.from_us, range.to_us),
        Grouping::Model => store.tokens_by_model(range.from_us, range.to_us),
        Grouping::Day => store.tokens_by_day(range.from_us, range.to_us, range.offset_seconds()),
    }
    .context("reading the breakdown")?;

    if groups.is_empty() {
        return Ok(());
    }

    println!();
    println!("{}", render::group_header(by_label(by)));

    let overall = totals.total();
    // Days are a sequence: truncating them would hide the middle of the range
    // rather than the tail, so only ranked breakdowns get limited.
    let shown = if by == Grouping::Day {
        groups.len()
    } else {
        groups.len().min(limit)
    };
    for group in groups.iter().take(shown) {
        println!(
            "{}",
            render::group_line(&group.label, &group.totals, overall)
        );
    }

    if shown < groups.len() {
        println!(
            "... and {} more (--limit {})",
            groups.len() - shown,
            groups.len()
        );
    }

    Ok(())
}

/// The column heading for a grouping.
const fn by_label(by: Grouping) -> &'static str {
    match by {
        Grouping::Project => "repository",
        Grouping::Directory => "directory",
        Grouping::Model => "model",
        Grouping::Day => "day",
    }
}

/// Prints the four counters and their sum.
fn print_totals(totals: &TokenTotals) {
    // The label is chrome and the number is the answer, so only the label is
    // dimmed. The total is the one line people came for, so only it is bold.
    let counter = |label: &str, value: i64| {
        println!(
            "  {}  {:>15}",
            theme::paint(&format!("{label:<13}"), theme::MUTED),
            render::thousands(value)
        );
    };

    counter("input", totals.input);
    counter("cache write", totals.cache_creation);
    counter("cache read", totals.cache_read);
    counter("output", totals.output);
    println!("  {}", theme::rule(30));
    println!(
        "  {}  {}",
        theme::paint(&format!("{:<13}", "total"), theme::MUTED),
        // Padded before it is bolded: widening a string that already carries
        // escape sequences counts those bytes and shortens the column.
        theme::bold(&format!("{:>15}", render::thousands(totals.total())))
    );
    counter("responses", totals.responses);
}

/// Pauses or resumes collection.
///
/// The marker file is the whole mechanism: the daemon checks it per write
/// batch, so a pause takes effect within a fifth of a second and survives a
/// restart. Both transitions are recorded, so the gap it creates explains
/// itself rather than looking like an idle agent.
fn set_paused(paths: &Paths, paused: bool) -> Result<()> {
    paths.ensure_root().context("creating the data directory")?;
    let marker = paths.pause_marker();

    if paused {
        if marker.exists() {
            println!("Already paused.");
            return Ok(());
        }
        std::fs::write(&marker, "").with_context(|| format!("creating {}", marker.display()))?;
        println!("Collection paused. Events are accepted and discarded until you resume.");
        println!("Run `agentwatch resume` to start recording again.");
    } else {
        if !marker.exists() {
            println!("Not paused.");
            return Ok(());
        }
        std::fs::remove_file(&marker).with_context(|| format!("removing {}", marker.display()))?;
        println!("Collection resumed.");
    }

    if std::os::unix::net::UnixStream::connect(paths.socket()).is_err() {
        println!("\nThe daemon is not running, so this takes effect when it starts.");
    }
    Ok(())
}

/// Installs, removes, or reports on the background jobs.
fn service_command(paths: &Paths, action: ServiceAction) -> Result<()> {
    match action {
        // Both jobs, always: someone checking why the icon is gone should not
        // have to know there is a second job or a flag to ask about it.
        ServiceAction::Status => {
            for job in service::JOBS {
                let path = service::plist_path(job);
                println!("{} ({})", job.label(), job.description());
                println!("  Definition: {}", path.display());
                println!("  Installed:  {}", if path.exists() { "yes" } else { "no" });
                println!(
                    "  Loaded:     {}",
                    if service::is_loaded(job) { "yes" } else { "no" }
                );
                if !path.exists() {
                    let flag = if job == service::Job::MenuBar {
                        " --menu-bar"
                    } else {
                        ""
                    };
                    println!("  Run `agentwatch service install{flag}` to start it at login.");
                }
            }
            Ok(())
        }

        ServiceAction::Install {
            binary,
            menu_bar,
            dry_run,
            yes,
        } => {
            let job = job_for(menu_bar);
            let path = service::plist_path(job);
            let binary = service::resolve_binary(job, binary)?;

            let definition = service::plist(job, &binary, paths.root(), &environment_overrides());

            println!("Job label:  {}", job.label());
            println!("Definition: {}\n", path.display());
            print!("{definition}");

            if dry_run {
                println!("\nDry run — nothing was written.");
                return Ok(());
            }
            if !yes && !confirm("Apply this change?")? {
                println!("\nCancelled. Nothing was written.");
                return Ok(());
            }

            service::install_job(job, &definition)?;

            println!("\nInstalled and started. It will start again at login.");
            println!("Logs: {}/{}", paths.root().display(), job.log_name());
            Ok(())
        }

        ServiceAction::Uninstall { menu_bar } => {
            let job = job_for(menu_bar);
            let path = service::plist_path(job);

            if service::is_loaded(job) {
                service::bootout(job).context("unloading the job")?;
            }
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
                println!("Removed {}.", path.display());
            } else {
                println!("Not installed.");
            }
            if job == service::Job::Daemon {
                println!(
                    "\nStored data is untouched. Delete {} to remove it.",
                    paths.root().display()
                );
            }
            Ok(())
        }
    }
}

/// Which job a `--menu-bar` flag selects.
///
/// The collector is the default so that every existing invocation, and every
/// line of documentation describing one, keeps meaning what it meant.
const fn job_for(menu_bar: bool) -> service::Job {
    if menu_bar {
        service::Job::MenuBar
    } else {
        service::Job::Daemon
    }
}

/// Environment overrides that must be carried into the launchd job.
///
/// launchd starts jobs with an empty environment, so any directory the user has
/// redirected in their shell has to be written into the job definition. Missing
/// one does not fail loudly: the service simply reads a different database, or
/// watches a settings file nobody is using.
fn environment_overrides() -> Vec<(&'static str, PathBuf)> {
    ["AGENTWATCH_DIR", "CLAUDE_CONFIG_DIR"]
        .into_iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (key, PathBuf::from(value))))
        .collect()
}

/// Adds or removes our hooks, showing the exact diff first.
fn install_hooks(
    binary: Option<PathBuf>,
    settings: Option<PathBuf>,
    uninstall: bool,
    assume_yes: bool,
    dry_run: bool,
) -> Result<()> {
    let path = settings.unwrap_or_else(install::file::default_settings_path);
    let current = install::file::read(&path)?;

    let (updated, change) = if uninstall {
        install::plan_uninstall(&current)
    } else {
        let executable = install::file::resolve_executable(binary)?;
        install::plan_install(&current, &agentwatch_types::hook_command(&executable))
    };

    let action = if uninstall { "remove" } else { "install" };
    println!("Settings file: {}", path.display());

    if change.is_empty() {
        println!(
            "\nNothing to {action}. {}",
            if uninstall {
                "Our hooks are not present."
            } else {
                "Our hooks are already installed."
            }
        );
        return Ok(());
    }

    let before = format!("{}\n", serde_json::to_string_pretty(&current)?);
    let after = format!("{}\n", serde_json::to_string_pretty(&updated)?);

    let entries = |count: usize| if count == 1 { "entry" } else { "entries" };
    if uninstall {
        println!(
            "\nThis would remove {} hook {}:\n",
            change.removed,
            entries(change.removed)
        );
    } else {
        // Said separately, because they are different acts: one adds a hook
        // where there was none, the other repoints one that was already there
        // — which is how an upgrade from 0.1 looks, and worth seeing named.
        let mut parts = Vec::new();
        if change.added > 0 {
            parts.push(format!("add {} {}", change.added, entries(change.added)));
        }
        if change.updated > 0 {
            parts.push(format!(
                "repoint {} existing {}",
                change.updated,
                entries(change.updated)
            ));
        }
        println!("\nThis would {}:\n", parts.join(" and "));
    }
    print!("{}", install::unified_diff(&before, &after));

    if dry_run {
        println!("\nDry run — nothing was written.");
        return Ok(());
    }

    if !assume_yes && !confirm("Apply this change?")? {
        println!("\nCancelled. Nothing was written.");
        return Ok(());
    }

    let backup = install::file::write(&path, &updated)?;
    println!("\nWrote {}.", path.display());
    if let Some(backup) = backup {
        println!("Previous version saved to {}.", backup.display());
    }
    if !uninstall {
        println!(
            "\nStart the collector with `agentwatch service install`, then open a new session."
        );
    }
    Ok(())
}

/// Asks the user to approve a write.
///
/// A non-interactive run without `--yes` declines rather than proceeding: this
/// edits the configuration of the agent being monitored, and a pipe is not
/// consent.
fn confirm(question: &str) -> Result<bool> {
    use std::io::{IsTerminal as _, Write as _};

    if !std::io::stdin().is_terminal() {
        println!("\nNot a terminal. Re-run with --yes to write, or --dry-run to inspect.");
        return Ok(false);
    }

    print!("\n{question} [y/N] ");
    std::io::stdout().flush().context("prompting")?;

    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("reading your answer")?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Lists sessions with their counts.
fn sessions(paths: &Paths, days: u32, limit: u32, show_coverage: bool) -> Result<()> {
    let store = open_for_reading(paths)?;
    let (offset, _) = range::local_offset();
    let range = range::last_days(days, offset);

    let rows = store
        .sessions(range.from_us, range.to_us, limit)
        .context("reading sessions")?;

    println!("Sessions — {}", range.label);
    if rows.is_empty() {
        println!("\nNone.");
        return Ok(());
    }

    println!();
    println!("{}", render::session_header());
    for row in &rows {
        println!("{}", render::session_line(row));
    }

    if !show_coverage {
        return Ok(());
    }

    for row in &rows {
        let coverage = store.coverage(&row.id).context("reading coverage")?;
        println!();
        println!(
            "{}  {}",
            &row.id[..8],
            row.project.as_deref().unwrap_or("(no project)")
        );
        print_coverage(&coverage);
    }

    Ok(())
}

/// Prints what a session's data can answer.
///
/// `disabled` and `not collected` are distinct from `no`: one means the data
/// was never gathered, the other that it was gathered and there was none.
fn print_coverage(coverage: &Coverage) {
    let observed = |seen: bool| if seen { "yes" } else { "none seen" };

    println!("  tokens         {}", observed(coverage.tokens));
    println!("  session start  {}", observed(coverage.session_bounds));
    println!("  tools          {}", observed(coverage.tools));
    println!("  commands       {}", observed(coverage.commands));
    println!("  files          {}", observed(coverage.files));
    println!("  mcp            {}", observed(coverage.mcp));
    println!("  network        not collected");
    println!("  processes      not collected");
    println!("  prompt content disabled");
}

/// Prints a timeline of events.
fn activity(paths: &Paths, days: u32, limit: u32, filter: ActivityFilter) -> Result<()> {
    let store = open_for_reading(paths)?;
    let (offset, _) = range::local_offset();
    let range = range::last_days(days, offset);

    let rows = store
        .activity(range.from_us, range.to_us, &filter, limit)
        .context("reading activity")?;

    println!("Activity — {}", range.label);
    if rows.is_empty() {
        println!("\nNothing recorded.");
        return Ok(());
    }

    println!();
    println!("{}", render::header());
    for row in &rows {
        println!("{}", render::event_line_painted(row));
    }
    Ok(())
}

/// Lists access to sensitive paths.
fn security(paths: &Paths, days: u32, limit: u32) -> Result<()> {
    let store = open_for_reading(paths)?;
    let (offset, _) = range::local_offset();
    let range = range::last_days(days, offset);

    let rows = store
        .notable_access(range.from_us, range.to_us, limit)
        .context("reading sensitive access")?;

    println!("Sensitive access — {}", range.label);

    if rows.is_empty() {
        println!("\nNothing above normal.");
        println!();
        println!("{}", render::SECURITY_CAVEAT);
        return Ok(());
    }

    println!();
    println!("{}", render::notable_header());
    for row in &rows {
        println!("{}", render::notable_line(row));
    }
    println!();
    println!("{}", render::SECURITY_CAVEAT);
    Ok(())
}

/// Writes events to stdout as JSON Lines.
fn export(paths: &Paths, days: u32, limit: u32, kinds: Vec<String>) -> Result<()> {
    use std::io::Write as _;

    let store = open_for_reading(paths)?;
    let (offset, _) = range::local_offset();
    let range = range::last_days(days, offset);

    let filter = ActivityFilter {
        kinds,
        ..ActivityFilter::default()
    };
    let rows = store
        .activity(range.from_us, range.to_us, &filter, limit)
        .context("reading events")?;

    // Locked and buffered: an export is the one command likely to be piped into
    // something, and per-line locking would dominate its runtime.
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    for row in &rows {
        let payload: serde_json::Value = serde_json::from_str(&row.payload)
            .unwrap_or_else(|_| serde_json::Value::String(row.payload.clone()));
        let record = serde_json::json!({
            "timestamp": agentwatch_types::Timestamp::from_micros(row.timestamp_us).to_rfc3339(),
            "agent": row.agent_id,
            "kind": row.kind,
            "evidence": row.evidence,
            "project": row.project_path,
            "event": payload,
        });
        writeln!(out, "{record}").context("writing to stdout")?;
    }

    out.flush().context("flushing stdout")?;
    Ok(())
}

/// Imports historical usage from transcripts.
fn import(paths: &Paths, limit: Option<usize>) -> Result<()> {
    paths.ensure_root().context("creating the data directory")?;
    let mut store = Store::open(paths.database()).context("opening the database")?;

    let report = sync::import(&mut store, limit)?;
    let repositories = store
        .backfill_repositories(&mut agentwatch_types::RepositoryResolver::new())
        .context("resolving repositories")?;

    println!("transcripts read     {}", report.files);
    if report.unreadable > 0 {
        println!("unreadable           {}", report.unreadable);
    }
    println!(
        "assistant records    {}",
        render::thousands(report.records as i64)
    );
    println!(
        "distinct responses   {}",
        render::thousands(report.responses as i64)
    );
    println!(
        "rows written         {}",
        render::thousands(report.written as i64)
    );
    println!(
        "repositories         {} (from {} directories, {} outside any repository)",
        repositories.repositories, repositories.projects, repositories.unresolved
    );
    println!();
    println!(
        "Counting records rather than responses would have inflated every total by {:.2}x.",
        report.record_inflation()
    );

    Ok(())
}

/// Re-derives totals from the transcripts and reports drift.
fn verify(paths: &Paths) -> Result<()> {
    let store = open_for_reading(paths)?;
    let drift = sync::verify(&store)?;

    println!("{:<22} {:>16} {:>16}", "", "transcripts", "stored");
    println!(
        "{:<22} {:>16} {:>16}",
        "responses",
        render::thousands(drift.transcript_responses),
        render::thousands(drift.stored_responses)
    );
    println!(
        "{:<22} {:>16} {:>16}",
        "total tokens",
        render::thousands(drift.transcript_tokens),
        render::thousands(drift.stored_tokens)
    );
    println!();

    if drift.is_clean() {
        println!("No drift. Storage matches the transcripts exactly.");
        return Ok(());
    }

    let responses = drift.transcript_responses - drift.stored_responses;
    let tokens = drift.transcript_tokens - drift.stored_tokens;

    // Drift has two directions and they mean opposite things. Positive is
    // transcripts we have not read yet, which importing fixes. Negative is
    // storage holding responses the transcripts no longer contain — which is
    // what a compacted or deleted transcript looks like, and telling someone to
    // import to "read what is missing" in that case sends them to re-read a
    // file that will never mention it again.
    if responses < 0 || tokens < 0 {
        println!(
            "{}",
            theme::paint(
                &format!(
                    "AHEAD: storage holds {} responses and {} tokens the transcripts no longer contain.",
                    render::thousands(-responses),
                    render::thousands(-tokens)
                ),
                theme::WARN
            )
        );
        println!("Normal after a session's transcript is compacted or deleted: the database keeps");
        println!("what it recorded at the time. Nothing to do, and nothing is lost.");
        return Ok(());
    }

    println!(
        "{}",
        theme::paint(
            &format!(
                "DRIFT: {} responses, {} tokens not yet read.",
                render::thousands(responses),
                render::thousands(tokens)
            ),
            theme::WARN
        )
    );
    println!("Run `agentwatch import` to read what is missing.");
    Ok(())
}

/// Opens the database for reading, with a useful message when it is absent.
pub(crate) fn open_for_reading(paths: &Paths) -> Result<Store> {
    anyhow::ensure!(
        paths.database().exists(),
        "no database yet at {}\nRun `agentwatch init` to set up, or `agentwatch import` to read your history.",
        paths.database().display()
    );
    Store::open_read_only(paths.database()).context("opening the database")
}

/// Prints the most recent events.
fn events(paths: &Paths, limit: u32) -> Result<()> {
    if !paths.database().exists() {
        println!("No database yet. Run `agentwatch init` to set up.");
        return Ok(());
    }

    let store = Store::open_read_only(paths.database()).context("opening the database")?;
    let rows = store.recent_events(limit).context("reading events")?;

    if rows.is_empty() {
        println!("No events recorded yet.");
        return Ok(());
    }

    // Times are UTC and labelled as such. Rendering them in the user's zone
    // needs the calendar handling that phase 3 owns, and a clock that silently
    // shows the wrong hour is worse than one that says which hour it means.
    println!("{}", render::header());
    for row in rows.iter().rev() {
        println!("{}", render::event_line_painted(row));
    }

    Ok(())
}
