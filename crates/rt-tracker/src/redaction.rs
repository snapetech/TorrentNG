use url::Url;

const MAX_LINE_SPLIT_KEY_BYTES: usize = 256;

/// Remove passkey-bearing URL details and common credential fields from
/// untrusted tracker-provided warning and failure messages.
pub fn sanitize_tracker_message(message: &str) -> String {
    let mut normalized = String::with_capacity(message.len());
    for character in message.chars() {
        if matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
            // NUL is an internal line boundary marker. Input NUL characters
            // are handled by the control-character branch below and cannot
            // spoof this boundary.
            normalized.push(' ');
            normalized.push('\0');
            normalized.push(' ');
        } else if character.is_control() || is_formatting_control(character) {
            // A visible non-whitespace replacement keeps obfuscated
            // credential names contiguous for classification.
            normalized.push('\u{fffd}');
        } else {
            normalized.push(character);
        }
    }
    let mut redacted = Vec::new();
    let mut redact_next_value = false;
    let mut redact_header_rest_of_line = false;
    let mut suppress_fragments_until = None;
    let mut suppress_header_rest_after_fragments = false;
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if let Some(end) = suppress_fragments_until {
            if index <= end {
                if *token == "\0" {
                    if !suppress_header_rest_after_fragments {
                        redact_header_rest_of_line = false;
                    }
                } else {
                    redacted.push("[redacted]".to_owned());
                    if suppress_header_rest_after_fragments && index == end {
                        redact_header_rest_of_line = true;
                    }
                }
                continue;
            }
            suppress_fragments_until = None;
            suppress_header_rest_after_fragments = false;
        }
        if *token == "\0" {
            if !redact_header_rest_of_line {
                if let Some(end) = sensitive_header_line_join_end(&tokens, index) {
                    suppress_fragments_until = Some(end);
                    suppress_header_rest_after_fragments = true;
                } else {
                    suppress_fragments_until = sensitive_query_line_join_end(&tokens, index);
                }
            }
            redact_header_rest_of_line = false;
            continue;
        }
        if redact_header_rest_of_line {
            continue;
        }
        if is_sensitive_header_token(token) {
            redacted.push(redact_token(token));
            redact_next_value = false;
            redact_header_rest_of_line = true;
            continue;
        }
        if redact_next_value {
            redacted.push("[redacted]".to_owned());
            redact_next_value = false;
            continue;
        }
        redact_next_value = ends_with_sensitive_empty_value(token);
        redacted.push(redact_token(token));
    }
    redacted.join(" ")
}

