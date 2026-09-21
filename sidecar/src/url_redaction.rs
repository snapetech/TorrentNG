use std::path::Path;

const MAX_LINE_SPLIT_KEY_BYTES: usize = 256;

/// Keep useful scheme/host/port context without exposing URL credentials,
/// passkeys embedded in paths, query parameters, or fragments.
pub(crate) fn redact_log_url(value: &str) -> String {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("magnet:") {
        return "[redacted-magnet]".to_owned();
    }
    if trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.starts_with("./")
        || trimmed.starts_with("../")
    {
        return "[redacted-path]".to_owned();
    }

    let Ok(mut url) = reqwest::Url::parse(trimmed) else {
        return "[redacted-url]".to_owned();
    };
    if url.host_str().is_none() {
        return "[redacted-url]".to_owned();
    }

    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

/// Redact credentials and control characters from untrusted backend status
/// text before it is persisted or returned through a compatibility API.
pub(crate) fn redact_sensitive_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
            normalized.push(' ');
            normalized.extend(character.escape_debug());
            normalized.push(' ');
        } else {
            normalized.push(character);
        }
    }
    let sanitized = crate::log_sanitization::sanitize_operator_log_text(&normalized);
    let mut redacted = Vec::new();
    let mut redact_next_value = false;
    let mut redact_header_rest_of_line = false;
    let mut suppress_fragments_until = None;
    let mut suppress_header_rest_after_fragments = false;
    let tokens = sanitized.split_whitespace().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if let Some(end) = suppress_fragments_until {
            if index <= end {
                if is_line_break_escape(token) {
                    redacted.push((*token).to_owned());
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
        if is_line_break_escape(token) {
            if !redact_header_rest_of_line {
                if let Some(end) = sensitive_header_line_join_end(&tokens, index) {
                    suppress_fragments_until = Some(end);
                    suppress_header_rest_after_fragments = true;
                } else {
                    // A credential name can be split over more than one line
                    // break. Suppress its remaining fragments through the value.
                    suppress_fragments_until = sensitive_query_line_join_end(&tokens, index);
                }
            }
            redacted.push((*token).to_owned());
            redact_header_rest_of_line = false;
            continue;
        }
        if redact_header_rest_of_line {
            continue;
        }
        if is_sensitive_header_token(token) {
            redacted.push(redact_sensitive_token(token));
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
        redacted.push(redact_sensitive_token(token));
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

/// Render a dynamic error for operator logs without preserving recognizable
/// credentials, paths, or record/terminal control characters.
pub fn redact_display(value: &impl std::fmt::Display) -> String {
    redact_sensitive_text(&value.to_string())
}

/// Preserve operator-event JSON shape while sanitizing every string value
/// and replacing values stored under recognized credential keys. Invalid
/// legacy JSON fails closed to an empty object so log APIs stay readable.
pub(crate) fn redact_sensitive_json(value: &str) -> String {
    let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(value) else {
        return "{}".to_owned();
    };
    redact_json_value(&mut parsed);
    serde_json::to_string(&parsed).unwrap_or_else(|_| "{}".to_owned())
}

fn redact_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = redact_sensitive_text(text),
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json_value(value);
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let normalized_key = normalize_sensitive_query_key(key);
                if is_sensitive_normalized_key(&normalized_key)
                    && !(normalized_key == "credentials" && value.is_object())
                {
                    *value = serde_json::Value::String("[redacted]".to_owned());
                } else {
                    redact_json_value(value);
                }
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn is_line_break_escape(token: &str) -> bool {
    matches!(token, "\\r" | "\\n" | "\\u{2028}" | "\\u{2029}")
}

fn sensitive_query_line_join_end(tokens: &[&str], boundary_index: usize) -> Option<usize> {
    let left = *tokens.get(boundary_index.checked_sub(1)?)?;
    if is_line_break_escape(left) {
        return None;
    }
    if left.len() > MAX_LINE_SPLIT_KEY_BYTES {
        return Some(tokens.len().saturating_sub(1));
    }
    if redact_query_token(left) != left {
        return None;
    }

    let mut right = String::new();
    let mut scanned_bytes = left.len();
    let mut index = boundary_index + 1;
    while index < tokens.len() {
        if is_line_break_escape(tokens[index]) {
            index += 1;
            continue;
        }

        right.push_str(tokens[index]);
        scanned_bytes = scanned_bytes.saturating_add(tokens[index].len());
        let joined = format!("{left}{right}");
        if redact_query_token(&right) == right && redact_query_token(&joined) != joined {
            return Some(index);
        }
        if tokens[index].contains('=') {
            return None;
        }
        if scanned_bytes > MAX_LINE_SPLIT_KEY_BYTES {
            return Some(tokens.len().saturating_sub(1));
        }

        index += 1;
        if index >= tokens.len() || !is_line_break_escape(tokens[index]) {
            break;
        }
        index += 1;
    }
    None
}

fn sensitive_header_line_join_end(tokens: &[&str], boundary_index: usize) -> Option<usize> {
    let left = *tokens.get(boundary_index.checked_sub(1)?)?;
    if is_line_break_escape(left) {
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
        if is_line_break_escape(tokens[index]) {
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
        if index >= tokens.len() || !is_line_break_escape(tokens[index]) {
            break;
        }
        index += 1;
    }
    None
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

pub(crate) fn redact_sensitive_token(token: &str) -> String {
    if token.to_ascii_lowercase().contains("magnet:") {
        return "[redacted-magnet]".to_owned();
    }
    if let Some(colon) = sensitive_header_colon(token) {
        return format!("{}[redacted]", &token[..=colon]);
    }
    if let Some(redacted_url) = redact_url_token(token) {
        return redacted_url;
    }
    if token.contains('=') {
        let redacted = redact_query_token(token);
        if redacted != token {
            if looks_like_path(token) {
                return redact_path_query_token(&redacted);
            }
            let fragment_start = redacted.find('#').unwrap_or(redacted.len());
            return redacted[..fragment_start].to_owned();
        }
    }
    if looks_like_path(token) {
        return redact_path_token(token);
    }
    token.to_owned()
}

fn looks_like_path(token: &str) -> bool {
    let trimmed = token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')' | '('));
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
        || windows_root_path
        || trimmed.starts_with(".\\")
        || trimmed.starts_with("..\\")
        || trimmed.starts_with("~\\")
        || windows_drive_path
}

fn redact_path_token(token: &str) -> String {
    let trimmed = token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')' | '('));
    let suffix = if trimmed.contains('\\') {
        trimmed
            .trim_end_matches(['\\', '/'])
            .rsplit(['\\', '/'])
            .next()
            .filter(|component| !component.is_empty() && !component.ends_with(':'))
    } else {
        Path::new(trimmed).file_name().and_then(|s| s.to_str())
    }
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

fn redact_url_token(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    let separator = lower.find("://")?;
    let start = lower[..separator]
        .rfind(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '+' | '.' | '-')
        })
        .map_or(0, |index| index + 1);
    let scheme = &lower[start..separator];
    if !scheme
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-'))
    {
        return Some("[redacted-url]".to_owned());
    }
    let (prefix, url) = token.split_at(start);
    Some(format!("{prefix}{}", redact_log_url(url)))
}

fn redact_query_token(token: &str) -> String {
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
    is_sensitive_normalized_key(&normalize_sensitive_query_key(key))
}

fn is_sensitive_header_token(token: &str) -> bool {
    sensitive_header_colon(token).is_some()
}

fn sensitive_header_colon(token: &str) -> Option<usize> {
    let colon = token.find(':')?;
    let name = token[..colon].trim_matches(|character| matches!(character, '"' | '\''));
    is_sensitive_query_key(name).then_some(colon)
}

fn normalize_sensitive_query_key(key: &str) -> String {
    const MAX_DECODE_LAYERS: usize = 8;

    let mut decoded = key.as_bytes().to_vec();
    for _ in 0..MAX_DECODE_LAYERS {
        let mut next = Vec::with_capacity(decoded.len());
        let mut index = 0;
        while index < decoded.len() {
            let byte = decoded[index];
            if byte == b'%' && index + 2 < decoded.len() {
                if let (Some(high), Some(low)) =
                    (hex_value(decoded[index + 1]), hex_value(decoded[index + 2]))
                {
                    next.push((high << 4) | low);
                    index += 3;
                    continue;
                }
            }
            next.push(byte);
            index += 1;
        }
        if next == decoded {
            break;
        }
        decoded = next;
    }

    let mut normalized = String::with_capacity(decoded.len());
    let mut index = 0;
    while index < decoded.len() {
        if let Some(length) = escaped_control_sequence_len(&decoded, index) {
            index += length;
            continue;
        }
        let byte = decoded[index];
        if byte.is_ascii_alphanumeric() {
            normalized.push(char::from(byte.to_ascii_lowercase()));
        }
        index += 1;
    }
    normalized
}

fn escaped_control_sequence_len(bytes: &[u8], index: usize) -> Option<usize> {
    if bytes.get(index) != Some(&b'\\') {
        return None;
    }
    match *bytes.get(index + 1)? {
        b'0' | b'n' | b'r' | b't' => Some(2),
        b'u' if bytes.get(index + 2) == Some(&b'{') => {
            let closing = bytes[index + 3..].iter().position(|byte| *byte == b'}')? + index + 3;
            let codepoint =
                u32::from_str_radix(std::str::from_utf8(&bytes[index + 3..closing]).ok()?, 16)
                    .ok()?;
            let character = char::from_u32(codepoint)?;
            (character.is_control() || is_formatting_control(character))
                .then_some(closing + 1 - index)
        }
        b'x' if index + 3 < bytes.len() => {
            let digits = std::str::from_utf8(&bytes[index + 2..index + 4]).ok()?;
            let byte = u8::from_str_radix(digits, 16).ok()?;
            (byte.is_ascii_control()).then_some(4)
        }
        _ => None,
    }
}

fn is_sensitive_normalized_key(normalized: &str) -> bool {
    matches!(
        normalized,
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
            | "torrentpass"
            | "trackerpass"
            | "privatekey"
            | "signature"
            | "sig"
            | "accesskey"
            | "accesskeyid"
            | "clientsecret"
    ) || normalized.ends_with("token")
        || normalized.ends_with("apikey")
        || normalized.ends_with("authorization")
        || normalized.ends_with("cookie")
        || normalized.ends_with("auth")
        || normalized.ends_with("passkey")
        || normalized.ends_with("torrentpass")
        || normalized.ends_with("trackerpass")
        || normalized.ends_with("passphrase")
        || normalized.contains("password")
        || normalized.contains("passwd")
        || normalized.contains("secret")
        || normalized.contains("credential")
        || normalized.contains("bearer")
        || normalized == "pwd"
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::redact_log_url;

    #[test]
    fn tracker_url_redaction_removes_userinfo_path_query_and_fragment() {
        let raw = "https://user:password@tracker.example:8443/announce/passkey-path?passkey=query-secret#fragment-secret";
        let redacted = redact_log_url(raw);

        assert_eq!(redacted, "https://tracker.example:8443/");
        for secret in [
            "user",
            "password",
            "passkey-path",
            "query-secret",
            "fragment-secret",
        ] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
    }

    #[test]
    fn tracker_url_redaction_preserves_udp_origin_and_hides_magnets_and_paths() {
        assert_eq!(
            redact_log_url("udp://tracker.example:6969/key/announce"),
            "udp://tracker.example:6969/"
        );
        assert_eq!(
            redact_log_url("magnet:?xt=urn:btih:secret&tr=https%3A%2F%2Ftracker%2Fkey"),
            "[redacted-magnet]"
        );
        assert_eq!(
            redact_log_url("/data/private/file.torrent"),
            "[redacted-path]"
        );
        assert_eq!(redact_log_url("not a URL with secret"), "[redacted-url]");
    }

    #[test]
    fn sensitive_backend_text_redacts_credentials_and_sanitizes_controls() {
        let raw = "tracker rejected https://user:password@tracker.example/short-passkey?signature=query-secret %61uthkey=auth-secret \u{1b}[31m café\nordinary";
        let redacted = super::redact_sensitive_text(raw);

        assert!(redacted.contains("https://tracker.example/"));
        assert!(redacted.contains("%61uthkey=[redacted]"));
        assert!(redacted.contains("\\n"));
        assert!(redacted.contains("\\u{1b}"));
        assert!(redacted.contains("café"));
        for secret in [
            "user",
            "password",
            "short-passkey",
            "query-secret",
            "auth-secret",
        ] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert!(!redacted.chars().any(char::is_control));

        let split_secret =
            super::redact_sensitive_text("tracker warning\npasskey=secret-on-next-line");
        assert!(split_secret.contains("passkey=[redacted]"));
        assert!(!split_secret.contains("secret-on-next-line"));

        let split_url_secret = super::redact_sensitive_text(
            "tracker warning https://tracker.example/announce?token=\nsecret-on-next-line",
        );
        assert!(split_url_secret.contains("https://tracker.example/"));
        assert!(!split_url_secret.contains("secret-on-next-line"));
    }

    #[test]
    fn sensitive_backend_text_redacts_multi_encoded_keys_and_split_values() {
        let redacted = super::redact_sensitive_text(
            "tracker status %2570asskey=direct-secret %2574oken=\nsplit-secret",
        );

        assert!(redacted.contains("%2570asskey=[redacted]"));
        assert!(redacted.contains("%2574oken=[redacted]"));
        assert!(!redacted.contains("direct-secret"));
        assert!(!redacted.contains("split-secret"));

        let raw_json = serde_json::json!({"%2570asskey": "nested-secret"}).to_string();
        let redacted_json: serde_json::Value =
            serde_json::from_str(&super::redact_sensitive_json(&raw_json)).unwrap();
        assert_eq!(redacted_json["%2570asskey"], "[redacted]");
    }

    #[test]
    fn sensitive_backend_text_redacts_common_credential_key_aliases() {
        let redacted = super::redact_sensitive_text(
            "api_secret=api-secret secret_key=key-secret pwd=short-secret \
             tracker_pass=tracker-secret passphrase=phrase-secret bearer=bearer-secret \
             api_credential=credential-secret pid=peer-secret uk=user-secret sig=signature-secret",
        );

        for secret in [
            "api-secret",
            "key-secret",
            "short-secret",
            "tracker-secret",
            "phrase-secret",
            "bearer-secret",
            "credential-secret",
            "peer-secret",
            "user-secret",
            "signature-secret",
        ] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert!(redacted.contains("api_secret=[redacted]"));
        assert!(redacted.contains("secret_key=[redacted]"));
        assert!(redacted.contains("pwd=[redacted]"));
        assert!(redacted.contains("tracker_pass=[redacted]"));
        assert!(redacted.contains("passphrase=[redacted]"));
        assert!(redacted.contains("bearer=[redacted]"));
        assert!(redacted.contains("api_credential=[redacted]"));
        assert!(redacted.contains("pid=[redacted]"));
        assert!(redacted.contains("uk=[redacted]"));
        assert!(redacted.contains("sig=[redacted]"));

        let split_header = super::redact_sensitive_text(
            "Authori\nzation: Bearer split-header-secret\nnext line after split header",
        );
        assert!(!split_header.contains("split-header-secret"));
        assert!(split_header.contains("next line after split header"));

        let control_obfuscated = super::redact_sensitive_text(
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
        assert!(
            !super::redact_sensitive_text(&ambiguous_line_chain).contains("oversized-chain-secret")
        );

        let raw_json = serde_json::json!({
            "api_secret": "api-secret",
            "secret_key": "key-secret",
            "pwd": "short-secret",
            "tracker_pass": "tracker-secret",
            "passphrase": "phrase-secret",
            "bearer": "bearer-secret",
            "api_credential": "credential-secret",
            "credentials": "scalar-credential-secret",
            "pid": "peer-secret",
            "uk": "user-secret",
            "sig": "signature-secret",
        })
        .to_string();
        let redacted_json: serde_json::Value =
            serde_json::from_str(&super::redact_sensitive_json(&raw_json)).unwrap();
        for key in [
            "api_secret",
            "secret_key",
            "pwd",
            "tracker_pass",
            "passphrase",
            "bearer",
            "api_credential",
            "credentials",
            "pid",
            "uk",
            "sig",
        ] {
            assert_eq!(redacted_json[key], "[redacted]", "{key}");
        }
    }

    #[test]
    fn sensitive_backend_text_redacts_header_style_credentials_until_line_end() {
        let redacted = super::redact_sensitive_text(
            "request failed\nAuthorization: Bearer bearer-secret trailing-secret\nX-API-Key: api-key-secret\nProxy-Authorization: Basic basic-secret\nCookie: sid=cookie-secret; theme=second-cookie-secret\nSet-Cookie: session=set-cookie-secret; HttpOnly\nresponse details remain",
        );

        for secret in [
            "bearer-secret",
            "trailing-secret",
            "api-key-secret",
            "basic-secret",
            "cookie-secret",
            "second-cookie-secret",
            "set-cookie-secret",
        ] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        for header in [
            "Authorization:[redacted]",
            "X-API-Key:[redacted]",
            "Proxy-Authorization:[redacted]",
            "Cookie:[redacted]",
            "Set-Cookie:[redacted]",
        ] {
            assert!(redacted.contains(header), "{redacted}");
        }
        assert!(redacted.contains("response details remain"));
    }

    #[test]
    fn sensitive_query_redaction_preserves_path_privacy_and_drops_fragments() {
        for raw in [
            "/srv/private/announce/tracker-passkey?pid=peer-secret#fragment-secret",
            "/srv/private/tracker-passkey#pid=fragment-peer-secret",
        ] {
            let redacted = super::redact_sensitive_text(raw);
            assert!(redacted.starts_with("[redacted-path]"), "{redacted}");
            for secret in [
                "/srv/private",
                "announce",
                "tracker-passkey",
                "peer-secret",
                "fragment-secret",
                "fragment-peer-secret",
            ] {
                assert!(!redacted.contains(secret), "{redacted}");
            }
        }

        let redacted =
            super::redact_sensitive_text("announce?pid=peer-secret&x=visible#fragment-secret");
        assert!(redacted.contains("announce?pid=[redacted]&x=visible"));
        assert!(!redacted.contains("peer-secret"));
        assert!(!redacted.contains("fragment-secret"));
    }

    #[test]
    fn sensitive_backend_text_redacts_windows_drive_and_unc_paths() {
        let redacted = super::redact_sensitive_text(
            r"read C:\Users\alice\Downloads\private.torrent \\nas01\media\secret.mkv",
        );

        assert!(redacted.contains("[redacted-path:private.torrent]"));
        assert!(redacted.contains("[redacted-path:secret.mkv]"));
        for path_part in ["C:\\Users", "alice", "\\\\as01\\media"] {
            assert!(!redacted.contains(path_part), "{redacted}");
        }
    }

    #[test]
    fn display_values_are_safe_for_structured_log_fields() {
        let raw = "category\nvalue \u{001b}[31m \u{202e}spoof https://user:password@tracker.example/private/key?token=query-secret /home/alice/torrent.data";
        let redacted = super::redact_display(&raw);

        assert!(redacted.contains("category"));
        assert!(redacted.contains("\\n"));
        assert!(redacted.contains("\\u{1b}"));
        assert!(redacted.contains("https://tracker.example/"));
        assert!(redacted.contains("[redacted-path:torrent.data]"));
        for secret in ["user", "password", "token=", "query-secret", "/home/alice"] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert!(!redacted.chars().any(char::is_control));
    }

    #[test]
    fn sensitive_json_redacts_nested_values_and_preserves_shape() {
        let raw = serde_json::json!({
            "error": "request to https://user:password@tracker.example/private?token=url-secret failed",
            "credentials": {
                "access_token": "access-secret",
                "safe_label": "café"
            },
            "path": "/srv/private/torrent.data",
            "items": ["signature=array-secret", 3],
            "control": "line\nfeed \u{001b} \u{202e}"
        })
        .to_string();

        let redacted: serde_json::Value =
            serde_json::from_str(&super::redact_sensitive_json(&raw)).unwrap();
        assert_eq!(redacted["credentials"]["access_token"], "[redacted]");
        assert_eq!(redacted["credentials"]["safe_label"], "café");
        assert_eq!(redacted["items"][1], 3);
        assert!(redacted["control"].as_str().unwrap().contains("\\n"));
        let rendered = redacted.to_string();
        for secret in [
            "user",
            "password",
            "url-secret",
            "access-secret",
            "/srv/private",
            "array-secret",
        ] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
        assert!(!rendered.chars().any(char::is_control));
        assert_eq!(super::redact_sensitive_json("invalid JSON"), "{}");
    }
}
