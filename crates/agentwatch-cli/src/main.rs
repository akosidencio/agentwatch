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

use agentwatch_storage::{
    ActivityFilter, Coverage, SessionFilter, Store, TokenDetail, TokenGroup, TokenTotals,
};
use agentwatch_types::{Paths, Timestamp};
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
    /// By coding agent.
    Agent,
    /// By the provider that served the request.
    Provider,
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
        /// Skip reading the history supported agents have already written.
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
        /// Also show reasoning tokens, the cache-write TTL split, and server tools.
        #[arg(long)]
        detail: bool,
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
        /// Restrict to agents, for example `claude,codex`.
        #[arg(long, value_delimiter = ',')]
        agent: Vec<String>,
    },
    /// Show one session receipt (`latest` or an id prefix).
    Session {
        /// Session id, unique prefix, or `latest`.
        #[arg(default_value = "latest")]
        id: String,
    },
    /// Compare usage across agents, models, or projects.
    Compare {
        /// Dimension to compare.
        #[arg(long, value_enum, default_value_t = Grouping::Agent)]
        by: Grouping,
        /// Number of calendar days to include, ending today.
        #[arg(long, default_value_t = 30)]
        days: u32,
        /// Show at most this many rows.
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
    /// List the files the agents rewrote most.
    Churn {
        /// Number of calendar days to include, ending today.
        #[arg(long, default_value_t = 30)]
        days: u32,
        /// Ignore dates and count everything ever recorded.
        #[arg(long)]
        all: bool,
        /// Show at most this many files.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Run a read-only SQL query against the collected data.
    ///
    /// The connection is opened `query_only`, so a write is refused by SQLite
    /// itself. Tables: events, sessions, projects, repositories, token_usage,
    /// file_events, command_events, mcp_events, tool_outcomes.
    Sql {
        /// The query. Reads from stdin when omitted.
        query: Option<String>,
        /// Emit one JSON object per row instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Report how often each tool fails, and how long it takes.
    Reliability {
        /// Number of calendar days to include, ending today.
        #[arg(long, default_value_t = 30)]
        days: u32,
        /// Ignore dates and count everything ever recorded.
        #[arg(long)]
        all: bool,
        /// Show at most this many tools.
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
    /// Read historical telemetry from supported agents' durable local logs.
    ///
    /// Safe to run repeatedly: nothing is double counted.
    Import {
        /// Read at most this many transcript files.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Redact secrets from command lines that are already stored.
    ///
    /// Uses the same built-in and custom rules as new event collection. The
    /// operation is transactional and safe to run repeatedly.
    Scrub {
        /// Report how many commands need changes without writing them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-derive totals from agent logs and report any disagreement.
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
            detail,
        } => tokens(
            &paths,
            by,
            days,
            from.as_deref(),
            to.as_deref(),
            all,
            limit,
            detail,
        ),
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
            agent,
        } => sessions(&paths, days, limit, coverage, agent),
        Command::Session { id } => session(&paths, &id),
        Command::Compare { by, days, limit } => {
            tokens(&paths, by, days, None, None, false, limit, false)
        }
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
                include_subagents: false,
                project_prefix: project,
                kinds: kind,
            },
        ),
        Command::Churn { days, all, limit } => churn(&paths, days, all, limit),
        Command::Sql { query, json } => sql(&paths, query.as_deref(), json),
        Command::Reliability { days, all, limit } => reliability(&paths, days, all, limit),
        Command::Security { days, limit } => security(&paths, days, limit),
        Command::Export { days, limit, kind } => export(&paths, days, limit, kind),
        Command::Import { limit } => import(&paths, limit),
        Command::Scrub { dry_run } => scrub(&paths, dry_run),
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

    print_unknown_events(&store, totals.unknown_events)?;

    Ok(())
}

/// Warns when the adapter has stopped understanding what it is being sent.
///
/// Silent only when there is nothing to say. Every other counter in `status` is
/// a measure of work observed; this one is a measure of work *missed*, and it
/// is the difference between a monitor that is healthy and one that merely
/// looks it — an unrecognised payload is still recorded, still counted in
/// `events`, and still missing the file path or command line it arrived with.
fn print_unknown_events(store: &Store, unknown: i64) -> Result<()> {
    if unknown == 0 {
        return Ok(());
    }

    let labels = store
        .unknown_event_labels(4)
        .context("reading unrecognised event labels")?;
    let named = labels
        .iter()
        .map(|(label, count)| format!("{label} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");

    // The glyph and the wording carry the state on their own, so the warning
    // still reads as a warning with colour stripped or piped away.
    println!();
    println!(
        "{}",
        theme::paint(
            &format!("⚠ {unknown} events not understood — {named}"),
            theme::WARN
        )
    );
    println!(
        "{}",
        theme::paint(
            "  Their detail was dropped at collection. Upgrade, or report these names.",
            theme::MUTED
        )
    );

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
    detail: bool,
) -> Result<()> {
    let store = open_for_reading(paths)?;
    let (zone, zone_is_local) = range::local_zone();

    let range = match (all, from, to) {
        (true, _, _) => range::all_time(zone),
        (_, Some(from), Some(to)) => range::explicit(from, to, zone)?,
        _ => range::last_days(days, zone),
    };

    let totals = store
        .token_totals(range.from_us, range.to_us)
        .context("reading totals")?;

    println!("{}", theme::bold(&format!("Token usage — {}", range.label)));
    if !zone_is_local {
        println!("(times in UTC: the local timezone could not be determined)");
    }
    println!();

    let providers = store
        .tokens_by_provider(range.from_us, range.to_us)
        .context("reading the provider split")?;
    let detail = if detail {
        store
            .token_detail_by_provider(range.from_us, range.to_us)
            .context("reading the provider detail")?
    } else {
        Vec::new()
    };
    print_provider_totals(&providers, &totals, &detail);

    let groups = match by {
        Grouping::Agent => store.tokens_by_agent(range.from_us, range.to_us),
        Grouping::Provider => store.tokens_by_provider(range.from_us, range.to_us),
        Grouping::Project => store.tokens_by_repository(range.from_us, range.to_us),
        Grouping::Directory => store.tokens_by_project(range.from_us, range.to_us),
        Grouping::Model => store.tokens_by_model(range.from_us, range.to_us),
        Grouping::Day => store.tokens_by_day(range.from_us, range.to_us, |timestamp| {
            range.day_label(timestamp)
        }),
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

/// Prints the files that were rewritten most.
fn churn(paths: &Paths, days: u32, all: bool, limit: usize) -> Result<()> {
    let store = open_for_reading(paths)?;
    let (zone, _) = range::local_zone();
    let range = if all {
        range::all_time(zone)
    } else {
        range::last_days(days, zone)
    };

    let churn = store
        .file_churn(range.from_us, range.to_us, limit)
        .context("reading file churn")?;

    println!("{}", theme::bold(&format!("File churn — {}", range.label)));
    println!();

    if churn.is_empty() {
        println!("No file writes in range.");
        return Ok(());
    }

    println!(
        "{}",
        theme::paint(
            &format!(
                "{:>7}{:>8}{:>10}  {}",
                "writes", "reads", "sessions", "file"
            ),
            theme::MUTED
        )
    );

    for file in &churn {
        println!(
            "{:>7}{:>8}{:>10}  {}",
            file.writes,
            file.reads,
            file.sessions,
            render::short_path(&file.path, std::env::var("HOME").ok().as_deref()),
        );
    }

    Ok(())
}

/// Runs a read-only query and prints the result.
fn sql(paths: &Paths, query: Option<&str>, as_json: bool) -> Result<()> {
    let query = match query {
        Some(query) => query.to_owned(),
        None => {
            use std::io::Read as _;
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .context("reading the query from stdin")?;
            buffer
        }
    };
    anyhow::ensure!(!query.trim().is_empty(), "no query given");

    let store = open_for_reading(paths)?;
    let result = store.query(&query).context("running the query")?;

    if as_json {
        for row in &result.rows {
            let object: serde_json::Map<String, serde_json::Value> = result
                .columns
                .iter()
                .zip(row)
                .map(|(column, value)| {
                    let value = value.as_ref().map_or(serde_json::Value::Null, |text| {
                        serde_json::Value::String(text.clone())
                    });
                    (column.clone(), value)
                })
                .collect();
            println!("{}", serde_json::Value::Object(object));
        }
        return Ok(());
    }

    if result.rows.is_empty() {
        println!("No rows.");
        return Ok(());
    }

    // Column widths come from the data, so a narrow result does not print a
    // table padded out to nothing. NULL is shown as a dash rather than an
    // empty cell, which is indistinguishable from an empty string.
    let cell = |value: &Option<String>| value.clone().unwrap_or_else(|| "—".to_owned());
    let mut widths: Vec<usize> = result.columns.iter().map(|c| c.chars().count()).collect();
    for row in &result.rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell(value).chars().count());
        }
    }

    let line = |cells: Vec<String>| {
        cells
            .iter()
            .zip(&widths)
            .map(|(text, width)| format!("{text:<width$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };

    // Padded before painting: escape bytes count as width otherwise.
    println!(
        "{}",
        theme::paint(&line(result.columns.clone()), theme::MUTED)
    );
    for row in &result.rows {
        println!("{}", line(row.iter().map(cell).collect()));
    }
    println!();
    println!(
        "{}",
        theme::paint(&format!("{} rows", result.rows.len()), theme::MUTED)
    );

    Ok(())
}

/// Prints how each tool has behaved: how often it fails and how long it takes.
fn reliability(paths: &Paths, days: u32, all: bool, limit: usize) -> Result<()> {
    let store = open_for_reading(paths)?;
    let (zone, _) = range::local_zone();
    let range = if all {
        range::all_time(zone)
    } else {
        range::last_days(days, zone)
    };

    let report = store
        .tool_reliability(range.from_us, range.to_us)
        .context("reading tool reliability")?;

    println!(
        "{}",
        theme::bold(&format!("Tool reliability — {}", range.label))
    );
    println!();

    if report.is_empty() {
        println!(
            "No completed tool calls in range. Outcomes are read from transcripts, so run \
             `agentwatch import` if this is a fresh database."
        );
        return Ok(());
    }

    println!(
        "{}",
        theme::paint(
            &format!(
                "{:<16}{:>8}{:>9}{:>10}{:>10}{:>10}",
                "tool", "calls", "failed", "p50", "p95", "max"
            ),
            theme::MUTED
        )
    );

    for tool in report.iter().take(limit) {
        let rate = tool.failure_rate();
        // A failure rate is the one column worth colouring: it is the only one
        // where a number is a problem rather than just a fact. Zero stays
        // unpainted so a healthy table is quiet.
        let failed = if tool.failures == 0 {
            format!("{:>8}", "—")
        } else {
            let text = format!("{:>7.1}%", rate);
            let colour = if rate >= 10.0 {
                theme::BAD
            } else {
                theme::WARN
            };
            // Padded before painting: escape bytes count as width otherwise.
            format!("{}{}", " ", theme::paint(&text, colour))
        };

        println!(
            "{:<16}{:>8}{}{:>10}{:>10}{:>10}",
            tool.tool,
            render::thousands(tool.calls),
            failed,
            duration(tool.p50_ms),
            duration(tool.p95_ms),
            duration(tool.max_ms),
        );
    }

    if report.len() > limit {
        println!(
            "... and {} more (--limit {})",
            report.len() - limit,
            report.len()
        );
    }

    // Said once, under the table, because the `max` column invites exactly the
    // wrong conclusion otherwise: durations are wall-clock between the call and
    // its result, so a tool that sat waiting for your approval reads as slow.
    println!();
    println!(
        "{}",
        theme::paint(
            "Durations are wall-clock, so a call awaiting approval or a resumed session \
             inflates max. Read p50 and p95.",
            theme::MUTED
        )
    );

    Ok(())
}

/// Renders a millisecond duration, or a dash when it was never measured.
///
/// A missing duration is not a zero one, so it must not print as `0ms`.
fn duration(ms: Option<i64>) -> String {
    match ms {
        None => "—".to_owned(),
        Some(ms) if ms < 1_000 => format!("{ms}ms"),
        Some(ms) if ms < 60_000 => format!("{:.1}s", ms as f64 / 1_000.0),
        Some(ms) => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1_000),
    }
}

/// The column heading for a grouping.
const fn by_label(by: Grouping) -> &'static str {
    match by {
        Grouping::Agent => "agent",
        Grouping::Provider => "provider",
        Grouping::Project => "repository",
        Grouping::Directory => "directory",
        Grouping::Model => "model",
        Grouping::Day => "day",
    }
}

/// Prints one counter block per provider, then what may honestly be combined.
///
/// # Why the headline is not a single block
///
/// The four counters mean different things to different providers. Anthropic
/// serves nearly all of its input from the prompt cache and reports it under
/// `cache read`; OpenAI reports no cache *writes* at all. Summed, the cache
/// write line silently describes one provider, and the input line adds two
/// quantities that were not measured the same way — by tokenizers that do not
/// agree on what a token is.
///
/// So the per-provider blocks are the answer, and the combined footer totals
/// only what survives being added up: how many responses there were. The token
/// sum is still printed, because refusing to show it would be its own kind of
/// dishonesty, but it is labelled for what it is.
///
/// A single provider gets no footer — there is nothing to combine, and the
/// caveat would be noise.
fn print_provider_totals(
    providers: &[TokenGroup],
    overall: &TokenTotals,
    detail: &[(String, TokenDetail)],
) {
    let detail_for = |label: &str| {
        detail
            .iter()
            .find(|(provider, _)| provider == label)
            .map(|(_, found)| found)
    };

    if providers.len() <= 1 {
        print_totals(overall);
        if let Some(only) = providers.first()
            && let Some(found) = detail_for(&only.label)
        {
            print_detail(found, overall);
        }
        return;
    }

    for provider in providers {
        println!("  {}", theme::bold(&provider.label));
        print_totals(&provider.totals);
        if let Some(found) = detail_for(&provider.label) {
            print_detail(found, &provider.totals);
        }
        println!();
    }

    println!("  {}", theme::bold("all providers"));
    println!(
        "  {}  {:>15}",
        theme::paint(&format!("{:<13}", "responses"), theme::MUTED),
        render::thousands(overall.responses)
    );
    println!(
        "  {}  {:>15}",
        theme::paint(&format!("{:<13}", "tokens"), theme::MUTED),
        render::thousands(overall.total())
    );
    println!(
        "  {}",
        theme::paint(
            "tokens above are summed across different tokenizers — compare per provider",
            theme::MUTED
        )
    );
}

/// Prints the counters the provider reported beyond the headline four.
///
/// Silent when the provider reported none of them, rather than printing a
/// column of zeroes for a provider that does not have the concept. Percentages
/// are shown against the figure they are a subset of — reasoning against
/// output, each cache tier against total cache write — because the absolute
/// number alone does not say whether it is worth acting on.
fn print_detail(detail: &TokenDetail, totals: &TokenTotals) {
    if detail.is_empty() {
        return;
    }

    println!("  {}", theme::paint("detail", theme::MUTED));

    // Padded and right-aligned before any colour is applied: widening a string
    // that already carries escape sequences counts those bytes as width.
    let line = |label: &str, value: i64, note: &str| {
        println!(
            "  {}  {:>15}{}",
            theme::paint(&format!("{label:<13}"), theme::MUTED),
            render::thousands(value),
            theme::paint(note, theme::MUTED)
        );
    };

    if let Some(share) = detail.reasoning_share().filter(|_| detail.reasoning > 0) {
        line(
            "reasoning",
            detail.reasoning,
            &format!("   {share:.1}% of output"),
        );
    }

    // The TTL split only means something when something was written, and the
    // two tiers are priced differently — which is the whole reason to show
    // them apart rather than folded into the single cache-write figure above.
    let cache_write = detail.cache_write();
    if cache_write > 0 {
        for (label, value) in [
            ("cache 5m", detail.cache_write_5m),
            ("cache 1h", detail.cache_write_1h),
        ] {
            let share = value as f64 * 100.0 / cache_write as f64;
            line(label, value, &format!("   {share:.1}% of cache write"));
        }
    }

    if detail.web_search_requests > 0 {
        line("web search", detail.web_search_requests, "");
    }
    if detail.web_fetch_requests > 0 {
        line("web fetch", detail.web_fetch_requests, "");
    }

    // Shown as a share of responses because the absolute count says nothing on
    // its own: ten misses in ten thousand responses is noise, ten in fifty is
    // the reason the bill moved.
    if detail.cache_misses > 0 {
        let note = totals.responses.gt(&0).then(|| {
            format!(
                "   {:.1}% of responses",
                detail.cache_misses as f64 * 100.0 / totals.responses as f64
            )
        });
        line(
            "cache misses",
            detail.cache_misses,
            note.as_deref().unwrap_or(""),
        );
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
fn sessions(
    paths: &Paths,
    days: u32,
    limit: u32,
    show_coverage: bool,
    agents: Vec<String>,
) -> Result<()> {
    let store = open_for_reading(paths)?;
    let (zone, _) = range::local_zone();
    let range = range::last_days(days, zone);

    let agents = agents
        .into_iter()
        .map(|agent| match agent.as_str() {
            "claude" | "claude-code" => "claude-code".to_owned(),
            "codex" => "codex".to_owned(),
            "copilot" | "github-copilot" => "github-copilot".to_owned(),
            _ => agent,
        })
        .collect();
    let rows = store
        .sessions_filtered(range.from_us, range.to_us, limit, &SessionFilter { agents })
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

/// Prints a complete receipt for one session.
fn session(paths: &Paths, reference: &str) -> Result<()> {
    let store = open_for_reading(paths)?;
    let candidates = store
        .sessions(
            0,
            i64::MAX,
            if reference == "latest" { 100 } else { 10_000 },
        )
        .context("reading sessions")?;

    let matches: Vec<_> = if reference == "latest" {
        let selected = candidates
            .iter()
            .position(|row| !row.is_subagent)
            .unwrap_or(0);
        candidates.into_iter().nth(selected).into_iter().collect()
    } else {
        candidates
            .into_iter()
            .filter(|row| row.id.starts_with(reference))
            .collect()
    };
    anyhow::ensure!(!matches.is_empty(), "no session matches `{reference}`");
    anyhow::ensure!(
        matches.len() == 1,
        "session prefix `{reference}` is ambiguous"
    );
    let row = &matches[0];

    // Query every section before printing. If the database is damaged or from
    // an incompatible build, the command fails cleanly instead of leaving a
    // plausible-looking partial receipt on stdout.
    let projects = store
        .projects_for_session(&row.id)
        .context("reading session projects")?;
    let tokens = store
        .receipt_tokens(&row.id)
        .context("reading session token breakdown")?;
    let files = store
        .receipt_files(&row.id)
        .context("reading session files")?;
    let commands = store
        .receipt_commands(&row.id)
        .context("reading session commands")?;
    let notable = store
        .receipt_notable_access(&row.id)
        .context("reading session sensitive access")?;
    let timeline = store
        .activity(
            0,
            i64::MAX,
            &ActivityFilter {
                session: Some(row.id.clone()),
                include_subagents: true,
                ..ActivityFilter::default()
            },
            u32::MAX,
        )
        .context("reading session timeline")?;
    let coverage = store.coverage(&row.id).context("reading coverage")?;

    let started = row.started_at_us.map_or_else(
        || "unknown".to_owned(),
        |micros| Timestamp::from_micros(micros).to_rfc3339(),
    );
    let duration_ms = row.duration_ms.or_else(|| {
        (row.status == "active").then(|| {
            row.started_at_us
                .map(|started| (Timestamp::now().as_micros() - started).max(0) / 1_000)
        })?
    });
    let duration = duration_ms.map_or_else(
        || "unknown".to_owned(),
        |milliseconds| {
            let formatted = render::format_duration(milliseconds);
            if row.status == "active" {
                format!("{formatted} (running)")
            } else {
                formatted
            }
        },
    );

    println!("Session receipt {}", row.id);
    println!();
    println!("  agent      {}", row.agent_id);
    println!(
        "  role       {}",
        if row.is_subagent { "subagent" } else { "main" }
    );
    println!("  status     {}", row.status);
    println!("  started    {started}");
    println!("  duration   {duration}");
    println!(
        "  branch     {}",
        row.git_branch.as_deref().unwrap_or("unknown")
    );
    println!(
        "  surface    {}",
        row.surface.as_deref().unwrap_or("unknown")
    );

    println!();
    println!("Projects touched ({})", projects.len());
    if projects.is_empty() {
        println!("  None observed.");
    } else {
        for project in projects {
            println!("  {project}");
        }
    }

    println!();
    println!("Tokens by model and role ({})", tokens.len());
    if tokens.is_empty() {
        println!("  None observed.");
    } else {
        println!("{}", render::receipt_token_header());
        for group in &tokens {
            println!("{}", render::receipt_token_line(group));
        }
    }

    println!();
    println!("Files touched ({})", files.len());
    if files.is_empty() {
        println!("  None observed.");
    } else {
        println!("{}", render::receipt_file_header());
        for file in &files {
            println!("{}", render::receipt_file_line(file));
        }
    }

    println!();
    println!("Commands executed ({})", commands.len());
    if commands.is_empty() {
        println!("  None observed.");
    } else {
        println!("{}", render::receipt_command_header());
        for command in &commands {
            println!("{}", render::receipt_command_line(command));
        }
    }

    println!();
    println!("Sensitive access ({})", notable.len());
    if notable.is_empty() {
        println!("  None observed.");
    } else {
        println!("{}", render::notable_header());
        for access in &notable {
            println!("{}", render::notable_line(access));
        }
    }
    println!();
    println!("{}", render::SECURITY_CAVEAT);

    println!();
    println!("Timeline ({} events)", timeline.len());
    if timeline.is_empty() {
        println!("  Nothing recorded.");
    } else {
        println!("{}", render::header());
        for event in &timeline {
            println!("{}", render::event_line_painted(event));
        }
    }

    println!();
    println!("Coverage");
    print_coverage(&coverage);

    println!();
    println!("Coverage gaps");
    for gap in render::coverage_gaps(&row.agent_id, row.git_branch.is_some()) {
        println!("  {gap}");
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
    let (zone, _) = range::local_zone();
    let range = range::last_days(days, zone);

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
    let (zone, _) = range::local_zone();
    let range = range::last_days(days, zone);

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
    let (zone, _) = range::local_zone();
    let range = range::last_days(days, zone);

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

    println!("source files read    {}", report.files);
    println!("  Claude transcripts {}", report.claude_files);
    println!("  Codex rollouts     {}", report.codex_files);
    if report.unreadable > 0 {
        println!("unreadable           {}", report.unreadable);
    }
    println!(
        "usage records        {}",
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

/// Re-applies the active command-redaction policy to historical rows.
fn scrub(paths: &Paths, dry_run: bool) -> Result<()> {
    anyhow::ensure!(
        paths.database().exists(),
        "no database yet at {}\nRun `agentwatch init` first.",
        paths.database().display()
    );

    let mut store = Store::open(paths.database()).context("opening the database")?;
    let report = store
        .scrub_commands(dry_run)
        .context("scrubbing stored commands")?;

    println!(
        "commands scanned      {}",
        render::thousands(report.scanned as i64)
    );
    println!(
        "commands needing scrub {}",
        render::thousands(report.changed as i64)
    );
    println!("custom patterns       {}", report.custom_patterns);
    println!(
        "pattern file          {}",
        paths.redaction_patterns().display()
    );
    if report.dry_run {
        println!("\nDry run only. Re-run without `--dry-run` to apply these changes.");
    } else if report.changed == 0 {
        println!("\nNo stored command needed changes.");
    } else {
        println!("\nStored command projections and raw event payloads were scrubbed.");
    }

    Ok(())
}

/// Re-derives totals from the transcripts and reports drift.
fn verify(paths: &Paths) -> Result<()> {
    let store = open_for_reading(paths)?;
    let drift = sync::verify(&store)?;

    println!("{:<22} {:>16} {:>16}", "", "source logs", "stored");
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

#[cfg(test)]
mod session_receipt_tests {
    use std::io::Cursor;

    use agentwatch_adapter_claude::{ClaudeAdapter, read_token_usage_from};
    use agentwatch_adapter_codex::read_rollout_from;
    use agentwatch_events::{HookAdapter, HookEnvelope};

    use super::*;

    fn claude_hook(payload: serde_json::Value, timestamp_us: i64) -> agentwatch_events::AgentEvent {
        let envelope: HookEnvelope = serde_json::from_value(serde_json::json!({
            "v": 1,
            "source": "claude-code",
            "sent_at": timestamp_us,
            "hook_version": "test",
            "payload": payload,
        }))
        .expect("envelope");
        ClaudeAdapter::new()
            .normalize(&envelope)
            .expect("normalize hook")
    }

    #[test]
    fn claude_receipt_combines_hooks_transcript_models_and_sidechains() {
        let mut store = Store::open_in_memory().expect("schema");
        let hooks = [
            claude_hook(
                serde_json::json!({
                    "hook_event_name": "SessionStart",
                    "session_id": "claude-receipt",
                    "cwd": "/work/claude",
                    "source": "startup"
                }),
                1,
            ),
            claude_hook(
                serde_json::json!({
                    "hook_event_name": "PostToolUse",
                    "session_id": "claude-receipt",
                    "cwd": "/work/claude",
                    "tool_name": "Read",
                    "tool_input": {"file_path": "/Users/dev/.aws/credentials"}
                }),
                2,
            ),
            claude_hook(
                serde_json::json!({
                    "hook_event_name": "PostToolUse",
                    "session_id": "claude-receipt",
                    "cwd": "/work/claude",
                    "tool_name": "Bash",
                    "tool_input": {"command": "cargo test", "description": "tests"}
                }),
                3,
            ),
            claude_hook(
                serde_json::json!({
                    "hook_event_name": "PostToolUse",
                    "session_id": "claude-receipt",
                    "cwd": "/work/claude",
                    "tool_name": "mcp__github__search",
                    "tool_input": {}
                }),
                4,
            ),
            claude_hook(
                serde_json::json!({
                    "hook_event_name": "SessionEnd",
                    "session_id": "claude-receipt",
                    "cwd": "/work/claude",
                    "reason": "complete"
                }),
                5,
            ),
        ];
        store.insert_events(&hooks).expect("hooks");

        let transcript = r#"
{"type":"assistant","timestamp":"2026-08-20T17:22:02.051Z","sessionId":"claude-receipt","cwd":"/work/claude","gitBranch":"main","entrypoint":"claude-vscode","isSidechain":false,"message":{"id":"main-response","model":"claude-main","usage":{"input_tokens":10,"output_tokens":5}}}
{"type":"assistant","timestamp":"2026-08-20T17:22:03.051Z","sessionId":"claude-receipt","cwd":"/work/claude","gitBranch":"main","entrypoint":"claude-vscode","isSidechain":true,"message":{"id":"child-response","model":"claude-child","usage":{"input_tokens":4,"output_tokens":3}}}
"#;
        let (usage, _) = read_token_usage_from(Cursor::new(transcript)).expect("transcript");
        store.insert_events(&usage).expect("usage");

        let session = store
            .sessions(0, i64::MAX, 10)
            .expect("sessions")
            .into_iter()
            .find(|session| session.agent_id == "claude-code")
            .expect("Claude session");
        let tokens = store.receipt_tokens(&session.id).expect("tokens");
        let files = store.receipt_files(&session.id).expect("files");
        let commands = store.receipt_commands(&session.id).expect("commands");
        let notable = store.receipt_notable_access(&session.id).expect("notable");
        let coverage = store.coverage(&session.id).expect("coverage");

        assert_eq!(session.git_branch.as_deref(), Some("main"));
        assert_eq!(session.surface.as_deref(), Some("claude-vscode"));
        assert!(tokens.iter().any(|group| !group.is_subagent));
        assert!(tokens.iter().any(|group| group.is_subagent));
        assert_eq!((files.len(), files[0].reads, files[0].writes), (1, 1, 0));
        assert_eq!(commands[0].command, "cargo test");
        assert_eq!(notable[0].evidence, "hook");
        assert!(coverage.tokens && coverage.files && coverage.commands && coverage.mcp);
    }

    #[test]
    fn codex_receipt_rolls_separate_child_rollout_activity_into_the_parent() {
        let mut store = Store::open_in_memory().expect("schema");
        let main = r#"
{"timestamp":"2026-08-21T12:00:00Z","type":"session_meta","payload":{"id":"codex-main","session_id":"codex-main","cwd":"/work/codex","originator":"codex_vscode","source":"vscode"}}
{"timestamp":"2026-08-21T12:00:01Z","type":"turn_context","payload":{"model":"gpt-main"}}
{"timestamp":"2026-08-21T12:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"tools.exec_command({\"cmd\":\"cargo test\",\"workdir\":\"/work/codex\"})"}}
{"timestamp":"2026-08-21T12:00:03Z","type":"event_msg","payload":{"type":"patch_apply_end","success":true,"changes":{"/work/codex/src/main.rs":{"type":"update"}}}}
{"timestamp":"2026-08-21T12:00:04Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}}
"#;
        let child = r#"
{"timestamp":"2026-08-21T12:00:05Z","type":"session_meta","payload":{"id":"codex-child","session_id":"codex-main","cwd":"/work/codex","source":{"subagent":{"other":"reviewer"}},"thread_source":"subagent"}}
{"timestamp":"2026-08-21T12:00:06Z","type":"turn_context","payload":{"model":"gpt-reviewer"}}
{"timestamp":"2026-08-21T12:00:07Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"tools.exec_command({\"cmd\":\"cargo clippy\",\"workdir\":\"/work/codex\"})"}}
{"timestamp":"2026-08-21T12:00:08Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":20,"output_tokens":8,"total_tokens":28}}}}
"#;
        let (mut events, _) = read_rollout_from(Cursor::new(main), None).expect("main rollout");
        let (child_events, _) = read_rollout_from(Cursor::new(child), None).expect("child rollout");
        events.extend(child_events);
        store.insert_events(&events).expect("events");

        let parent = store
            .sessions(0, i64::MAX, 10)
            .expect("sessions")
            .into_iter()
            .find(|session| !session.is_subagent)
            .expect("parent");
        let tokens = store.receipt_tokens(&parent.id).expect("tokens");
        let commands = store.receipt_commands(&parent.id).expect("commands");
        let timeline = store
            .activity(
                0,
                i64::MAX,
                &ActivityFilter {
                    session: Some(parent.id.clone()),
                    include_subagents: true,
                    ..ActivityFilter::default()
                },
                u32::MAX,
            )
            .expect("timeline");

        assert!(tokens.iter().any(|group| group.model == "gpt-main"));
        assert!(
            tokens
                .iter()
                .any(|group| { group.model == "gpt-reviewer" && group.is_subagent })
        );
        assert_eq!(commands.len(), 2);
        assert!(
            commands
                .iter()
                .any(|command| command.command == "cargo clippy")
        );
        assert!(timeline.iter().any(|event| event.kind == "file.write"));
        assert!(timeline.len() >= events.len());
    }
}
