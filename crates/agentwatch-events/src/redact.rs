//! Secret-safe command storage.

use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};

const REDACTED: &str = "[REDACTED]";
const CUSTOM_REDACTED: &str = "[REDACTED: custom]";
const MAX_CUSTOM_PATTERNS: usize = 100;
const MAX_PATTERN_BYTES: usize = 1_024;
const MAX_COMPILED_PATTERN_BYTES: usize = 1_048_576;

#[allow(clippy::expect_used, reason = "compile-time literal covered by tests")]
static BEARER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bbearer[ \t]+[A-Za-z0-9._~+/=-]{8,}")
        .expect("the built-in bearer-token regex is valid")
});

#[allow(clippy::expect_used, reason = "compile-time literal covered by tests")]
static URL_QUERY_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)([?&](?:[A-Za-z0-9_-]+[_-])?(?:token|secret|password|passwd|api[_-]?key|authorization)=)[^&#\s'\"]+"#,
    )
    .expect("the built-in URL-query credential regex is valid")
});

// Deliberately restricted to well-known key shapes. Named assignments such as
// `API_KEY=...` are handled separately, without guessing that every long
// alphanumeric word is a credential.
#[allow(clippy::expect_used, reason = "compile-time literal covered by tests")]
static KNOWN_API_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:sk-(?:proj-|ant-api[0-9]{2}-)?[A-Za-z0-9_-]{8,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{20,})",
    )
    .expect("the built-in API-key regex is valid")
});

/// A validated command sanitizer with optional user-defined regexes.
///
/// Custom expressions are applied after AgentWatch's structural and built-in
/// credential rules. Rust's regex engine guarantees linear-time matching, and
/// limits here keep an accidental configuration from consuming unreasonable
/// memory on the collector's write path.
#[derive(Debug, Clone, Default)]
pub struct CommandRedactor {
    custom: Vec<Regex>,
}

impl CommandRedactor {
    /// Starts with AgentWatch's built-in credential rules and no custom rules.
    #[must_use]
    pub const fn new() -> Self {
        Self { custom: Vec::new() }
    }

    /// Adds one custom Rust regex whose entire match will be replaced.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid expressions, expressions that can match an
    /// empty string, or configurations beyond the documented safety limits.
    pub fn add_pattern(&mut self, pattern: &str) -> Result<(), RedactionPatternError> {
        if self.custom.len() >= MAX_CUSTOM_PATTERNS {
            return Err(RedactionPatternError::TooMany {
                maximum: MAX_CUSTOM_PATTERNS,
            });
        }
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(RedactionPatternError::TooLong {
                maximum: MAX_PATTERN_BYTES,
            });
        }

        let regex = RegexBuilder::new(pattern)
            .size_limit(MAX_COMPILED_PATTERN_BYTES)
            .build()
            .map_err(RedactionPatternError::Invalid)?;
        if regex.is_match("") {
            return Err(RedactionPatternError::MatchesEmpty);
        }
        self.custom.push(regex);
        Ok(())
    }

    /// Number of configured custom expressions.
    #[must_use]
    pub fn custom_pattern_count(&self) -> usize {
        self.custom.len()
    }

    /// Replaces credential values while retaining useful command shape.
    #[must_use]
    pub fn redact(&self, command: &str) -> String {
        let mut redacted = redact_structural(command);
        redacted = BEARER_TOKEN
            .replace_all(&redacted, "Bearer [REDACTED]")
            .into_owned();
        redacted = KNOWN_API_KEY.replace_all(&redacted, REDACTED).into_owned();

        for pattern in &self.custom {
            redacted = redact_outside_markers(pattern, &redacted);
        }
        redacted
    }
}

