//! Resolving a working directory to the repository that contains it.
//!
//! A session's `cwd` is wherever the agent happened to be started, which is
//! frequently a subdirectory. Left alone that splits one repository across
//! dozens of "projects" — the real corpus this was built against showed 170
//! projects for roughly 40 repositories, which makes the headline breakdown
//! close to useless.
//!
//! Repositories are resolved *alongside* the working directory rather than
//! replacing it. The directory is still the honest answer to "where was this
//! session started"; the repository is the useful answer to "what was it
//! working on".

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Marker that identifies a repository root.
///
/// A directory for an ordinary clone, a file for a worktree or submodule, so
/// existence rather than kind is the test.
const GIT_MARKER: &str = ".git";

/// Highest number of parent directories to examine.
///
/// A guard against pathological paths and symlink loops, not a real limit:
/// nothing legitimate nests this deep.
const MAX_DEPTH: usize = 64;

/// Resolves working directories to repository roots, remembering answers.
///
/// The cache matters: importing history asks the same question tens of
/// thousands of times, and every miss is a walk up the filesystem.
#[derive(Debug, Default)]
pub struct RepositoryResolver {
    cache: HashMap<String, Option<String>>,
}

impl RepositoryResolver {
    /// Creates an empty resolver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Finds the repository containing a directory.
    ///
    /// Returns `None` when the directory is not inside a repository, or no
    /// longer exists — which is the normal case when reading old transcripts
    /// for projects that have since been deleted or moved.
    pub fn resolve(&mut self, directory: &str) -> Option<String> {
        if let Some(cached) = self.cache.get(directory) {
            return cached.clone();
        }

        let resolved = find_repository_root(Path::new(directory))
            .map(|root| root.to_string_lossy().into_owned());
        self.cache.insert(directory.to_owned(), resolved.clone());
        resolved
    }

    /// How many distinct directories have been resolved.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Whether anything has been resolved yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

/// Walks upward looking for a repository marker.
fn find_repository_root(start: &Path) -> Option<PathBuf> {
    let mut current = start;

    for _ in 0..MAX_DEPTH {
        if current.join(GIT_MARKER).exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }

    None
}

/// The last component of a path, for display.
///
/// Falls back to the whole path for roots and other component-less inputs.
#[must_use]
pub fn display_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds `root/.git` plus a nested directory inside it.
    fn repository_with_subdirectory() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("acme-api");
        let nested = root.join("packages").join("core").join("src");
        std::fs::create_dir_all(&nested).expect("create");
        std::fs::create_dir_all(root.join(GIT_MARKER)).expect("create marker");
        (directory, root, nested)
    }

    #[test]
    fn a_subdirectory_resolves_to_its_repository_root() {
        let (_guard, root, nested) = repository_with_subdirectory();
        let mut resolver = RepositoryResolver::new();

        assert_eq!(
            resolver.resolve(&nested.to_string_lossy()),
            Some(root.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn the_root_itself_resolves_to_itself() {
        let (_guard, root, _nested) = repository_with_subdirectory();
        let mut resolver = RepositoryResolver::new();

        assert_eq!(
            resolver.resolve(&root.to_string_lossy()),
            Some(root.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn a_worktree_marker_file_counts_as_a_root() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("worktree");
        std::fs::create_dir_all(&root).expect("create");
        std::fs::write(root.join(GIT_MARKER), "gitdir: /elsewhere").expect("write");

        let mut resolver = RepositoryResolver::new();
        assert_eq!(
            resolver.resolve(&root.to_string_lossy()),
            Some(root.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn a_directory_outside_any_repository_resolves_to_nothing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let plain = directory.path().join("not-a-repo");
        std::fs::create_dir_all(&plain).expect("create");

        let mut resolver = RepositoryResolver::new();
        assert_eq!(resolver.resolve(&plain.to_string_lossy()), None);
    }

    #[test]
    fn a_vanished_directory_resolves_to_nothing_rather_than_failing() {
        let mut resolver = RepositoryResolver::new();
        assert_eq!(resolver.resolve("/nonexistent/deleted/project"), None);
    }

    #[test]
    fn repeated_lookups_are_cached() {
        let (_guard, _root, nested) = repository_with_subdirectory();
        let mut resolver = RepositoryResolver::new();
        let directory = nested.to_string_lossy().into_owned();

        resolver.resolve(&directory);
        resolver.resolve(&directory);
        resolver.resolve(&directory);

        assert_eq!(
            resolver.len(),
            1,
            "one directory should cost one cache entry"
        );
    }

    #[test]
    fn a_missing_answer_is_cached_too() {
        let mut resolver = RepositoryResolver::new();
        resolver.resolve("/nonexistent/a");
        resolver.resolve("/nonexistent/a");
        assert_eq!(resolver.len(), 1);
    }

    #[test]
    fn display_name_is_the_last_component() {
        assert_eq!(display_name("/Users/dev/projects/acme-api"), "acme-api");
        assert_eq!(display_name("/"), "/");
    }
}
