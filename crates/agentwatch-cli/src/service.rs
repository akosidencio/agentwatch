//! Running AgentWatch's background jobs under launchd.
//!
//! Rendering the plist is a pure function so it can be tested without touching
//! `~/Library/LaunchAgents`, and so the text shown before installing is the
//! exact text that gets written.
//!
//! Two jobs, one machine: the collector, which must run whether or not anyone
//! is looking, and the status item, which is meaningless without a GUI login.
//! They are separate launchd jobs rather than one, so a CLI-only user is never
//! given a menu bar item and quitting the icon never stops collection.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

/// Which job a command is talking about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Job {
    /// The collector. Runs headless, wants to run always.
    Daemon,
    /// The menu bar status item. Only meaningful in a GUI login session.
    MenuBar,
}

impl Job {
    /// The launchd job label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Daemon => "dev.agentwatch.daemon",
            Self::MenuBar => "dev.agentwatch.menubar",
        }
    }

    /// The subcommand the job runs, if it takes one.
    ///
    /// The collector is a subcommand of the one `agentwatch` executable. The
    /// status item is its own binary — see its module docs for the measurement
    /// that keeps it that way — so it takes none.
    pub(crate) const fn subcommand(self) -> Option<&'static str> {
        match self {
            Self::Daemon => Some("daemon"),
            Self::MenuBar => None,
        }
    }

    /// The executable name the job runs, relative to the install directory.
    pub(crate) const fn binary_name(self) -> &'static str {
        match self {
            Self::Daemon => "agentwatch",
            Self::MenuBar => "agentwatch-menubar",
        }
    }

    /// The log file the job writes, relative to the data directory.
    pub(crate) const fn log_name(self) -> &'static str {
        match self {
            Self::Daemon => "daemon.log",
            Self::MenuBar => "menubar.log",
        }
    }

    /// How the job is described to the user.
    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Daemon => "collector",
            Self::MenuBar => "menu bar status item",
        }
    }
}

/// Every job, for commands that report on all of them.
pub(crate) const JOBS: [Job; 2] = [Job::Daemon, Job::MenuBar];

/// Where a job definition lives.
pub(crate) fn plist_path(job: Job) -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
    home.join("Library/LaunchAgents")
        .join(format!("{}.plist", job.label()))
}

/// The executable a job runs.
///
/// The running binary for the collector, and its sibling for the status item,
/// so a tree built or installed anywhere wires itself up rather than pointing at
/// whatever happens to be in `~/.local/bin`.
pub(crate) fn default_binary(job: Job) -> PathBuf {
    let fallback = || {
        let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
        home.join(".local/bin").join(job.binary_name())
    };

    let Ok(current) = std::env::current_exe() else {
        return fallback();
    };
    match job {
        Job::Daemon => current,
        Job::MenuBar => current
            .parent()
            .map(|directory| directory.join(job.binary_name()))
            .unwrap_or_else(fallback),
    }
}

/// Renders the launchd job definition.
///
/// `KeepAlive` restarts only on a crash, not on a clean exit, so
/// `agentwatch service stop` actually stops it rather than being undone by
/// launchd a second later.
pub(crate) fn plist(
    job: Job,
    binary: &Path,
    log_directory: &Path,
    overrides: &[(&str, PathBuf)],
) -> String {
    // launchd jobs start with an empty environment, so anything the daemon
    // reads from one has to be baked in here. Without this a user with a custom
    // AGENTWATCH_DIR would find the service writing to a different database
    // than their CLI reads, and a custom CLAUDE_CONFIG_DIR would leave it
    // watching a settings file that is not the one in use.
    let environment = if overrides.is_empty() {
        String::new()
    } else {
        let entries: String = overrides
            .iter()
            .map(|(key, value)| {
                format!(
                    "
        <key>{key}</key>
        <string>{}</string>",
                    escape(&value.display().to_string())
                )
            })
            .collect();

        format!(
            "
    <key>EnvironmentVariables</key>
    <dict>{entries}
    </dict>"
        )
    };

    // Background for the collector; Interactive for the status item, which
    // draws in the menu bar and should not be given throttled I/O and a low
    // scheduling priority. Aqua-only for the same reason: a menu bar item has
    // nothing to do in a login session that has no menu bar.
    let (process_type, session) = match job {
        Job::Daemon => ("Background", String::new()),
        Job::MenuBar => (
            "Interactive",
            "\n
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>"
                .to_owned(),
        ),
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>

    <key>ProgramArguments</key>
    <array>
        <string>{executable}</string>{subcommand}
    </array>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>Crashed</key>
        <true/>
        <key>SuccessfulExit</key>
        <false/>
    </dict>

    <key>ThrottleInterval</key>
    <integer>10</integer>

    <key>ProcessType</key>
    <string>{process_type}</string>{session}

    <key>StandardOutPath</key>
    <string>{log}/{log_name}</string>

    <key>StandardErrorPath</key>
    <string>{log}/{log_name}</string>{environment}
</dict>
</plist>
"#,
        label = job.label(),
        executable = escape(&binary.display().to_string()),
        subcommand = job
            .subcommand()
            .map_or_else(String::new, |subcommand| format!(
                "\n        <string>{subcommand}</string>"
            )),
        log = escape(&log_directory.display().to_string()),
        log_name = job.log_name(),
    )
}