fn redact_outside_markers(pattern: &Regex, command: &str) -> String {
    let mut output = String::with_capacity(command.len());
    let mut remaining = command;

    while let Some(marker_start) = remaining.find("[REDACTED") {
        let before = &remaining[..marker_start];
        output.push_str(&pattern.replace_all(before, CUSTOM_REDACTED));

        let marker = &remaining[marker_start..];
        let Some(marker_end) = marker.find(']').map(|index| index + 1) else {
            output.push_str(&pattern.replace_all(marker, CUSTOM_REDACTED));
            return output;
        };
        output.push_str(&marker[..marker_end]);
        remaining = &marker[marker_end..];
    }

    output.push_str(&pattern.replace_all(remaining, CUSTOM_REDACTED));
    output
}

/// A custom redaction expression was unsafe or invalid.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RedactionPatternError {
    /// The expression is not valid in Rust's regex syntax.
    #[error("invalid regex: {0}")]
    Invalid(regex::Error),
    /// Replacing an empty match would inject markers throughout every command.
    #[error("pattern must not match an empty string")]
    MatchesEmpty,
    /// One expression is unreasonably large for a command sanitizer.
    #[error("pattern exceeds the {maximum}-byte limit")]
    TooLong {
        /// Maximum accepted expression length.
        maximum: usize,
    },
    /// The configuration contains too many expressions.
    #[error("configuration exceeds the {maximum}-pattern limit")]
    TooMany {
        /// Maximum accepted expression count.
        maximum: usize,
    },
}

/// Applies AgentWatch's built-in command credential rules.
#[must_use]
pub fn redact_command(command: &str) -> String {
    CommandRedactor::new().redact(command)
}

fn redact_structural(command: &str) -> String {
    let lowercase = command.to_ascii_lowercase();
    if lowercase.contains("authorization:") || lowercase.contains("x-api-key:") {
        return "[REDACTED: credential header]".to_owned();
    }

    let mut output = Vec::new();
    let mut redact_next = false;
    let mut redact_through_quote = None;

    for word in command.split_whitespace() {
        if let Some(quote) = redact_through_quote {
            if word.ends_with(quote) {
                redact_through_quote = None;
            }
            continue;
        }
        if redact_next {
            output.push(REDACTED.to_owned());
            redact_next = false;
            redact_through_quote = open_quote(word);
            continue;
        }

        if is_secret_argument_name(word) {
            output.push(word.to_owned());
            redact_next = true;
            continue;
        }

        if let Some((name, value)) = word.split_once('=')
            && is_secret_argument_name(name)
        {
            output.push(format!("{name}={REDACTED}"));
            redact_through_quote = open_quote(value);
            continue;
        }

        output.push(redact_url_credentials(word));
    }

    output.join(" ")
}

fn open_quote(value: &str) -> Option<char> {
    let quote = value
        .chars()
        .next()
        .filter(|character| matches!(character, '\'' | '"'))?;
    (!value[quote.len_utf8()..].ends_with(quote)).then_some(quote)
}

fn is_secret_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase().replace('-', "_");
    normalized == "authorization"
        || normalized == "auth"
        || normalized == "password"
        || normalized == "passwd"
        || normalized == "pwd"
        || normalized == "secret"
        || normalized == "token"
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_password")
        || normalized.ends_with("_passwd")
        || normalized.ends_with("_pwd")
        || normalized.ends_with("_api_key")
        || normalized.ends_with("_access_key")
        || normalized.ends_with("_secret_key")
        || normalized.ends_with("_private_key")
        || normalized == "api_key"
        || normalized == "apikey"
}

fn is_secret_argument_name(name: &str) -> bool {
    let candidate = name
        .trim_matches(|character| matches!(character, '\'' | '"'))
        .trim_start_matches('-');
    !candidate.is_empty()
        && candidate
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        && is_secret_name(candidate)
}

