//! What you see when you type `agentwatch` with nothing after it.
//!
//! Clap's default for a missing subcommand is a usage error on stderr and a
//! non-zero exit. That is the correct answer for a script and the wrong one for
//! a person who has just installed something and is finding out what it does.
//! So a bare invocation is a welcome instead: what this is, whether it is
//! running, and the one command to type next.
//!
//! The installer prints this too, by running the binary it just installed. One
//! definition, so the greeting cannot drift from the tool.

use agentwatch_storage::Store;
use agentwatch_types::Paths;

use crate::{render, theme};

/// The tagline under the wordmark on a first run.
const TAGLINE: &str = "See what your AI agents are doing.";

/// The tagline once setup has actually worked.
pub(crate) const WELCOME: &str = "Set up and watching. Welcome.";

/// Commands worth knowing about, grouped by what you came here to do.
///
/// Not every command: `hook` is machine-invoked and `hook-config` is a fallback
/// for people who would rather paste settings by hand. `--help` is the complete
/// list, and this points at it.
const GROUPS: [(&str, &[(&str, &str)]); 3] = [
    (
        "Setup",
        &[
            ("init", "set up hooks, the collector, and history"),
            ("service", "manage the background jobs"),
            ("pause / resume", "stop and start recording"),
            ("uninstall", "take it all back off this machine"),
        ],
    ),
    (
        "Activity",
        &[
            ("status", "what is running, and what is stored"),
            ("watch", "live full-screen view"),
            ("activity", "timeline of what the agents did"),
            ("sessions", "sessions, and what their data can answer"),
        ],
    ),
    (
        "Analysis",
        &[
            ("tokens", "usage by project, model, or day"),
            ("security", "access to sensitive paths"),
            ("export", "events as JSON Lines"),
            ("verify", "re-derive totals from the transcripts"),
        ],
    ),
];

/// The banner: wordmark, version, and one line of context.
///
/// Padded before it is painted. `format!` counts escape bytes as width, so
/// colouring first collapses the box — which is exactly how this went wrong the
/// first time it was written.
pub(crate) fn banner(tagline: &str) -> String {
    let title = format!("A G E N T W A T C H   v{}", env!("CARGO_PKG_VERSION"));
    let width = title.chars().count().max(tagline.chars().count()) + 4;

    let line = |text: &str, colour: u8| {
        let padded = format!("  {text}");
        format!(
            "  │{}│\n",
            theme::paint(&format!("{padded:<width$}"), colour)
        )
    };

    let rule = "─".repeat(width);
    format!(
        "{}{}{}{}",
        theme::paint(&format!("  ╭{rule}╮\n"), theme::FAINT),
        line(&title, theme::ACCENT),
        line(tagline, theme::MUTED),
        theme::paint(&format!("  ╰{rule}╯\n"), theme::FAINT),
    )
}

/// Prints the welcome, the current state, and what to do next.
pub(crate) fn overview(paths: &Paths) {
    print!("{}", banner(TAGLINE));
    println!();

    match state(paths) {
        // Nothing recorded yet, so the only useful thing to say is how to
        // start. Said in full, rather than as a hint under a list of commands
        // that will all report nothing.
        None => {
            println!("  Not set up yet.");
            println!();
            println!(
                "  {}  {}",
                theme::bold("agentwatch init"),
                theme::paint(
                    "registers the hooks, starts the collector, reads your history",
                    theme::MUTED
                )
            );
            println!(
                "  {}  {}",
                theme::paint(&format!("{:<15}", "  --dry-run"), theme::FAINT),
                theme::paint("shows the whole plan and writes nothing", theme::MUTED)
            );
        }
        Some(summary) => println!("  {summary}"),
    }

    println!();
    for (group, commands) in GROUPS {
        for (index, (command, what)) in commands.iter().enumerate() {
            let heading = if index == 0 { group } else { "" };
            println!(
                "  {}{}{}",
                theme::paint(&format!("{heading:<11}"), theme::VIOLET),
                theme::paint(&format!("{command:<16}"), theme::ACCENT),
                theme::paint(what, theme::MUTED)
            );
        }
    }

    println!();
    for (command, what) in [
        ("agentwatch --help", "every command and flag"),
        ("agentwatch <command> --help", "what one command takes"),
    ] {
        println!(
            "  {}{}",
            theme::paint(&format!("{command:<29}"), theme::FAINT),
            theme::paint(what, theme::MUTED)
        );
    }
}

