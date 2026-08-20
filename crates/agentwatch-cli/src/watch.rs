//! The live activity view.
//!
//! A separate surface from the one-shot commands, deliberately. A TUI owns the
//! terminal, so it cannot be piped into `grep` or `jq`; the printing commands
//! stay plain stdout for exactly that reason. This exists for the question the
//! printing commands answer badly: *what is happening right now.*
//!
//! Fed by polling SQLite rather than by a subscription. At this volume a poll
//! costs a couple of milliseconds against an index, WAL means it never blocks
//! the daemon's writes, and it needs no new protocol. A push path stays
//! available if polling ever shows up in a profile.

use std::io::Stdout;
use std::time::{Duration, Instant};

use agentwatch_storage::{ActivityFilter, EventRow, Store, TokenTotals};
use agentwatch_types::Paths;
use anyhow::{Context as _, Result};
use crossterm::event::{self, Event as TerminalEvent, KeyCode, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Row, Table};

use crate::range;
use crate::render;

/// How often to re-read the database.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long to wait for a keypress between polls.
///
/// Shorter than the poll interval so quitting feels immediate rather than
/// taking up to a full refresh.
const INPUT_TIMEOUT: Duration = Duration::from_millis(100);

/// Events kept in the scrollback.
const FEED_LENGTH: u32 = 200;

/// Everything the view draws.
struct Dashboard {
    /// Today's token counts.
    totals: TokenTotals,
    /// Sessions seen today, newest first.
    sessions: Vec<agentwatch_storage::SessionRow>,
    /// The recent event feed, oldest first.
    feed: Vec<EventRow>,
    /// Sensitive accesses today.
    notable: usize,
    /// Whether the daemon is currently listening.
    daemon_running: bool,
    /// When this snapshot was taken.
    refreshed_at: Instant,
}

/// Runs the live view until the user quits.
///
/// # Errors
///
/// Returns an error if the terminal cannot be set up or the database read.
pub(crate) fn run(paths: &Paths) -> Result<()> {
    let store = crate::open_for_reading(paths)?;
    let mut terminal = enter()?;

    let outcome = event_loop(&mut terminal, &store, paths);

    // Restore the terminal whatever happened: leaving a user in raw mode with
    // an alternate screen is a far worse failure than whatever caused it.
    leave(&mut terminal)?;
    outcome
}

/// Polls, draws, and handles input until asked to stop.
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    store: &Store,
    paths: &Paths,
) -> Result<()> {
    let mut dashboard = snapshot(store, paths)?;

    loop {
        terminal.draw(|frame| draw(frame, &dashboard))?;

        if event::poll(INPUT_TIMEOUT)?
            && let TerminalEvent::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Char('q' | 'Q') | KeyCode::Esc)
        {
            return Ok(());
        }

        if dashboard.refreshed_at.elapsed() >= POLL_INTERVAL {
            dashboard = snapshot(store, paths)?;
        }
    }
}

/// Reads the current state of the world.
fn snapshot(store: &Store, paths: &Paths) -> Result<Dashboard> {
    let (offset, _) = range::local_offset();
    let today = range::last_days(1, offset);

    Ok(Dashboard {
        totals: store
            .token_totals(today.from_us, today.to_us)
            .context("reading totals")?,
        sessions: store
            .sessions(today.from_us, today.to_us, 8)
            .context("reading sessions")?,
        feed: store
            .activity(
                today.from_us,
                today.to_us,
                &ActivityFilter::default(),
                FEED_LENGTH,
            )
            .context("reading activity")?,
        notable: store
            .notable_access(today.from_us, today.to_us, 500)
            .context("reading sensitive access")?
            .len(),
        daemon_running: std::os::unix::net::UnixStream::connect(paths.socket()).is_ok(),
        refreshed_at: Instant::now(),
    })
}

/// Lays out and renders one frame.
fn draw(frame: &mut ratatui::Frame<'_>, dashboard: &Dashboard) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(9),
            Constraint::Min(5),
        ])
        .split(frame.area());

    draw_header(frame, areas[0], dashboard);
    draw_sessions(frame, areas[1], dashboard);
    draw_feed(frame, areas[2], dashboard);
}

