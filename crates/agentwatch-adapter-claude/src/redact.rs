//! Turning content into metadata.

use sha2::{Digest, Sha256};

/// Summarizes text as a length and a digest, discarding the text itself.
///
/// The digest lets repeated prompts be recognized as repeats without anyone
/// being able to read what they said.
pub(crate) fn summarize(text: &str) -> (u32, String) {
    let char_count = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);

    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();

    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }

    (char_count, hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_characters_not_bytes() {
        let (count, _) = summarize("héllo");
        assert_eq!(count, 5);
    }

    #[test]
    fn hashes_the_empty_string_to_the_known_digest() {
        let (count, digest) = summarize("");
        assert_eq!(count, 0);
        assert_eq!(
            digest,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn identical_text_hashes_identically() {
        let (_, first) = summarize("refactor the auth module");
        let (_, second) = summarize("refactor the auth module");
        assert_eq!(first, second);
    }
}