fn is_formatting_control(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

fn ends_with_sensitive_empty_value(token: &str) -> bool {
    let segment = token.rsplit(['&', ';']).next().unwrap_or(token);
    let Some((key, value)) = segment.split_once('=') else {
        return false;
    };
    if !value.is_empty() {
        return false;
    }
    let key = key
        .rsplit('?')
        .next()
        .unwrap_or(key)
        .rsplit('#')
        .next()
        .unwrap_or(key);
    is_sensitive_query_key(key)
}

fn sensitive_query_line_join_end(tokens: &[&str], boundary_index: usize) -> Option<usize> {
    let left = *tokens.get(boundary_index.checked_sub(1)?)?;
    if left == "\0" {
        return None;
    }
    if left.len() > MAX_LINE_SPLIT_KEY_BYTES {
        return Some(tokens.len().saturating_sub(1));
    }
    if redact_query_parameters(left) != left {
        return None;
    }

    let mut right = String::new();
    let mut scanned_bytes = left.len();
    let mut index = boundary_index + 1;
    while index < tokens.len() {
        if tokens[index] == "\0" {
            index += 1;
            continue;
        }

        right.push_str(tokens[index]);
        scanned_bytes = scanned_bytes.saturating_add(tokens[index].len());
        let joined = format!("{left}{right}");
        if redact_query_parameters(&right) == right && redact_query_parameters(&joined) != joined {
            return Some(index);
        }
        if tokens[index].contains('=') {
            return None;
        }
        if scanned_bytes > MAX_LINE_SPLIT_KEY_BYTES {
            return Some(tokens.len().saturating_sub(1));
        }

        index += 1;
        if index >= tokens.len() || tokens[index] != "\0" {
            break;
        }
        index += 1;
    }
    None
}

fn sensitive_header_line_join_end(tokens: &[&str], boundary_index: usize) -> Option<usize> {
    let left = *tokens.get(boundary_index.checked_sub(1)?)?;
    if left == "\0" {
        return None;
    }
    if left.len() > MAX_LINE_SPLIT_KEY_BYTES {
        return Some(tokens.len().saturating_sub(1));
    }
    if is_sensitive_header_token(left) {
        return None;
    }

    let mut right = String::new();
    let mut scanned_bytes = left.len();
    let mut index = boundary_index + 1;
    while index < tokens.len() {
        if tokens[index] == "\0" {
            index += 1;
            continue;
        }

        right.push_str(tokens[index]);
        scanned_bytes = scanned_bytes.saturating_add(tokens[index].len());
        let joined = format!("{left}{right}");
        if !is_sensitive_header_token(&right) && is_sensitive_header_token(&joined) {
            return Some(index);
        }
        if tokens[index].contains(':') {
            return None;
        }
        if scanned_bytes > MAX_LINE_SPLIT_KEY_BYTES {
            return Some(tokens.len().saturating_sub(1));
        }

        index += 1;
        if index >= tokens.len() || tokens[index] != "\0" {
            break;
        }
        index += 1;
    }
    None
}

fn redact_token(token: &str) -> String {
    if token.to_ascii_lowercase().contains("magnet:") {
        return "[redacted-magnet]".to_owned();
    }
    if let Some(colon) = sensitive_header_colon(token) {
        return format!("{}[redacted]", &token[..=colon]);
    }
    if let Some(redacted_url) = redact_url_token(token) {
        return redacted_url;
    }
    let redacted_query = redact_query_parameters(token);
    if redacted_query != token {
        if looks_like_path(token) {
            return redact_path_query_token(&redacted_query);
        }
        let fragment_start = redacted_query.find('#').unwrap_or(redacted_query.len());
        return redacted_query[..fragment_start].to_owned();
    }
    if looks_like_path(token) {
        return redact_path_token(token);
    }
    token.to_owned()
}

fn looks_like_path(token: &str) -> bool {
    let trimmed =
        token.trim_matches(|character| matches!(character, '"' | '\'' | ',' | ';' | ')' | '('));
    let bytes = trimmed.as_bytes();
    let windows_drive_path = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    let windows_root_path =
        trimmed.starts_with('\\') && !trimmed.starts_with(r"\u{") && !trimmed.starts_with(r"\x{");
    trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with(".\\")
        || trimmed.starts_with("..\\")
        || trimmed.starts_with("~\\")
        || windows_root_path
        || windows_drive_path
}

fn redact_path_token(token: &str) -> String {
    let trimmed =
        token.trim_matches(|character| matches!(character, '"' | '\'' | ',' | ';' | ')' | '('));
    let suffix = trimmed
        .trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .filter(|component| !component.is_empty() && !component.ends_with(':'))
        .unwrap_or("path");
    token.replace(trimmed, &format!("[redacted-path:{suffix}]"))
}

fn redact_path_query_token(redacted: &str) -> String {
    let Some(query_start) = redacted.find('?') else {
        return "[redacted-path]".to_owned();
    };
    let query_end = redacted[query_start..]
        .find('#')
        .map_or(redacted.len(), |offset| query_start + offset);
    format!("[redacted-path]{}", &redacted[query_start..query_end])
}

fn is_sensitive_header_token(token: &str) -> bool {
    sensitive_header_colon(token).is_some()
}

fn sensitive_header_colon(token: &str) -> Option<usize> {
    let colon = token.find(':')?;
    let name = token[..colon].trim_matches(|character| matches!(character, '"' | '\''));
    is_sensitive_query_key(name).then_some(colon)
}

fn redact_url_token(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    let separator = lower.find("://")?;
    let start = lower[..separator]
        .rfind(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '+' | '.' | '-')
        })
        .map_or(0, |index| index + 1);
    let scheme = &lower[start..separator];
    let prefix = &token[..start];
    if !scheme
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-'))
    {
        return Some(format!("{prefix}[redacted-url]"));
    }

    let Ok(mut url) = Url::parse(&token[start..]) else {
        return Some(format!("{prefix}[redacted-url]"));
    };
    if url.host_str().is_none() {
        return Some(format!("{prefix}[redacted-url]"));
    }

    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Some(format!("{prefix}{url}"))
}

