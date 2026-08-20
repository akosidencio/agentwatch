//! Classifying paths by what it would mean for an agent to read them.
//!
//! Classification works from paths and names only. Deciding whether a file
//! holds a secret by reading it would mean AgentWatch itself handling every
//! secret on the machine, which is a worse position than the one it is trying
//! to report on.
//!
//! The cost of that choice is honest false negatives: a credential in a file
//! named `notes.txt` is invisible here. The alternative is worse.

use serde::{Deserialize, Serialize};

/// How much it matters that something touched this path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Nothing notable.
    Normal,
    /// Configuration that commonly holds credentials.
    Sensitive,
    /// Private key material, or a cloud credential store.
    HighlySensitive,
}

impl Sensitivity {
    /// The stable string stored in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Sensitive => "sensitive",
            Self::HighlySensitive => "highly_sensitive",
        }
    }

    /// Whether this is worth surfacing at all.
    #[must_use]
    pub const fn is_notable(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

/// Directory segments whose contents are private key or credential stores.
const HIGHLY_SENSITIVE_DIRECTORIES: [&str; 4] = [".ssh", ".gnupg", ".aws", ".docker"];

/// Directory segments holding credentialed configuration.
const SENSITIVE_DIRECTORIES: [&str; 3] = [".kube", "gcloud", ".config/gcloud"];

/// Extensions that are key material regardless of where they live.
const HIGHLY_SENSITIVE_EXTENSIONS: [&str; 6] = ["pem", "key", "p12", "pfx", "jks", "keystore"];

/// Exact file names that are private keys.
const PRIVATE_KEY_NAMES: [&str; 5] = [
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "id_dsa",
    ".git-credentials",
];

/// Exact file names that commonly carry tokens.
const SENSITIVE_NAMES: [&str; 6] = [
    ".env",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".dockercfg",
    "credentials",
];

/// Name prefixes that suggest credentials.
const SENSITIVE_PREFIXES: [&str; 3] = ["credentials", "secrets", ".env."];

/// Classifies a path.
///
/// Matching is on path segments rather than on a home-relative prefix, so
/// `~/.ssh/id_ed25519` and a copy of it under `/tmp/backup/.ssh/id_ed25519` are
/// treated alike — the second is if anything more interesting.
#[must_use]
pub fn classify(path: &str) -> Sensitivity {
    let normalized = path.replace('\\', "/");
    let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();

    let Some(name) = segments.last().copied() else {
        return Sensitivity::Normal;
    };
    let lowercase = name.to_ascii_lowercase();

    if PRIVATE_KEY_NAMES.contains(&lowercase.as_str()) {
        return Sensitivity::HighlySensitive;
    }

    if let Some(extension) = lowercase.rsplit_once('.').map(|(_, ext)| ext)
        && HIGHLY_SENSITIVE_EXTENSIONS.contains(&extension)
    {
        return Sensitivity::HighlySensitive;
    }

    // A directory match applies to everything beneath it, but not to the
    // directory's own name appearing as a leaf.
    let parents = &segments[..segments.len().saturating_sub(1)];
    if parents
        .iter()
        .any(|segment| HIGHLY_SENSITIVE_DIRECTORIES.contains(segment))
    {
        return Sensitivity::HighlySensitive;
    }

    if SENSITIVE_NAMES.contains(&lowercase.as_str())
        || SENSITIVE_PREFIXES
            .iter()
            .any(|prefix| lowercase.starts_with(prefix))
    {
        return Sensitivity::Sensitive;
    }

    if parents
        .iter()
        .any(|segment| SENSITIVE_DIRECTORIES.contains(segment))
    {
        return Sensitivity::Sensitive;
    }

    Sensitivity::Normal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_private_keys_are_highly_sensitive() {
        assert_eq!(
            classify("/Users/dev/.ssh/id_ed25519"),
            Sensitivity::HighlySensitive
        );
        assert_eq!(
            classify("/Users/dev/.ssh/config"),
            Sensitivity::HighlySensitive
        );
    }

    #[test]
    fn cloud_credential_stores_are_highly_sensitive() {
        assert_eq!(
            classify("/Users/dev/.aws/credentials"),
            Sensitivity::HighlySensitive
        );
        assert_eq!(
            classify("/Users/dev/.docker/config.json"),
            Sensitivity::HighlySensitive
        );
        assert_eq!(
            classify("/Users/dev/.gnupg/secring.gpg"),
            Sensitivity::HighlySensitive
        );
    }

    #[test]
    fn key_material_is_highly_sensitive_wherever_it_lives() {
        assert_eq!(classify("/tmp/server.pem"), Sensitivity::HighlySensitive);
        assert_eq!(
            classify("./certs/private.KEY"),
            Sensitivity::HighlySensitive
        );
        assert_eq!(classify("/opt/app/store.jks"), Sensitivity::HighlySensitive);
    }

    #[test]
    fn a_copy_outside_home_is_still_classified() {
        assert_eq!(
            classify("/tmp/backup/.ssh/id_rsa"),
            Sensitivity::HighlySensitive,
            "an exfiltrated copy is more interesting, not less"
        );
    }

    #[test]
    fn env_files_are_sensitive() {
        assert_eq!(classify("/work/acme/.env"), Sensitivity::Sensitive);
        assert_eq!(
            classify("/work/acme/.env.production"),
            Sensitivity::Sensitive
        );
        assert_eq!(classify("/work/.npmrc"), Sensitivity::Sensitive);
        assert_eq!(classify("/work/.netrc"), Sensitivity::Sensitive);
    }

    #[test]
    fn credential_named_files_are_sensitive() {
        assert_eq!(classify("/work/credentials.json"), Sensitivity::Sensitive);
        assert_eq!(classify("/work/secrets.yaml"), Sensitivity::Sensitive);
    }

    #[test]
    fn kubernetes_and_gcloud_config_is_sensitive() {
        assert_eq!(classify("/Users/dev/.kube/config"), Sensitivity::Sensitive);
        assert_eq!(
            classify("/Users/dev/.config/gcloud/application_default_credentials.json"),
            Sensitivity::Sensitive
        );
    }

    #[test]
    fn ordinary_source_files_are_normal() {
        assert_eq!(classify("/work/acme/src/main.rs"), Sensitivity::Normal);
        assert_eq!(classify("/work/acme/README.md"), Sensitivity::Normal);
        assert_eq!(classify("Cargo.toml"), Sensitivity::Normal);
    }

    #[test]
    fn a_file_merely_named_like_a_sensitive_directory_is_normal() {
        assert_eq!(
            classify("/work/docs/.kube"),
            Sensitivity::Normal,
            "a directory rule should not fire on a leaf of the same name"
        );
    }

    #[test]
    fn an_empty_path_is_normal() {
        assert_eq!(classify(""), Sensitivity::Normal);
        assert_eq!(classify("/"), Sensitivity::Normal);
    }

    #[test]
    fn severity_orders_from_least_to_most_serious() {
        assert!(Sensitivity::HighlySensitive > Sensitivity::Sensitive);
        assert!(Sensitivity::Sensitive > Sensitivity::Normal);
    }

    #[test]
    fn only_non_normal_classifications_are_notable() {
        assert!(!Sensitivity::Normal.is_notable());
        assert!(Sensitivity::Sensitive.is_notable());
    }
}
