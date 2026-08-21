//! Menu bar status item.
//!
//! Deliberately a *status* surface, not a control panel. It answers the two
//! questions worth a glance — is collection running, and what has it cost today
//! — and hands everything else to the CLI. A menu bar item that tries to be a
//! dashboard becomes a dashboard nobody can pipe, filter, or script.
//!
//! Excluded from the workspace's default members: `tray-icon` and `winit`
//! roughly double the dependency tree, which is not a cost to impose on people
//! who only ever use the command line.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod icon;
mod state;

use std::time::Duration;

use agentwatch_types::Paths;
use anyhow::{Context as _, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};

use crate::icon::Glyph;
use crate::state::{Snapshot, Zone};

/// How often the database is re-read.
///
/// Matched to the daemon's flush interval multiplied out: faster than this and
/// the title flickers between values nobody asked to watch that closely.
const REFRESH: Duration = Duration::from_secs(2);

fn main() -> Result<()> {
    let paths = Paths::from_env().context("resolving the data directory")?;

    // Before the event loop, deliberately. Reading the local timezone is only
    // sound while the process is single threaded, and `EventLoop::new` is where
    // that stops being true.
    let zone = Zone::resolve();

    let event_loop = EventLoop::new().context("creating the event loop")?;
    event_loop.set_control_flow(ControlFlow::WaitUntil(std::time::Instant::now() + REFRESH));

    let mut application = App::new(paths, zone);
    event_loop
        .run_app(&mut application)
        .context("running the event loop")?;
    Ok(())
}

/// The menu bar application.
struct App {
    /// Where the data lives.
    paths: Paths,
    /// The timezone "today" is measured in, resolved at startup.
    zone: Zone,
    /// The status item, once created.
    tray: Option<tray_icon::TrayIcon>,
    /// Menu entries whose labels are refreshed.
    items: Option<Items>,
    /// Last snapshot drawn, so unchanged state costs no redraws.
    last: Option<Snapshot>,
}

/// Menu entries we update or respond to.
struct Items {
    /// Daemon state line.
    status: MenuItem,
    /// Today's token total.
    tokens: MenuItem,
    /// Sessions seen today.
    sessions: MenuItem,
    /// Sensitive accesses today.
    sensitive: MenuItem,
    /// Pause or resume collection.
    pause: MenuItem,
    /// Open the live view in a terminal.
    dashboard: MenuItem,
    /// Quit this status item.
    quit: MenuItem,
}

impl App {
    /// Creates the application.
    const fn new(paths: Paths, zone: Zone) -> Self {
        Self {
            zone,
            paths,
            tray: None,
            items: None,
            last: None,
        }
    }

    /// Builds the status item and its menu.
    fn build(&mut self) -> Result<()> {
        let items = Items {
            status: MenuItem::new("Checking…", false, None),
            tokens: MenuItem::new("Tokens today: —", false, None),
            sessions: MenuItem::new("Sessions today: —", false, None),
            sensitive: MenuItem::new("Sensitive access: —", false, None),
            pause: MenuItem::new("Pause collection", true, None),
            dashboard: MenuItem::new("Open live view", true, None),
            quit: MenuItem::new("Quit AgentWatch menu", true, None),
        };

        let menu = Menu::new();
        menu.append(&items.status)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&items.tokens)?;
        menu.append(&items.sessions)?;
        menu.append(&items.sensitive)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&items.pause)?;
        menu.append(&items.dashboard)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&items.quit)?;

        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_title("—")
            .with_tooltip("AgentWatch");

        if let Some(image) = icon::build(Glyph::Idle) {
            // Template mode lets macOS recolour the glyph for the current menu
            // bar; without it the icon stays black and vanishes in dark mode.
            builder = builder.with_icon(image).with_icon_as_template(true);
        }

        self.tray = Some(builder.build().context("creating the status item")?);
        self.items = Some(items);
        Ok(())
    }

    /// Re-reads the database and updates the menu if anything moved.
    fn refresh(&mut self) {
        let snapshot = Snapshot::read(&self.paths, self.zone);
        if self.last.as_ref() == Some(&snapshot) {
            return;
        }

        if let (Some(tray), Some(items)) = (self.tray.as_ref(), self.items.as_ref()) {
            if let Some(image) = icon::build(snapshot.glyph()) {
                let _ = tray.set_icon(Some(image));
            }
            tray.set_title(Some(snapshot.title()));
            items.status.set_text(snapshot.status_line());
            items
                .tokens
                .set_text(format!("Tokens today: {}", snapshot.tokens_text()));
            items
                .sessions
                .set_text(format!("Sessions today: {}", snapshot.sessions));
            items.sensitive.set_text(snapshot.sensitive_line());
            items.pause.set_text(if snapshot.paused {
                "Resume collection"
            } else {
                "Pause collection"
            });
        }

        self.last = Some(snapshot);
    }

    /// Handles a menu click.
    fn on_menu(&mut self, id: &tray_icon::menu::MenuId, event_loop: &ActiveEventLoop) {
        let Some(items) = self.items.as_ref() else {
            return;
        };

        if id == items.quit.id() {
            // Quits the status item only. Collection is the daemon's job and
            // keeps running, which is the behaviour someone who just wants the
            // icon gone almost certainly wants.
            event_loop.exit();
        } else if id == items.pause.id() {
            let paused = self.paths.is_paused();
            let _ = run_cli(&self.paths, if paused { "resume" } else { "pause" });
            self.last = None;
            self.refresh();
        } else if id == items.dashboard.id() {
            open_live_view(&self.paths);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        if matches!(cause, winit::event::StartCause::Init)
            && let Err(error) = self.build()
        {
            eprintln!("could not create the status item: {error:#}");
            event_loop.exit();
            return;
        }

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            self.on_menu(&event.id, event_loop);
        }
        while TrayIconEvent::receiver().try_recv().is_ok() {}

        self.refresh();
        event_loop.set_control_flow(ControlFlow::WaitUntil(std::time::Instant::now() + REFRESH));
    }
}

/// Runs an `agentwatch` subcommand, ignoring failure.
fn run_cli(paths: &Paths, command: &str) -> Result<()> {
    let binary = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|directory| directory.join("agentwatch")))
        .unwrap_or_else(|| "agentwatch".into());

    std::process::Command::new(binary)
        .arg(command)
        .env("AGENTWATCH_DIR", paths.root())
        .output()
        .context("running agentwatch")?;
    Ok(())
}

/// Opens the live view in Terminal.
///
/// `agentwatch watch` owns a terminal, so it needs one opened for it rather
/// than being spawned headless where its output would go nowhere.
fn open_live_view(paths: &Paths) {
    let binary = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|directory| directory.join("agentwatch")))
        .unwrap_or_else(|| "agentwatch".into());

    let script = format!(
        "tell application \"Terminal\" to do script \"AGENTWATCH_DIR={} {} watch\"\n\
         tell application \"Terminal\" to activate",
        shell_quote(&paths.root().display().to_string()),
        shell_quote(&binary.display().to_string()),
    );

    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output();
}

/// Quotes a path for inclusion in a shell command inside AppleScript.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}
