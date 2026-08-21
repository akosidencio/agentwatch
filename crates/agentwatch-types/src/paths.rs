//! Filesystem locations AgentWatch uses.
//!
//! Every location is derived from a single root so tests can redirect the whole
//! application into a temporary directory.

use std::path::{Path, PathBuf};

/// Environment variable that overrides the data directory.
pub const DATA_DIR_ENV: &str = "AGENTWATCH_DIR";

/// File name for optional custom command-redaction expressions.
pub const REDACTION_PATTERNS_FILENAME: &str = "redaction-patterns.txt";

/// Resolved locations for the daemon's data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// Builds paths rooted at an explicit directory.
    #[must_use]
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolves the data directory from the environment.
    ///
    /// Honours [`DATA_DIR_ENV`] first, then falls back to
    /// `~/Library/Application Support/AgentWatch`.
    ///
    /// # Errors
    ///
    /// Returns an error if neither the override nor `HOME` is set.
    pub fn from_env() -> Result<Self, PathError> {
        if let Some(dir) = std::env::var_os(DATA_DIR_ENV)
            && !dir.is_empty()
        {
            return Ok(Self::with_root(dir));
        }

        let home = std::env::var_os("HOME").ok_or(PathError::NoHome)?;
        if home.is_empty() {
            return Err(PathError::NoHome);
        }

        Ok(Self::with_root(
            Path::new(&home)
                .join("Library")
                .join("Application Support")
                .join("AgentWatch"),
        ))
    }

    /// The data directory itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The Unix domain socket the daemon listens on.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.root.join("agentwatch.sock")
    }

    /// The SQLite database file.
    #[must_use]
    pub fn database(&self) -> PathBuf {
        self.root.join("agentwatch.db")
    }

    /// Optional custom command-redaction expressions, one Rust regex per line.
    #[must_use]
    pub fn redaction_patterns(&self) -> PathBuf {
        self.root.join(REDACTION_PATTERNS_FILENAME)
    }

    /// Marker whose presence means collection is paused.
    ///
    /// A file rather than a socket command because the daemon's socket carries
    /// events in one direction only, and because a pause should survive a
    /// daemon restart — a pause that quietly lifts itself is worse than no
    /// pause at all.
    #[must_use]
    pub fn pause_marker(&self) -> PathBuf {
        self.root.join("paused")
    }

    /// Whether collection is currently paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.pause_marker().exists()
    }

    /// Creates the data directory if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn ensure_root(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }
}

/// Failure to resolve the data directory.
///
/// Hand-written rather than derived so this crate keeps its tiny dependency set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathError {
    /// Neither the override nor `HOME` was set.
    NoHome,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHome => {
                f.write_str("cannot locate the data directory: set AGENTWATCH_DIR or HOME")
            }
        }
    }
}

impl std::error::Error for PathError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_every_location_from_the_root() {
        let paths = Paths::with_root("/tmp/aw");
        assert_eq!(paths.socket(), Path::new("/tmp/aw/agentwatch.sock"));
        assert_eq!(paths.database(), Path::new("/tmp/aw/agentwatch.db"));
        assert_eq!(
            paths.redaction_patterns(),
            Path::new("/tmp/aw/redaction-patterns.txt")
        );
    }
}