/// The status line: daemon state, today's tokens, sensitive count.
fn draw_header(frame: &mut ratatui::Frame<'_>, area: Rect, dashboard: &Dashboard) {
    let (indicator, indicator_style) = if dashboard.daemon_running {
        ("running", Style::default().fg(Color::Green))
    } else {
        // Not an error: the view still works against stored history. But it
        // must be obvious, or an idle feed reads as an idle agent.
        ("not running", Style::default().fg(Color::Yellow))
    };

    let notable_style = if dashboard.notable > 0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let line = Line::from(vec![
        Span::styled("daemon ", Style::default().fg(Color::DarkGray)),
        Span::styled(indicator, indicator_style),
        Span::raw("   "),
        Span::styled("tokens today ", Style::default().fg(Color::DarkGray)),
        Span::raw(render::thousands(dashboard.totals.total())),
        Span::raw("   "),
        Span::styled("responses ", Style::default().fg(Color::DarkGray)),
        Span::raw(render::thousands(dashboard.totals.responses)),
        Span::raw("   "),
        Span::styled("sensitive ", Style::default().fg(Color::DarkGray)),
        Span::styled(dashboard.notable.to_string(), notable_style),
    ]);

    frame.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" agentwatch — q to quit "),
        ),
        area,
    );
}

/// Today's sessions.
fn draw_sessions(frame: &mut ratatui::Frame<'_>, area: Rect, dashboard: &Dashboard) {
    let home = std::env::var("HOME").ok();

    let rows = dashboard.sessions.iter().map(|session| {
        let style = match session.status.as_str() {
            "active" => Style::default().fg(Color::Green),
            "ended" => Style::default().fg(Color::DarkGray),
            _ => Style::default().fg(Color::Yellow),
        };

        Row::new(vec![
            session.id[..8.min(session.id.len())].to_owned(),
            session.status.clone(),
            render::thousands(session.tokens),
            session.commands.to_string(),
            session.files.to_string(),
            if session.sensitive > 0 {
                session.sensitive.to_string()
            } else {
                "-".to_owned()
            },
            session.project.as_deref().map_or_else(String::new, |path| {
                render::short_path(path, home.as_deref())
            }),
        ])
        .style(style)
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(14),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec![
            "session", "status", "tokens", "cmd", "file", "sens", "project",
        ])
        .style(Style::default().fg(Color::DarkGray)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" sessions today "),
    );

    frame.render_widget(table, area);
}

/// The live event feed, newest at the bottom.
fn draw_feed(frame: &mut ratatui::Frame<'_>, area: Rect, dashboard: &Dashboard) {
    let visible = area.height.saturating_sub(2) as usize;
    let start = dashboard.feed.len().saturating_sub(visible);

    let items: Vec<ListItem<'_>> = dashboard.feed[start..]
        .iter()
        .map(|row| {
            let style = match row.kind.as_str() {
                "command" => Style::default().fg(Color::Cyan),
                "file.write" => Style::default().fg(Color::Magenta),
                "mcp.call" => Style::default().fg(Color::Blue),
                "session.started" | "session.ended" => Style::default().fg(Color::Green),
                "token.usage" => Style::default().fg(Color::DarkGray),
                _ => Style::default(),
            };
            ListItem::new(Line::styled(render::event_line(row), style))
        })
        .collect();

    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(" activity ")),
        area,
    );
}

/// Takes over the terminal.
fn enter() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    crossterm::terminal::enable_raw_mode().context("entering raw mode")?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)
        .context("entering the alternate screen")?;
    Terminal::new(CrosstermBackend::new(stdout)).context("creating the terminal")
}