fn redact_url_credentials(word: &str) -> String {
    let Some(scheme_end) = word.find("://") else {
        return word.to_owned();
    };
    let authority_start = scheme_end + 3;
    let authority_end = word[authority_start..]
        .find(['/', '?', '#'])
        .map_or(word.len(), |offset| authority_start + offset);
    let authority = &word[authority_start..authority_end];
    let password_redacted = authority.rfind('@').and_then(|at| {
        authority[..at].find(':').map(|colon| {
            let secret_start = authority_start + colon + 1;
            let secret_end = authority_start + at;
            format!("{}{REDACTED}{}", &word[..secret_start], &word[secret_end..])
        })
    });
    let url = password_redacted.as_deref().unwrap_or(word);
    URL_QUERY_SECRET
        .replace_all(url, "${1}[REDACTED]")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_assignments_flags_and_url_passwords() {
        let command = "API_KEY=sk-live curl --token abc postgres://me:hunter2@db/app";
        let safe = redact_command(command);
        assert_eq!(
            safe,
            "API_KEY=[REDACTED] curl --token [REDACTED] postgres://me:[REDACTED]@db/app"
        );
        assert!(!safe.contains("sk-live"));
        assert!(!safe.contains("hunter2"));
    }

    #[test]
    fn redacts_credential_headers_and_url_query_parameters() {
        assert_eq!(
            redact_command("curl -H 'X-Api-Key: private-value' https://example.test"),
            "[REDACTED: credential header]"
        );
        assert_eq!(
            redact_command("curl 'https://example.test/run?access_token=private&mode=fast'"),
            "curl 'https://example.test/run?access_token=[REDACTED]&mode=fast'"
        );
    }

    #[test]
    fn redacts_bearer_tokens_and_known_api_key_shapes_in_arbitrary_text() {
        let command = "curl -H 'X-Auth: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig' \
            --data sk-proj-AbCdEf0123456789 ghp_0123456789abcdefghij";
        let safe = redact_command(command);

        assert!(!safe.contains("eyJhbGci"));
        assert!(!safe.contains("sk-proj-"));
        assert!(!safe.contains("ghp_"));
        assert!(safe.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn redacts_additional_password_and_cloud_key_environment_names() {
        let safe =
            redact_command("DB_PWD=hunter2 AWS_SECRET_ACCESS_KEY=abc CLIENT_SECRET=def command");
        assert_eq!(
            safe,
            "DB_PWD=[REDACTED] AWS_SECRET_ACCESS_KEY=[REDACTED] CLIENT_SECRET=[REDACTED] command"
        );
    }

    #[test]
    fn ordinary_commands_remain_readable() {
        assert_eq!(
            redact_command("cargo test --workspace"),
            "cargo test --workspace"
        );
    }

    #[test]
    fn quoted_multiword_secrets_do_not_leak_the_tail() {
        assert_eq!(
            redact_command("API_KEY='two secret words' cargo test"),
            "API_KEY=[REDACTED] cargo test"
        );
        assert_eq!(
            redact_command("curl -H 'Authorization: Bearer abc' https://example.test"),
            "[REDACTED: credential header]"
        );
    }

    #[test]
    fn custom_patterns_replace_the_whole_match() {
        let mut redactor = CommandRedactor::new();
        redactor
            .add_pattern(r"ACME-[A-Z0-9]{8}")
            .expect("valid pattern");

        assert_eq!(
            redactor.redact("deploy --license ACME-12AB34CD now"),
            "deploy --license [REDACTED: custom] now"
        );
        assert_eq!(redactor.custom_pattern_count(), 1);
    }

    #[test]
    fn custom_redaction_is_idempotent_even_when_a_rule_matches_marker_text() {
        let mut redactor = CommandRedactor::new();
        redactor.add_pattern(r"[A-Z]{4,}").expect("valid pattern");

        let once = redactor.redact("echo PRIVATE TOKEN=already-secret");
        assert_eq!(redactor.redact(&once), once);
    }

    #[test]
    fn custom_patterns_that_match_empty_strings_are_rejected() {
        let mut redactor = CommandRedactor::new();
        let error = redactor.add_pattern(".*").expect_err("unsafe pattern");
        assert!(matches!(error, RedactionPatternError::MatchesEmpty));
    }
}