fn redact_query_parameters(token: &str) -> String {
    let mut output = String::with_capacity(token.len());
    for part in token.split_inclusive(['&', ';']) {
        let (segment, separator) = match part.chars().last() {
            Some('&' | ';') => (&part[..part.len() - 1], &part[part.len() - 1..]),
            _ => (part, ""),
        };
        if let Some((key_prefix, _value)) = segment.split_once('=') {
            let key = key_prefix
                .rsplit('?')
                .next()
                .unwrap_or(key_prefix)
                .rsplit('#')
                .next()
                .unwrap_or(key_prefix);
            if is_sensitive_query_key(key) {
                output.push_str(key_prefix);
                output.push_str("=[redacted]");
            } else {
                output.push_str(segment);
            }
        } else {
            output.push_str(segment);
        }
        output.push_str(separator);
    }
    output
}

fn is_sensitive_query_key(key: &str) -> bool {
    let decoded = Url::parse(&format!("https://redaction.invalid/?{key}=x"))
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .next()
                .map(|(query_key, _)| query_key.into_owned())
        })
        .unwrap_or_else(|| key.to_owned());
    let normalized = decoded
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
        .map(char::from)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "passkey"
            | "pass"
            | "pid"
            | "auth"
            | "authkey"
            | "authorization"
            | "token"
            | "key"
            | "secret"
            | "uk"
            | "rsskey"
            | "apikey"
            | "password"
            | "passwd"
            | "cookie"
            | "session"
            | "sessionid"
            | "torrentpass"
            | "trackerpass"
            | "privatekey"
            | "signature"
            | "sig"
            | "accesskey"
            | "accesskeyid"
            | "clientsecret"
    ) || normalized.ends_with("token")
        || normalized.ends_with("passkey")
        || normalized.ends_with("torrentpass")
        || normalized.ends_with("trackerpass")
        || normalized.ends_with("passphrase")
        || normalized.ends_with("apikey")
        || normalized.ends_with("authkey")
        || normalized.ends_with("authorization")
        || normalized.ends_with("cookie")
        || normalized.ends_with("auth")
        || normalized.ends_with("signature")
        || normalized.contains("password")
        || normalized.contains("passwd")
        || normalized.contains("secret")
        || normalized.contains("credential")
        || normalized.contains("bearer")
        || normalized == "pwd"
        || key.contains('%')
}

#[cfg(test)]
mod tests {
    use super::sanitize_tracker_message;

    #[test]
    fn removes_credentials_from_urls_and_magnets() {
        let message = "See https://user:auth-secret@tracker.example:8443/short-passkey/announce?signature=query-secret#fragment-secret and ftp://user:password@files.example/private?token=ftp-secret magnet:?xt=urn:btih:magnet-secret";
        let sanitized = sanitize_tracker_message(message);

        assert_eq!(
            sanitized,
            "See https://tracker.example:8443/ and ftp://files.example/ [redacted-magnet]"
        );
        for secret in [
            "user",
            "auth-secret",
            "short-passkey",
            "query-secret",
            "fragment-secret",
            "password",
            "ftp-secret",
            "magnet-secret",
        ] {
            assert!(!sanitized.contains(secret), "{sanitized}");
        }

        let control_obfuscated = sanitize_tracker_message(
            "pass\u{202e}phrase=bidi-secret api\u{200b}secret=zero-width-secret pass\tphrase=tab-secret pass\nphrase=line-break-secret pass\nph\nrase=multi-line-break-secret pass\u{e0001}phrase=tag-secret",
        );
        for secret in [
            "bidi-secret",
            "zero-width-secret",
            "tab-secret",
            "line-break-secret",
            "multi-line-break-secret",
            "tag-secret",
        ] {
            assert!(!control_obfuscated.contains(secret), "{control_obfuscated}");
        }

        let ambiguous_line_chain = format!(
            "{}pass\nphrase=oversized-chain-secret",
            "ordinary\n".repeat(40)
        );
        assert!(!sanitize_tracker_message(&ambiguous_line_chain).contains("oversized-chain-secret"));
    }

