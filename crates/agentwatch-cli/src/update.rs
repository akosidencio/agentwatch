//! Replacing the installed binaries with a published release.
//!
//! Exists because "download the archive again" is not the whole job. The
//! launchd jobs keep running the *old* image until they are restarted — the
//! plist still points at the right path, but the process holding that inode is
//! the one that was already running. Anyone updating by hand gets a new binary
//! on their PATH and a collector that is still the previous build, with nothing
//! saying so. That restart is the part this command exists to not forget.
//!
//! # Why it shells out to curl
//!
//! There is no network client in this codebase, and that is a claim worth
//! keeping true: it is the honest answer to "does this thing phone home?".
//! `curl` is invoked, once, only when someone types `update` — the same
//! transport `install.sh` already requires. The checksum is then verified
//! in-process with `sha2`, which is already here, because verification is the
//! one step that should not be delegated to a second process.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use sha2::{Digest, Sha256};

use crate::{service, theme};

/// What to update to.
#[derive(Debug, Clone)]
pub(crate) struct Options {
    /// Release tag to install. `None` means the latest published release.
    pub(crate) version: Option<String>,
    /// Show what would happen and exit.
    pub(crate) dry_run: bool,
    /// Replace without asking.
    pub(crate) assume_yes: bool,
}

/// Where releases come from.
const REPOSITORY: &str = "akosidencio/agentwatch";

/// Files a release archive is expected to contain.
///
/// The executable is required. The status item is installed only if the archive
/// carries it and it is already alongside — updating is not the moment to add a
/// component somebody chose not to have.
const REQUIRED: &str = "agentwatch";

/// The optional companion.
const COMPANION: &str = "agentwatch-menubar";

