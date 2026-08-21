//! The menu bar status item, as its own executable.
//!
//! Everything else — the CLI, the collector, the hook — is one `agentwatch`
//! binary. This one stays separate for a measured reason: linking `tray-icon`
//! and `winit` pulls AppKit and CoreGraphics into the executable, and dyld
//! loads them at every launch. Folded into `agentwatch`, that cost lands on the
//! hook, which the agent spawns on *every tool call*: measured 7.7 ms → 12.8 ms
//! per call, against 9.0 ms for the same merge without the menu bar.
//!
//! So the split is not tidiness. A status item nobody is looking at has no
//! business making every tool call slower.
//!
//! Nobody types this. `agentwatch init` installs it as a launchd job.

#![forbid(unsafe_code)]

use anyhow::Result;

fn main() -> Result<()> {
    agentwatch_menubar::run()
}