    #[test]
    fn redacts_standalone_and_percent_encoded_credential_fields() {
        assert_eq!(
            sanitize_tracker_message(
                "authkey=auth-secret signature=sig-secret pass%6bey=encoded-secret ordinary=visible"
            ),
            "authkey=[redacted] signature=[redacted] pass%6bey=[redacted] ordinary=visible"
        );

        let sanitized = sanitize_tracker_message(
            "api_secret=api-secret secret_key=key-secret pwd=pwd-secret passphrase=phrase-secret bearer=bearer-secret api_credential=credential-secret p.a.s.s.k.e.y=obfuscated-secret access.t.o.k.e.n=obfuscated-token",
        );
        for secret in [
            "api-secret",
            "key-secret",
            "pwd-secret",
            "phrase-secret",
            "bearer-secret",
            "credential-secret",
            "obfuscated-secret",
            "obfuscated-token",
        ] {
            assert!(!sanitized.contains(secret), "{sanitized}");
        }
    }

    #[test]
    fn redacts_header_credentials_and_sensitive_path_queries() {
        let sanitized = sanitize_tracker_message(
            "tracker response\nAuthorization: Bearer header-secret trailing-secret\nAuthori\nzation: Bearer split-header-secret\nnext line after split header\nX-API-Key: api-key-secret\nCookie: sid=cookie-secret; theme=second-secret\n/srv/private/announce/tracker-passkey?pid=peer-secret#fragment-secret\n/srv/private/tracker-passkey#pid=fragment-peer-secret\nannounce?pid=standalone-peer-secret&x=visible#standalone-fragment-secret\nnext line remains",
        );

        for secret in [
            "header-secret",
            "split-header-secret",
            "trailing-secret",
            "api-key-secret",
            "cookie-secret",
            "second-secret",
            "/srv/private",
            "tracker-passkey",
            "peer-secret",
            "fragment-secret",
            "fragment-peer-secret",
            "standalone-peer-secret",
            "standalone-fragment-secret",
        ] {
            assert!(!sanitized.contains(secret), "{sanitized}");
        }
        assert!(sanitized.contains("Authorization:[redacted]"));
        assert!(sanitized.contains("X-API-Key:[redacted]"));
        assert!(sanitized.contains("Cookie:[redacted]"));
        assert!(sanitized.contains("[redacted-path]?pid=[redacted]"));
        assert!(sanitized.contains("announce?pid=[redacted]&x=visible"));
        assert!(sanitized.contains("next line after split header"));
        assert!(sanitized.contains("next line remains"));
        assert!(!sanitized.chars().any(char::is_control));
    }

    #[test]
    fn redacts_credential_values_split_across_line_boundaries() {
        assert_eq!(
            sanitize_tracker_message("tracker warning\npasskey=split-secret"),
            "tracker warning passkey=[redacted]"
        );
        assert_eq!(
            sanitize_tracker_message(
                "tracker warning https://tracker.example/announce?token=\nurl-secret"
            ),
            "tracker warning https://tracker.example/ [redacted]"
        );
    }

    #[test]
    fn removes_terminal_control_characters_from_remote_messages() {
        let sanitized = sanitize_tracker_message(
            "warning:\u{1b}[31mred\u{1b}[0m\u{7f}\u{202e}spoof\u{2066}text",
        );

        assert_eq!(
            sanitized,
            "warning:\u{fffd}[31mred\u{fffd}[0m\u{fffd}\u{fffd}spoof\u{fffd}text"
        );
        assert!(!sanitized.chars().any(char::is_control));
        assert!(!sanitized.chars().any(super::is_formatting_control));
    }
}