/// This build's version.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Fetches a release and puts it in place.
pub(crate) fn run(options: &Options) -> Result<()> {
    let destination = install_directory()?;
    let target = release_target();
    let archive = archive_name(target);
    let tag = options.version.as_deref().map(normalize_tag);
    let version = release_version(tag.as_deref())?;
    let base = download_base(tag.as_deref());

    theme::heading("AgentWatch update");
    let field = |name: &str| theme::paint(&format!("{name:<16}"), theme::MUTED);
    println!("  {}{CURRENT}", field("installed"));
    println!("  {}{}", field("updating to"), version);
    println!("  {}{}", field("into"), destination.display());
    println!("  {}{}", field("architecture"), target);

    if looks_like_a_build_tree(&destination) {
        println!();
        println!(
            "  {}",
            theme::paint(
                "This is a build directory, not an install. `cargo build --release` is the \
                 update here.",
                theme::WARN
            )
        );
    }

    // Downloaded into the destination directory rather than a system temporary
    // one, so putting the binaries in place is a rename on the same filesystem
    // — atomic, and it cannot half-succeed across volumes.
    let staging = destination.join(format!(".agentwatch-update-{}", std::process::id()));
    std::fs::create_dir_all(&staging).with_context(|| {
        format!(
            "creating {} — is {} writable?",
            staging.display(),
            destination.display()
        )
    })?;
    let result = fetch_and_place(&staging, &destination, &base, &archive, &version, options);

    // Whatever happened, do not leave a dot-directory in someone's bin.
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Downloads, verifies, and — unless this is a dry run — installs.
fn fetch_and_place(
    staging: &Path,
    destination: &Path,
    base: &str,
    archive: &str,
    version: &str,
    options: &Options,
) -> Result<()> {
    println!();
    println!("  downloading {archive}");
    let archive_path = staging.join(archive);
    curl(&format!("{base}/{archive}"), &archive_path).with_context(|| {
        format!("could not download {base}/{archive} — is that version published?")
    })?;

    println!("  downloading SHA256SUMS");
    let sums_path = staging.join("SHA256SUMS");
    curl(&format!("{base}/SHA256SUMS"), &sums_path)?;

    // Verified before anything is unpacked, never after: a substituted archive
    // should not reach the point of being a file on the PATH.
    println!("  verifying checksum");
    let sums = std::fs::read_to_string(&sums_path).context("reading SHA256SUMS")?;
    let expected = expected_digest(&sums, archive)
        .with_context(|| format!("{archive} is not listed in SHA256SUMS"))?;
    let actual = digest(&archive_path)?;
    if expected != actual {
        bail!(
            "checksum mismatch, refusing to install\n  expected: {expected}\n  actual:   {actual}"
        );
    }
    println!(
        "  {}",
        theme::paint(&format!("ok ({actual})"), theme::MUTED)
    );

    unpack(&archive_path, staging)?;

    let new = staging.join(REQUIRED);
    if !new.is_file() {
        bail!("{archive} did not contain {REQUIRED}");
    }

    // Only components that are already installed are replaced.
    let mut moves = vec![(new.clone(), destination.join(REQUIRED))];
    if staging.join(COMPANION).is_file() && destination.join(COMPANION).is_file() {
        moves.push((staging.join(COMPANION), destination.join(COMPANION)));
    }

    let jobs: Vec<service::Job> = service::JOBS
        .into_iter()
        .filter(|job| service::is_loaded(*job))
        .collect();

    println!();
    // A downgrade is a legitimate thing to ask for — pinning `--version` to
    // step back off a bad release is exactly what that flag is for — but it
    // must never be what "update" quietly does because the latest published
    // release happens to be older than the build in hand.
    let direction = if is_older(version, CURRENT) {
        theme::paint(&format!("DOWNGRADE  {CURRENT} → {version}"), theme::WARN)
    } else if version == CURRENT {
        theme::bold(&format!("REPAIR     {CURRENT}"))
    } else {
        theme::bold(&format!("{CURRENT} → {version}"))
    };
    println!("  {direction}");
    println!();
    for (_, into) in &moves {
        println!("  {}{}", theme::label("replace"), into.display());
    }
    for job in &jobs {
        println!("  {}{}", theme::label("restart"), job.label());
    }

    if options.dry_run {
        println!();
        println!(
            "  {}",
            theme::paint("Dry run — nothing was replaced.", theme::MUTED)
        );
        return Ok(());
    }
    if !options.assume_yes && !crate::confirm(&format!("Update to {version}?"))? {
        println!(
            "\n  {}",
            theme::paint("Cancelled. Nothing was replaced.", theme::MUTED)
        );
        return Ok(());
    }

    // Running a downloaded executable is materially different from inspecting
    // it. Do it only after the user has consented, never during `--dry-run`, and
    // require the binary to agree with the release page that selected it.
    let reported = reported_version(&new)?;
    if reported != version {
        bail!(
            "downloaded binary reports version {reported}, but the release is {version}; refusing to install"
        );
    }

    println!();
    for (from, into) in &moves {
        // Renamed rather than written over: the rename swaps the directory
        // entry, so a process already running the old image keeps the inode it
        // started with instead of having the file changed underneath it.
        std::fs::set_permissions(from, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .with_context(|| format!("setting permissions on {}", from.display()))?;
        std::fs::rename(from, into).with_context(|| format!("replacing {}", into.display()))?;
        println!(
            "  {}{}{}",
            theme::paint("✓ ", theme::GOOD),
            theme::label("replaced"),
            theme::paint(&into.display().to_string(), theme::MUTED)
        );
    }

    // The reason this is a command and not a documented sequence of steps.
    // Failure must reach the exit status: otherwise automation sees success and
    // the closing message lies while launchd still holds the old inode.
    restart_jobs_with(&jobs, service::restart)?;

    println!();
    println!(
        "  {} {}",
        theme::bold("Updated to"),
        theme::paint(version, theme::GOOD)
    );
    if jobs.is_empty() {
        println!(
            "  {}",
            theme::paint(
                "Nothing was running to restart. `agentwatch init` sets it up.",
                theme::MUTED
            )
        );
    } else {
        println!(
            "  {}",
            theme::paint(
                "The running AgentWatch jobs are using the new build. Hook entries were left \
                 alone: they already point here.",
                theme::MUTED
            )
        );
    }
    Ok(())
}

/// Restarts every job, reporting all failures before returning an error.
fn restart_jobs_with(
    jobs: &[service::Job],
    mut restart: impl FnMut(service::Job) -> Result<()>,
) -> Result<()> {
    let mut failures = Vec::new();
    for job in jobs {
        match restart(*job) {
            Ok(()) => println!(
                "  {}{}{}",
                theme::paint("✓ ", theme::GOOD),
                theme::label("restarted"),
                theme::paint(job.label(), theme::MUTED)
            ),
            Err(error) => {
                println!(
                    "  {}{}{}",
                    theme::paint("✗ ", theme::BAD),
                    theme::label("restart"),
                    theme::paint(
                        &format!("{error:#} — run `agentwatch init` to repair"),
                        theme::BAD
                    )
                );
                failures.push(job.label());
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        bail!("failed to restart {}", failures.join(", "))
    }
}

/// Where the running executable lives, which is what gets replaced.
fn install_directory() -> Result<PathBuf> {
    let current = std::env::current_exe().context("locating the running executable")?;
    current
        .parent()
        .map(Path::to_path_buf)
        .context("the running executable has no directory")
}

/// Whether this looks like `target/debug` or `target/release`.
fn looks_like_a_build_tree(directory: &Path) -> bool {
    matches!(
        directory.file_name().and_then(|name| name.to_str()),
        Some("debug" | "release")
    ) && directory
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("target")
}

/// The release this build would update to.
///
/// Read from the compiled target rather than from `uname`, which reports the
/// host and not the binary: an x86_64 build running under Rosetta on Apple
/// silicon would otherwise update itself to an arm64 archive.
const fn release_target() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-apple-darwin"
    }
}

/// The archive name for a target.
fn archive_name(target: &str) -> String {
    format!("agentwatch-{target}.tar.gz")
}

/// Accepts `0.1.2` and `v0.1.2` alike.
fn normalize_tag(version: &str) -> String {
    let trimmed = version.trim();
    if trimmed.starts_with('v') {
        trimmed.to_owned()
    } else {
        format!("v{trimmed}")
    }
}

/// The download directory for a tag, or for the latest release.
fn download_base(tag: Option<&str>) -> String {
    match tag {
        Some(tag) => format!("https://github.com/{REPOSITORY}/releases/download/{tag}"),
        None => format!("https://github.com/{REPOSITORY}/releases/latest/download"),
    }
}

/// Resolves the release version without executing anything from its archive.
fn release_version(tag: Option<&str>) -> Result<String> {
    if let Some(tag) = tag {
        return Ok(tag.trim_start_matches('v').to_owned());
    }

    let url = format!("https://github.com/{REPOSITORY}/releases/latest");
    let output = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--tlsv1.2",
            "-o",
            "/dev/null",
            "--write-out",
            "%{url_effective}",
            &url,
        ])
        .output()
        .context("resolving the latest release")?;
    if !output.status.success() {
        bail!("could not resolve the latest release");
    }

    let effective = String::from_utf8(output.stdout).context("release URL was not UTF-8")?;
    version_from_release_url(&effective)
        .with_context(|| format!("could not read a version from {effective}"))
}

