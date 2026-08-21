//! Secret-safe command storage.

/// Replaces common command-line credential values while retaining the command
/// shape needed for activity and debugging.
#[must_use]
pub fn redact_command(command: &str) -> String {
    if command.to_ascii_lowercase().contains("authorization:") {
        return "[REDACTED: authorization header]".to_owned();
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
            output.push("[REDACTED]".to_owned());
            redact_next = false;
            redact_through_quote = open_quote(word);
            continue;
        }

        let bare = word.trim_matches(|character| matches!(character, '\'' | '"'));
        if is_secret_name(bare.trim_start_matches('-')) {
            output.push(word.to_owned());
            redact_next = true;
            continue;
        }

        if let Some((name, value)) = word.split_once('=')
            && is_secret_name(name.trim_start_matches('-'))
        {
            output.push(format!("{name}=[REDACTED]"));
            redact_through_quote = open_quote(value);
            continue;
        }

        output.push(redact_url_password(word));
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
        || normalized == "password"
        || normalized == "passwd"
        || normalized == "token"
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_password")
        || normalized.ends_with("_api_key")
        || normalized == "api_key"
        || normalized == "apikey"
}

fn redact_url_password(word: &str) -> String {
    let Some(scheme_end) = word.find("://") else {
        return word.to_owned();
    };
    let authority_start = scheme_end + 3;
    let authority_end = word[authority_start..]
        .find(['/', '?', '#'])
        .map_or(word.len(), |offset| authority_start + offset);
    let authority = &word[authority_start..authority_end];
    let Some(at) = authority.rfind('@') else {
        return word.to_owned();
    };
    let Some(colon) = authority[..at].find(':') else {
        return word.to_owned();
    };

    let secret_start = authority_start + colon + 1;
    let secret_end = authority_start + at;
    format!("{}[REDACTED]{}", &word[..secret_start], &word[secret_end..])
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
    fn ordinary_commands_remain_readable() {
        assert_eq!(
            redact_command("cargo test --workspace"),
            "cargo test --workspace"
        );
    }

    #[test]
    fn quoted_multword_secrets_do_not_leak_the_tail() {
        assert_eq!(
            redact_command("API_KEY='two secret words' cargo test"),
            "API_KEY=[REDACTED] cargo test"
        );
        assert_eq!(
            redact_command("curl -H 'Authorization: Bearer abc' https://example.test"),
            "[REDACTED: authorization header]"
        );
    }
}