/// Escapes text for XML character data.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Whether launchd currently knows about the job.
pub(crate) fn is_loaded(job: Job) -> bool {
    std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{}/{}", user_id(), job.label())])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The current user's numeric id, for launchd's `gui/<uid>` domain.
///
/// Shells out rather than calling `getuid`, because the workspace forbids
/// `unsafe` and that policy is worth more than the process spawn — this runs
/// only when someone installs or queries the service. Cached so repeated calls
/// within one command cost nothing.
fn user_id() -> u32 {
    static CACHED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

    *CACHED.get_or_init(|| {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(501)
    })
}

/// Resolves the binary a job should run, and checks it is actually there.
///
/// A launchd job pointing at a missing file does not fail at install time — it
/// fails ten seconds later, in a log nobody is reading — so the check happens
/// here, before anything is written.
pub(crate) fn resolve_binary(job: Job, explicit: Option<PathBuf>) -> Result<PathBuf> {
    let binary = explicit.unwrap_or_else(|| default_binary(job));
    if !binary.is_file() {
        let extra = if job == Job::MenuBar {
            // Expected in a build from source: the menu bar is outside the
            // workspace's default members. Release archives carry it.
            "\nThe menu bar is not in the default build: `cargo build -p agentwatch-menubar --release`."
        } else {
            ""
        };
        bail!(
            "no {} at {}\nBuild it with `cargo build --release`, or pass --binary <path>.{}",
            job.binary_name(),
            binary.display(),
            extra
        );
    }

    // launchd runs the job from `/`, so a relative path would resolve nowhere.
    binary
        .canonicalize()
        .with_context(|| format!("resolving {}", binary.display()))
}

/// Writes a job definition and loads it, replacing any previous version.
///
/// Unloading before writing matters: replacing the plist under a loaded job
/// leaves launchd running the old binary, so an upgrade would silently do
/// nothing. Returns the definition's path.
pub(crate) fn install_job(job: Job, definition: &str) -> Result<PathBuf> {
    let path = plist_path(job);
    let directory = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating {}", directory.display()))?;

    if is_loaded(job) {
        bootout(job).context("unloading the previous job")?;
    }

    std::fs::write(&path, definition).with_context(|| format!("writing {}", path.display()))?;
    bootstrap(&path).context("loading the job")?;
    Ok(path)
}

/// Loads the job.
pub(crate) fn bootstrap(path: &Path) -> Result<()> {
    run(&[
        "bootstrap",
        &format!("gui/{}", user_id()),
        &path.display().to_string(),
    ])
}

/// Unloads the job.
pub(crate) fn bootout(job: Job) -> Result<()> {
    run(&["bootout", &format!("gui/{}/{}", user_id(), job.label())])
}

/// Runs launchctl and turns a non-zero exit into an error with its output.
fn run(arguments: &[&str]) -> Result<()> {
    let output = std::process::Command::new("launchctl")
        .args(arguments)
        .output()
        .context("running launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "launchctl {} failed: {}",
            arguments.join(" "),
            if stderr.trim().is_empty() {
                "no output".into()
            } else {
                stderr.trim().to_owned()
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The executable the collector runs: the CLI itself.
    const EXECUTABLE: &str = "/usr/local/bin/agentwatch";

    /// The status item's own binary.
    const MENU_BAR: &str = "/usr/local/bin/agentwatch-menubar";

    fn rendered() -> String {
        plist(
            Job::Daemon,
            Path::new(EXECUTABLE),
            Path::new("/Users/dev/.agentwatch"),
            &[],
        )
    }

    fn rendered_menu_bar() -> String {
        plist(
            Job::MenuBar,
            Path::new(MENU_BAR),
            Path::new("/Users/dev/.agentwatch"),
            &[],
        )
    }

    fn program_arguments(text: &str) -> &str {
        text.split("<key>ProgramArguments</key>")
            .nth(1)
            .and_then(|rest| rest.split("</array>").next())
            .expect("ProgramArguments")
    }

    #[test]
    fn the_collector_is_the_cli_with_a_subcommand() {
        let text = rendered();
        let arguments = program_arguments(&text);
        assert!(arguments.contains(EXECUTABLE), "{arguments}");
        assert!(
            arguments.contains("<string>daemon</string>"),
            "without the subcommand the job would print the welcome screen and \
             exit, and launchd would call that a clean start: {arguments}"
        );
    }

    #[test]
    fn the_status_item_takes_no_subcommand() {
        let text = rendered_menu_bar();
        let arguments = program_arguments(&text);
        assert!(arguments.contains(MENU_BAR), "{arguments}");
        assert_eq!(
            arguments.matches("<string>").count(),
            1,
            "its own binary, so there is nothing to pass it: {arguments}"
        );
    }

    #[test]
    fn the_plist_is_well_formed_xml() {
        let text = rendered();
        assert!(text.starts_with("<?xml"));
        assert_eq!(
            text.matches("<dict>").count(),
            text.matches("</dict>").count()
        );
        assert!(text.trim_end().ends_with("</plist>"));
    }

    #[test]
    fn the_plist_names_the_daemon_and_the_label() {
        let text = rendered();
        assert!(text.contains(EXECUTABLE));
        assert!(text.contains(Job::Daemon.label()));
    }

    #[test]
    fn a_clean_exit_is_not_restarted() {
        let text = rendered();
        let successful_exit = text.find("SuccessfulExit").expect("key present");
        assert!(
            text[successful_exit..].starts_with("SuccessfulExit</key>\n        <false/>"),
            "stopping the service must not be undone by launchd"
        );
    }

    #[test]
    fn logs_go_to_the_agentwatch_directory() {
        assert!(rendered().contains("/Users/dev/.agentwatch/daemon.log"));
    }

    #[test]
    fn a_custom_directory_is_passed_through_the_environment() {
        let text = plist(
            Job::Daemon,
            Path::new("/bin/d"),
            Path::new("/tmp/logs"),
            &[("AGENTWATCH_DIR", PathBuf::from("/tmp/custom"))],
        );
        assert!(text.contains("AGENTWATCH_DIR"));
        assert!(text.contains("/tmp/custom"));
    }

    #[test]
    fn no_environment_block_when_the_default_directory_is_used() {
        assert!(!rendered().contains("EnvironmentVariables"));
    }

    #[test]
    fn xml_special_characters_are_escaped() {
        let text = plist(
            Job::Daemon,
            Path::new("/opt/a&b/daemon"),
            Path::new("/tmp/<logs>"),
            &[],
        );
        assert!(text.contains("/opt/a&amp;b/daemon"));
        assert!(text.contains("&lt;logs&gt;"));
        assert!(
            !text.contains("/opt/a&b/"),
            "raw ampersand would break the plist"
        );
    }

    #[test]
    fn the_menu_bar_plist_is_well_formed_and_names_its_own_job() {
        let text = rendered_menu_bar();
        assert!(text.starts_with("<?xml"));
        assert!(text.trim_end().ends_with("</plist>"));
        assert_eq!(
            text.matches("<dict>").count(),
            text.matches("</dict>").count()
        );
        assert!(text.contains("dev.agentwatch.menubar"));
        assert!(text.contains(MENU_BAR));
    }

    #[test]
    fn the_two_jobs_never_collide() {
        // Same label or same log file would mean installing one silently
        // replaced or corrupted the other.
        assert_ne!(Job::Daemon.label(), Job::MenuBar.label());
        assert_ne!(Job::Daemon.log_name(), Job::MenuBar.log_name());
        assert_ne!(Job::Daemon.subcommand(), Job::MenuBar.subcommand());
        assert_ne!(plist_path(Job::Daemon), plist_path(Job::MenuBar));
        assert!(rendered_menu_bar().contains("menubar.log"));
        assert!(rendered().contains("daemon.log"));
    }

    #[test]
    fn only_the_menu_bar_is_restricted_to_a_gui_session() {
        // The collector must run in any session type; a status item in a
        // session with no menu bar would be launchd restarting it forever.
        assert!(rendered_menu_bar().contains("<key>LimitLoadToSessionType</key>"));
        assert!(rendered_menu_bar().contains("<string>Aqua</string>"));
        assert!(!rendered().contains("LimitLoadToSessionType"));
    }

    #[test]
    fn the_menu_bar_is_interactive_and_the_collector_is_not() {
        // Background gets throttled I/O and a low scheduling priority, which
        // is right for the collector and wrong for something that draws.
        assert!(rendered_menu_bar().contains("<string>Interactive</string>"));
        assert!(rendered().contains("<string>Background</string>"));
    }
}