/// Extracts `0.1.2` from GitHub's `/releases/tag/v0.1.2` redirect target.
fn version_from_release_url(url: &str) -> Option<String> {
    let tag = url.trim().split("/releases/tag/").nth(1)?;
    let tag = tag.split(['?', '#']).next()?.trim_start_matches('v');
    (!tag.is_empty()).then(|| tag.to_owned())
}

/// Pulls one file down.
fn curl(url: &str, into: &Path) -> Result<()> {
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "--proto", "=https", "--tlsv1.2", "-o"])
        .arg(into)
        .arg(url)
        .status()
        .context("running curl — is it installed?")?;
    if !status.success() {
        bail!("curl failed for {url}");
    }
    Ok(())
}

/// The digest `SHA256SUMS` claims for one file.
///
/// The file lists every architecture's archive, and the names share a prefix, so
/// the match is on the whole final path component rather than a substring.
fn expected_digest<'a>(sums: &'a str, archive: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.split_once(char::is_whitespace)?;
        let name = name
            .trim()
            .trim_start_matches("*./")
            .trim_start_matches("./");
        (name.rsplit('/').next() == Some(archive)).then_some(digest.trim())
    })
}

/// The SHA-256 of a file, as lowercase hex.
fn digest(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(format!("{:x}", <Sha256 as Digest>::digest(&bytes)))
}

/// Unpacks the archive next to itself.
fn unpack(archive: &Path, into: &Path) -> Result<()> {
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .status()
        .context("running tar")?;
    if !status.success() {
        bail!("could not unpack {}", archive.display());
    }
    Ok(())
}

/// Whether `candidate` is an earlier release than `current`.
///
/// Compares the numeric triple and nothing else. A version this cannot parse is
/// reported as *not* older, so an unexpected format shows up as an ordinary
/// update rather than as a scary warning nobody can explain.
fn is_older(candidate: &str, current: &str) -> bool {
    let triple = |version: &str| {
        let mut parts = version.trim_start_matches('v').split('.');
        let mut next = || parts.next()?.parse::<u64>().ok();
        Some((next()?, next()?, next()?))
    };
    match (triple(candidate), triple(current)) {
        (Some(candidate), Some(current)) => candidate < current,
        _ => false,
    }
}

