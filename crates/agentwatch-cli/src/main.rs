//! The AgentWatch CLI.
//!
//! Phase 1 is deliberately thin: enough to prove the pipeline works and to wire
//! the hooks up. The analytics commands arrive in phase 3.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod hook_config;
mod range;
mod render;
mod sync;

use agentwatch_storage::{Store, TokenTotals};
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
    /// By working directory.
    Project,
    /// By the exact model identifier the provider reported.
    Model,
    /// By calendar day in your local timezone.
    Day,
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
        Grouping::Project => store.tokens_by_project(range.from_us, range.to_us),
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
        Grouping::Project => "project",
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

/// Imports historical usage from transcripts.
fn import(paths: &Paths, limit: Option<usize>) -> Result<()> {
    paths.ensure_root().context("creating the data directory")?;
    let mut store = Store::open(paths.database()).context("opening the database")?;

    let report = sync::import(&mut store, limit)?;

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
fn open_for_reading(paths: &Paths) -> Result<Store> {
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