/// One line describing the current state, or `None` if there is nothing yet.
fn state(paths: &Paths) -> Option<String> {
    let running = std::os::unix::net::UnixStream::connect(paths.socket()).is_ok();
    let totals = Store::open_read_only(paths.database())
        .ok()
        .and_then(|store| store.totals().ok());

    let counts = match totals {
        // A database with nothing in it is the same situation as no database:
        // the person is still at the start, and "0 events" instead of what to
        // type next would be the less useful answer.
        None | Some(agentwatch_storage::Totals { events: 0, .. }) if !running => return None,
        // Unreadable, or empty, while the collector is up. Say so rather than
        // claiming a fresh install: an upgrade whose database is a version
        // ahead lands here, and "not set up yet" would be plainly wrong.
        None | Some(agentwatch_storage::Totals { events: 0, .. }) => {
            "nothing recorded yet".to_owned()
        }
        Some(totals) => format!(
            "{} events · {} sessions",
            render::thousands(totals.events),
            render::thousands(totals.sessions)
        ),
    };

    let (dot, word, colour) = if running {
        ("●", "collecting", theme::GOOD)
    } else {
        ("○", "not running", theme::WARN)
    };

    Some(format!(
        "{}  {counts}{}",
        theme::paint(&format!("{dot} {word}"), colour),
        if paths.is_paused() {
            theme::paint("  · PAUSED", theme::BAD)
        } else {
            String::new()
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_is_a_closed_box_carrying_the_version() {
        let text = banner(TAGLINE);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "top, wordmark, tagline, bottom");

        // Painting happens after padding, so with colour off every line is the
        // same width. If this fails, the box is ragged in a terminal.
        let widths: Vec<usize> = lines.iter().map(|line| line.chars().count()).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "ragged banner: {widths:?}"
        );
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        assert!(text.contains("A G E N T W A T C H"));
    }

    #[test]
    fn a_longer_tagline_widens_the_box_rather_than_overflowing_it() {
        let long = "a tagline considerably longer than the wordmark line above it";
        let text = banner(long);
        assert!(text.contains(long));
        let widths: Vec<usize> = text.lines().map(|line| line.chars().count()).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "{widths:?}"
        );
    }

    #[test]
    fn the_command_table_lines_up() {
        // Every cell is padded before it is painted, so the plain-text widths
        // are the widths a terminal sees. One ragged row makes the whole screen
        // look broken.
        for (_, commands) in GROUPS {
            for (command, _) in commands {
                assert!(
                    command.chars().count() < 16,
                    "{command} overflows its column"
                );
            }
        }
    }

    #[test]
    fn a_fresh_machine_is_told_to_run_init() {
        let empty = tempfile::tempdir().expect("temp dir");
        assert!(
            state(&Paths::with_root(empty.path())).is_none(),
            "with no database there is nothing to report but the next step"
        );
    }

    #[test]
    fn every_listed_command_is_a_real_one() {
        // A welcome screen naming a command that does not exist is worse than
        // no welcome screen.
        use clap::CommandFactory as _;
        let cli = crate::Cli::command();
        let known: Vec<String> = cli
            .get_subcommands()
            .map(|command| command.get_name().to_owned())
            .collect();

        for (_, commands) in GROUPS {
            for (listed, _) in commands {
                for name in listed.split(" / ") {
                    assert!(known.iter().any(|known| known == name), "unknown: {name}");
                }
            }
        }
    }
}