/// Asks a downloaded binary what version it is.
///
/// The release URL supplies the expected version without executing the binary.
/// This second check, performed only after consent, ensures the archive actually
/// contains the version selected by that URL or explicit tag.
fn reported_version(binary: &Path) -> Result<String> {
    std::fs::set_permissions(binary, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .with_context(|| format!("setting permissions on {}", binary.display()))?;

    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .with_context(|| format!("running {} --version", binary.display()))?;
    if !output.status.success() {
        bail!("{} did not run", binary.display());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .last()
        .map(str::to_owned)
        .context("could not read the downloaded version")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_is_accepted_with_or_without_its_v() {
        assert_eq!(normalize_tag("0.1.2"), "v0.1.2");
        assert_eq!(normalize_tag("v0.1.2"), "v0.1.2");
        assert_eq!(normalize_tag("  0.1.2  "), "v0.1.2");
    }

    #[test]
    fn no_version_means_the_latest_release() {
        assert!(download_base(None).ends_with("/releases/latest/download"));
        assert!(download_base(Some("v0.1.2")).ends_with("/releases/download/v0.1.2"));
    }

    #[test]
    fn the_latest_release_redirect_yields_its_version() {
        assert_eq!(
            version_from_release_url(
                "https://github.com/akosidencio/agentwatch/releases/tag/v0.1.2"
            )
            .as_deref(),
            Some("0.1.2")
        );
    }

    #[test]
    fn restart_failure_makes_the_update_fail() {
        let error = restart_jobs_with(&[service::Job::Daemon], |_| {
            anyhow::bail!("launchctl failed")
        })
        .expect_err("restart failures must reach the command exit status");

        assert!(format!("{error:#}").contains(service::Job::Daemon.label()));
    }

    #[test]
    fn the_digest_for_this_architecture_is_the_one_read() {
        // Both architectures are listed, and the names share a prefix. Matching
        // loosely here would install the wrong binary and verify it happily.
        let sums = "\
aaaa  ./agentwatch-aarch64-apple-darwin.tar.gz
bbbb  ./agentwatch-x86_64-apple-darwin.tar.gz
cccc  install.sh
";
        assert_eq!(
            expected_digest(sums, "agentwatch-x86_64-apple-darwin.tar.gz"),
            Some("bbbb")
        );
        assert_eq!(
            expected_digest(sums, "agentwatch-aarch64-apple-darwin.tar.gz"),
            Some("aaaa")
        );
        assert_eq!(expected_digest(sums, "agentwatch-riscv.tar.gz"), None);
    }

    #[test]
    fn a_plain_shasum_line_is_read_too() {
        // `shasum -a 256 file` writes no `./` prefix; the release job's
        // `sha256sum ./*.tar.gz` does. Both have to parse.
        let sums = "dddd  agentwatch-x86_64-apple-darwin.tar.gz\n";
        assert_eq!(
            expected_digest(sums, "agentwatch-x86_64-apple-darwin.tar.gz"),
            Some("dddd")
        );
    }

    #[test]
    fn the_archive_matches_the_compiled_target() {
        let name = archive_name(release_target());
        assert!(name.starts_with("agentwatch-"));
        assert!(name.ends_with("-apple-darwin.tar.gz"));
        assert_eq!(
            name.contains("aarch64"),
            cfg!(target_arch = "aarch64"),
            "updating must follow the binary's architecture, not the host's"
        );
    }

    #[test]
    fn stepping_backwards_is_recognised() {
        assert!(is_older("0.1.1", "0.1.2"));
        assert!(is_older("0.1.9", "0.2.0"));
        assert!(is_older("0.9.9", "1.0.0"));
        assert!(!is_older("0.1.3", "0.1.2"));
        assert!(!is_older("0.1.2", "0.1.2"));
        assert!(
            is_older("v0.1.1", "0.1.2"),
            "a v prefix must not confuse it"
        );
        // Unparseable is reported as not-older: better an ordinary update line
        // than a warning that cannot be explained.
        assert!(!is_older("nightly", "0.1.2"));
        assert!(!is_older("0.1", "0.1.2"));
    }

    #[test]
    fn a_build_directory_is_recognised() {
        assert!(looks_like_a_build_tree(Path::new(
            "/w/agentwatch/target/debug"
        )));
        assert!(looks_like_a_build_tree(Path::new(
            "/w/agentwatch/target/release"
        )));
        assert!(!looks_like_a_build_tree(Path::new("/Users/a/.local/bin")));
        assert!(!looks_like_a_build_tree(Path::new("/usr/local/bin")));
    }

    #[test]
    fn the_digest_helper_agrees_with_shasum() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"agentwatch").expect("write");
        // Known SHA-256 of the literal bytes "agentwatch".
        let expected = String::from_utf8(
            std::process::Command::new("shasum")
                .args(["-a", "256"])
                .arg(file.path())
                .output()
                .expect("shasum")
                .stdout,
        )
        .expect("utf8");
        let expected = expected.split_whitespace().next().expect("digest");
        assert_eq!(digest(file.path()).expect("digest"), expected);
    }
}
