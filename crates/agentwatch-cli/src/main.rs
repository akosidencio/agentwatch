//! The AgentWatch CLI.
//!
//! Phase 1 is deliberately thin: enough to prove the pipeline works and to wire
//! the hooks up. The analytics commands arrive in phase 3.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod hook_config;
mod install;
mod range;
mod render;
mod service;
mod sync;
mod watch;

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
    #[command(subcommand)]
    command: Command,
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
        /// Path to the daemon binary.
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Show the job definition and exit without writing.
        #[arg(long)]
        dry_run: bool,
        /// Write without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Stop and remove the LaunchAgent.
    Uninstall,
    /// Report whether the service is installed and loaded.
    Status,
}

/// The available commands.
#[derive(Debug, Subcommand)]
enum Command {
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
        /// Path to the hook binary to register.
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
    /// Print the settings needed to enable monitoring.
    ///
    /// Prints only. Nothing is written to your Claude Code configuration; copy
    /// the output yourself, or wait for `install-hooks` in phase 4.
    HookConfig {
        /// Path to the hook binary to reference.
        #[arg(long)]
        binary: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env().context("resolving the data directory")?;

    match cli.command {
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
        Command::HookConfig { binary } => {
            print!("{}", hook_config::snippet(binary.as_deref()));
            Ok(())
        }
    }
}

/// Prints daemon liveness and headline counts.
fn status(paths: &Paths) -> Result<()> {
    let socket = paths.socket();
    let running = std::os::unix::net::UnixStream::connect(&socket).is_ok();

    println!(
        "daemon    {}",
        if running { "running" } else { "not running" }
    );
    println!("socket    {}", socket.display());
    if paths.is_paused() {
        println!("collection PAUSED — run `agentwatch resume` to record again");
    }
    println!("database  {}", paths.database().display());

    if !paths.database().exists() {
        println!("\nNo database yet. Start the daemon with `agentwatch-daemon`.");
        return Ok(());
    }

    let store = Store::open_read_only(paths.database()).context("opening the database")?;
    let totals = store.totals().context("reading totals")?;

    println!();
    println!("events            {}", totals.events);
    println!("sessions          {}", totals.sessions);
    println!("active sessions   {}", totals.active_sessions);
    println!("projects          {}", totals.projects);

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

    println!("Token usage — {}", range.label);
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
    println!("  input          {:>15}", render::thousands(totals.input));
    println!(
        "  cache write    {:>15}",
        render::thousands(totals.cache_creation)
    );
    println!(
        "  cache read     {:>15}",
        render::thousands(totals.cache_read)
    );
    println!("  output         {:>15}", render::thousands(totals.output));
    println!("  {:-<30}", "");
    println!("  total          {:>15}", render::thousands(totals.total()));
    println!(
        "  responses      {:>15}",
        render::thousands(totals.responses)
    );
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

/// Installs, removes, or reports on the background service.
fn service_command(paths: &Paths, action: ServiceAction) -> Result<()> {
    let path = service::plist_path();

    match action {
        ServiceAction::Status => {
            println!("Job label:  {}", service::LABEL);
            println!("Definition: {}", path.display());
            println!("Installed:  {}", if path.exists() { "yes" } else { "no" });
            println!(
                "Loaded:     {}",
                if service::is_loaded() { "yes" } else { "no" }
            );
            if !path.exists() {
                println!("\nRun `agentwatch service install` to start it at login.");
            }
            Ok(())
        }

        ServiceAction::Install {
            binary,
            dry_run,
            yes,
        } => {
            let binary = binary.unwrap_or_else(service::default_daemon_binary);
            if !binary.is_file() {
                anyhow::bail!(
                    "no daemon binary at {}\n\
                     Build it with `cargo build --release`, or pass --binary <path>.",
                    binary.display()
                );
            }
            let binary = binary
                .canonicalize()
                .with_context(|| format!("resolving {}", binary.display()))?;

            let definition = service::plist(&binary, paths.root(), &environment_overrides());

            println!("Job label:  {}", service::LABEL);
            println!("Definition: {}\n", path.display());
            print!("{definition}");

            if dry_run {
                println!("\nDry run — nothing was written.");
                return Ok(());
            }
            if !yes && !confirm()? {
                println!("\nCancelled. Nothing was written.");
                return Ok(());
            }

            let directory = path.parent().unwrap_or(std::path::Path::new("."));
            std::fs::create_dir_all(directory)
                .with_context(|| format!("creating {}", directory.display()))?;

            // Replacing a loaded job without unloading it first leaves launchd
            // running the old binary, so an upgrade would silently do nothing.
            if service::is_loaded() {
                service::bootout().context("unloading the previous job")?;
            }

            std::fs::write(&path, definition)
                .with_context(|| format!("writing {}", path.display()))?;
            service::bootstrap(&path).context("loading the job")?;

            println!("\nInstalled and started. It will start again at login.");
            println!("Logs: {}/daemon.log", paths.root().display());
            Ok(())
        }

        ServiceAction::Uninstall => {
            if service::is_loaded() {
                service::bootout().context("unloading the job")?;
            }
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
                println!("Removed {}.", path.display());
            } else {
                println!("Not installed.");
            }
            println!(
                "\nStored data is untouched. Delete {} to remove it.",
                paths.root().display()
            );
            Ok(())
        }
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
        let binary = binary.unwrap_or_else(install::file::default_hook_binary);

        // A hook pointing at a missing binary fails silently: the agent ignores
        // the exit code, so monitoring would simply never start and nothing
        // would say why.
        if !binary.is_file() {
            anyhow::bail!(
                "no hook binary at {}\n\
                 Build it with `cargo build --release`, or pass --binary <path>.",
                binary.display()
            );
        }

        // Hooks run with the project directory as cwd, so a relative path
        // would resolve somewhere else entirely — or nowhere.
        let binary = binary
            .canonicalize()
            .with_context(|| format!("resolving {}", binary.display()))?;

        install::plan_install(&current, &binary.to_string_lossy())
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

    println!(
        "\nThis would {action} {} hook {}:\n",
        if uninstall {
            change.removed
        } else {
            change.added
        },
        if (if uninstall {
            change.removed
        } else {
            change.added
        }) == 1
        {
            "entry"
        } else {
            "entries"
        }
    );
    print!("{}", install::unified_diff(&before, &after));

    if dry_run {
        println!("\nDry run — nothing was written.");
        return Ok(());
    }

    if !assume_yes && !confirm()? {
        println!("\nCancelled. Nothing was written.");
        return Ok(());
    }

    let backup = install::file::write(&path, &updated)?;
    println!("\nWrote {}.", path.display());
    if let Some(backup) = backup {
        println!("Previous version saved to {}.", backup.display());
    }
    if !uninstall {
        println!("\nStart the daemon with `agentwatch-daemon`, then open a new session.");
    }
    Ok(())
}

/// Asks the user to approve a write.
///
/// A non-interactive run without `--yes` declines rather than proceeding: this
/// edits the configuration of the agent being monitored, and a pipe is not
/// consent.
fn confirm() -> Result<bool> {
    use std::io::{IsTerminal as _, Write as _};

    if !std::io::stdin().is_terminal() {
        println!("\nNot a terminal. Re-run with --yes to write, or --dry-run to inspect.");
        return Ok(false);
    }

    print!("\nApply this change? [y/N] ");
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
        println!("{}", render::event_line(row));
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

    println!(
        "DRIFT: {} responses, {} tokens.",
        render::thousands(drift.transcript_responses - drift.stored_responses),
        render::thousands(drift.transcript_tokens - drift.stored_tokens)
    );
    println!("Run `agentwatch import` to read what is missing.");
    Ok(())
}

/// Opens the database for reading, with a useful message when it is absent.
pub(crate) fn open_for_reading(paths: &Paths) -> Result<Store> {
    anyhow::ensure!(
        paths.database().exists(),
        "no database yet at {}\nStart the daemon with `agentwatch-daemon`, or run `agentwatch import`.",
        paths.database().display()
    );
    Store::open_read_only(paths.database()).context("opening the database")
}

/// Prints the most recent events.
fn events(paths: &Paths, limit: u32) -> Result<()> {
    if !paths.database().exists() {
        println!("No database yet. Start the daemon with `agentwatch-daemon`.");
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
        println!("{}", render::event_line(row));
    }

    Ok(())
}
