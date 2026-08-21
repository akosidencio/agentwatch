//! Running the daemon under launchd.
//!
//! Rendering the plist is a pure function so it can be tested without touching
//! `~/Library/LaunchAgents`, and so the text shown before installing is the
//! exact text that gets written.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

/// launchd job label.
pub(crate) const LABEL: &str = "dev.agentwatch.daemon";

/// Where the job definition lives.
pub(crate) fn plist_path() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// Best guess at the installed daemon binary.
pub(crate) fn default_daemon_binary() -> PathBuf {
    if let Ok(current) = std::env::current_exe()
        && let Some(directory) = current.parent()
    {
        let sibling = directory.join("agentwatch-daemon");
        if sibling.is_file() {
            return sibling;
        }
    }

    let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
    home.join(".local/bin/agentwatch-daemon")
}

/// Renders the launchd job definition.
///
/// `KeepAlive` restarts only on a crash, not on a clean exit, so
/// `agentwatch service stop` actually stops it rather than being undone by
/// launchd a second later.
pub(crate) fn plist(daemon: &Path, log_directory: &Path, overrides: &[(&str, PathBuf)]) -> String {
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

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>

    <key>ProgramArguments</key>
    <array>
        <string>{daemon}</string>
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
    <string>Background</string>

    <key>StandardOutPath</key>
    <string>{log}/daemon.log</string>

    <key>StandardErrorPath</key>
    <string>{log}/daemon.log</string>{environment}
</dict>
</plist>
"#,
        label = LABEL,
        daemon = escape(&daemon.display().to_string()),
        log = escape(&log_directory.display().to_string()),
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
pub(crate) fn is_loaded() -> bool {
    std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{}/{LABEL}", user_id())])
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

/// Loads the job.
pub(crate) fn bootstrap(path: &Path) -> Result<()> {
    run(&[
        "bootstrap",
        &format!("gui/{}", user_id()),
        &path.display().to_string(),
    ])
}

/// Unloads the job.
pub(crate) fn bootout() -> Result<()> {
    run(&["bootout", &format!("gui/{}/{LABEL}", user_id())])
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

    fn rendered() -> String {
        plist(
            Path::new("/usr/local/bin/agentwatch-daemon"),
            Path::new("/Users/dev/.agentwatch"),
            &[],
        )
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
        assert!(text.contains("/usr/local/bin/agentwatch-daemon"));
        assert!(text.contains(LABEL));
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
        let text = plist(Path::new("/opt/a&b/daemon"), Path::new("/tmp/<logs>"), &[]);
        assert!(text.contains("/opt/a&amp;b/daemon"));
        assert!(text.contains("&lt;logs&gt;"));
        assert!(
            !text.contains("/opt/a&b/"),
            "raw ampersand would break the plist"
        );
    }
}