/// Gives the terminal back.
fn leave(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    crossterm::terminal::disable_raw_mode().context("leaving raw mode")?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )
    .context("leaving the alternate screen")?;
    terminal.show_cursor().context("restoring the cursor")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use agentwatch_storage::SessionRow;
    use ratatui::backend::TestBackend;

    use super::*;

    /// Renders a dashboard to an off-screen terminal and returns it as text.
    fn render(dashboard: &Dashboard) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, dashboard))
            .expect("draws");

        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
    }

    fn session(status: &str, sensitive: i64) -> SessionRow {
        SessionRow {
            id: "abcdef1234567890".to_owned(),
            agent_id: "claude-code".to_owned(),
            project: Some("/work/acme".to_owned()),
            git_branch: Some("main".to_owned()),
            started_at_us: Some(1_755_000_000_000_000),
            duration_ms: None,
            status: status.to_owned(),
            tokens: 1_234_567,
            responses: 12,
            commands: 3,
            files: 4,
            mcp_calls: 1,
            sensitive,
        }
    }

    fn dashboard(daemon_running: bool, notable: usize) -> Dashboard {
        Dashboard {
            totals: TokenTotals {
                input: 10,
                cache_creation: 20,
                cache_read: 30,
                output: 40,
                responses: 7,
            },
            sessions: vec![session("active", notable as i64)],
            feed: vec![EventRow {
                timestamp_us: 1_755_000_000_000_000,
                agent_id: "claude-code".to_owned(),
                kind: "command".to_owned(),
                evidence: "hook".to_owned(),
                project_path: Some("/work/acme".to_owned()),
                payload: r#"{"kind":"command","command":"cargo test"}"#.to_owned(),
            }],
            notable,
            daemon_running,
            refreshed_at: Instant::now(),
        }
    }

    #[test]
    fn renders_without_panicking() {
        let output = render(&dashboard(true, 0));
        assert!(
            output.contains("agentwatch"),
            "the frame should have a title"
        );
    }

    #[test]
    fn shows_todays_token_total() {
        let output = render(&dashboard(true, 0));
        assert!(output.contains("100"), "10+20+30+40 should be shown");
    }

    #[test]
    fn a_stopped_daemon_is_visible_rather_than_looking_idle() {
        let stopped = render(&dashboard(false, 0));
        assert!(
            stopped.contains("not running"),
            "an idle feed must not be confusable with a dead collector"
        );

        let running = render(&dashboard(true, 0));
        assert!(running.contains("running"));
    }

    #[test]
    fn shows_the_session_and_its_project() {
        let output = render(&dashboard(true, 0));
        assert!(
            output.contains("abcdef12"),
            "the session id should be shown"
        );
        assert!(output.contains("acme"), "the project should be shown");
    }

    #[test]
    fn shows_the_activity_feed() {
        let output = render(&dashboard(true, 0));
        assert!(
            output.contains("cargo test"),
            "the command should appear in the feed"
        );
    }

    #[test]
    fn survives_an_empty_database() {
        let empty = Dashboard {
            totals: TokenTotals::default(),
            sessions: Vec::new(),
            feed: Vec::new(),
            notable: 0,
            daemon_running: false,
            refreshed_at: Instant::now(),
        };
        assert!(render(&empty).contains("agentwatch"));
    }

    #[test]
    fn survives_a_terminal_too_small_to_hold_the_layout() {
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).expect("tiny terminal");
        terminal
            .draw(|frame| draw(frame, &dashboard(true, 1)))
            .expect("a cramped terminal must not panic");
    }

    #[test]
    fn a_feed_longer_than_the_pane_shows_its_tail() {
        let mut crowded = dashboard(true, 0);
        crowded.feed = (0..500)
            .map(|index| EventRow {
                timestamp_us: 1_755_000_000_000_000 + index,
                agent_id: "claude-code".to_owned(),
                kind: "command".to_owned(),
                evidence: "hook".to_owned(),
                project_path: None,
                payload: format!(r#"{{"kind":"command","command":"step-{index}"}}"#),
            })
            .collect();

        let output = render(&crowded);
        assert!(
            output.contains("step-499"),
            "the newest event must be visible"
        );
        assert!(
            !output.contains("step-0 "),
            "the oldest should have scrolled off"
        );
    }
}
