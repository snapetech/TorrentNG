use http::{header, HeaderMap};
use subtle::ConstantTimeEq;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderValueError {
    Duplicate,
    InvalidUtf8,
}

/// Compare a presented API token with every configured token without
/// short-circuiting on the first differing byte.
pub fn api_token_allowed(api_tokens: &[String], candidate: &str) -> bool {
    let mut matched = 0u8;
    for allowed in api_tokens {
        matched |= u8::from(bool::from(allowed.as_bytes().ct_eq(candidate.as_bytes())));
    }
    matched != 0
}

/// Parse an HTTP Bearer credential without making the authentication scheme's
/// casing significant. RFC 7235 defines the scheme as case-insensitive; keep
/// exactly one non-empty token and reject extra whitespace-separated fields.
pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let mut parts = single_header_value(headers, "authorization")
        .ok()??
        .split_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if parts.next().is_some() || !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    Some(token.to_owned())
}

/// Return whether a request carries one of the bearer-backed browser session
/// cookies.  The caller should invoke this only after it has validated the
/// cookie's value against the configured token set.
pub fn has_session_cookie(headers: &HeaderMap, names: &[&str]) -> bool {
    session_cookie_value(headers, names).is_some()
}

/// Return whether a request carries metadata that identifies it as a browser
/// request.  Keep this separate from [`csrf_request_allowed`]: clients that
/// do not send browser metadata remain compatible, while browser requests
/// must provide positive same-origin evidence before a public auth endpoint
/// sets or clears a session cookie.
pub fn has_browser_request_headers(headers: &HeaderMap) -> bool {
    headers.contains_key(header::ORIGIN)
        || headers.contains_key(header::REFERER)
        || headers.contains_key("sec-fetch-site")
}

/// Decode one of the percent-encoded session-cookie values used by the
/// compatibility facades.
pub fn session_cookie_value(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    let cookie = single_header_value(headers, "cookie").ok()??;
    let mut candidate = None;
    for part in cookie.split(';') {
        let part = part.trim();
        let Some((name, value)) = part.split_once('=') else {
            // A malformed candidate cookie is not proof of authentication,
            // but a malformed ordinary cookie should not invalidate an
            // otherwise valid session either.
            continue;
        };
        if !names.contains(&name) {
            continue;
        }
        if value.is_empty() {
            return None;
        }
        let decoded = percent_decode(value)?;
        if candidate.replace(decoded).is_some() {
            // Reject duplicate aliases (including SID + tng_session) rather
            // than letting different intermediaries choose different values.
            return None;
        }
    }
    candidate
}

/// Reject browser cookie mutations that do not carry positive same-origin
/// evidence when Fetch Metadata is present.
///
/// API clients using an Authorization header do not need this check. Missing
/// browser metadata remains allowed for non-browser clients, while an absent
/// Host or an explicit Origin/Referer or Fetch-Metadata claim is fail-closed.
/// Comparing the origin authority with Host keeps this independent of the
/// deployment's scheme and works behind TLS-terminating proxies without
/// trusting a proxy header supplied by the caller.
pub fn csrf_request_allowed(headers: &HeaderMap) -> bool {
    if let Some(value) = headers.get("sec-fetch-site") {
        // `same-site` is still cross-origin, and `none`/unknown values do not
        // prove that a cookie-backed mutation originated from this service.
        // Treat invalid header bytes the same way instead of silently
        // downgrading to the non-browser compatibility path.
        if !value
            .to_str()
            .is_ok_and(|value| value.eq_ignore_ascii_case("same-origin"))
        {
            return false;
        }
    }

    let Ok(Some(host)) = single_header_value(headers, "host") else {
        return false;
    };

    for (name, require_origin_scheme) in [("origin", true), ("referer", false)] {
        let Ok(value) = single_header_value(headers, name) else {
            return false;
        };
        let Some(value) = value else {
            continue;
        };
        if !same_origin_authority(value, host, require_origin_scheme) {
            return false;
        }
    }
    true
}

/// Return one valid UTF-8 header value, rejecting duplicates. Security
/// decisions must not inspect only the first value when an intermediary may
/// combine or interpret repeated headers differently.
pub fn single_header_value<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<Option<&'a str>, HeaderValueError> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(HeaderValueError::Duplicate);
    }
    value
        .to_str()
        .map(Some)
        .map_err(|_| HeaderValueError::InvalidUtf8)
}

fn same_origin_authority(value: &str, host: &str, require_scheme: bool) -> bool {
    let value = value.trim();
    if value.eq_ignore_ascii_case("null") {
        return false;
    }
    let Some(scheme_end) = value.find("://") else {
        return false;
    };
    if require_scheme && scheme_end == 0 {
        return false;
    }
    let authority_start = scheme_end + 3;
    let authority = value[authority_start..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let scheme = value[..scheme_end].to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return false;
    }
    normalize_authority(authority, &scheme) == normalize_authority(host.trim(), &scheme)
}

fn normalize_authority(authority: &str, scheme: &str) -> String {
    let default_port = match scheme {
        "http" => Some(":80"),
        "https" => Some(":443"),
        _ => None,
    };
    let authority = default_port
        .filter(|port| authority.ends_with(port))
        .map_or(authority, |port| &authority[..authority.len() - port.len()]);
    authority.to_ascii_lowercase()
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return None;
        }
        let high = hex_value(bytes[index + 1])?;
        let low = hex_value(bytes[index + 2])?;
        output.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(output).ok()
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
    use super::*;
    use http::HeaderValue;

    #[test]
    fn api_token_comparison_requires_an_exact_match() {
        let tokens = vec!["short-secret".to_owned(), "another-secret".to_owned()];

        assert!(api_token_allowed(&tokens, "short-secret"));
        assert!(api_token_allowed(&tokens, "another-secret"));
        assert!(!api_token_allowed(&tokens, "short-secret-extra"));
        assert!(!api_token_allowed(&tokens, "short-secre"));
        assert!(!api_token_allowed(&tokens, ""));
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_but_credential_shape_is_exact() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "bearer opaque-token".parse().unwrap(),
        );
        assert_eq!(bearer_token(&headers).as_deref(), Some("opaque-token"));

        headers.insert(
            header::AUTHORIZATION,
            "BEARER opaque-token extra".parse().unwrap(),
        );
        assert!(bearer_token(&headers).is_none());

        headers.insert(header::AUTHORIZATION, "Basic opaque-token".parse().unwrap());
        assert!(bearer_token(&headers).is_none());
    }

    #[test]
    fn session_cookie_detection_is_name_and_value_aware() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "other=1; SID=token".parse().unwrap());
        assert!(has_session_cookie(&headers, &["SID"]));
        assert!(!has_session_cookie(&headers, &["tng_session"]));
        headers.insert(header::COOKIE, "SID=".parse().unwrap());
        assert!(!has_session_cookie(&headers, &["SID"]));
    }

    #[test]
    fn session_cookie_detection_rejects_duplicate_cookie_headers() {
        let mut headers = HeaderMap::new();
        headers.append(header::COOKIE, "SID=first".parse().unwrap());
        headers.append(header::COOKIE, "SID=second".parse().unwrap());
        assert!(!has_session_cookie(&headers, &["SID"]));
        assert!(session_cookie_value(&headers, &["SID"]).is_none());
    }

    #[test]
    fn csrf_rejects_cross_site_and_mismatched_origins() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "torrentng.example".parse().unwrap());
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!csrf_request_allowed(&headers));

        headers.remove("sec-fetch-site");
        headers.insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
        assert!(!csrf_request_allowed(&headers));
    }

    #[test]
    fn csrf_rejects_same_site_or_invalid_fetch_metadata() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "torrentng.example".parse().unwrap());
        headers.insert("sec-fetch-site", "same-site".parse().unwrap());
        headers.insert(header::ORIGIN, "https://torrentng.example".parse().unwrap());
        assert!(!csrf_request_allowed(&headers));

        headers.insert("sec-fetch-site", "none".parse().unwrap());
        assert!(!csrf_request_allowed(&headers));

        headers.insert(
            "sec-fetch-site",
            HeaderValue::from_bytes(b"same-origin\x80").unwrap(),
        );
        assert!(!csrf_request_allowed(&headers));
    }

    #[test]
    fn csrf_accepts_same_host_origin_and_non_browser_requests() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "torrentng.example:443".parse().unwrap());
        headers.insert(header::ORIGIN, "https://TORRENTNG.EXAMPLE".parse().unwrap());
        assert!(csrf_request_allowed(&headers));
        headers.remove(header::ORIGIN);
        assert!(csrf_request_allowed(&headers));
    }

    #[test]
    fn csrf_rejects_cookie_mutations_without_host_context() {
        let headers = HeaderMap::new();
        assert!(!csrf_request_allowed(&headers));
    }

    #[test]
    fn csrf_rejects_non_http_origin_even_when_authority_matches() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "torrentng.example".parse().unwrap());
        headers.insert(header::ORIGIN, "ftp://torrentng.example".parse().unwrap());
        assert!(!csrf_request_allowed(&headers));
    }

    #[test]
    fn security_headers_reject_duplicates() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "torrentng.example".parse().unwrap());
        headers.append(header::ORIGIN, "https://torrentng.example".parse().unwrap());
        headers.append(header::ORIGIN, "https://attacker.example".parse().unwrap());
        assert!(!csrf_request_allowed(&headers));

        headers.remove(header::ORIGIN);
        headers.append(header::AUTHORIZATION, "Bearer one".parse().unwrap());
        headers.append(header::AUTHORIZATION, "Bearer two".parse().unwrap());
        assert!(bearer_token(&headers).is_none());
    }

    #[test]
    fn single_header_value_rejects_duplicate_idempotency_keys() {
        let mut headers = HeaderMap::new();
        headers.append("idempotency-key", "request-a".parse().unwrap());
        headers.append("idempotency-key", "request-b".parse().unwrap());
        assert_eq!(
            single_header_value(&headers, "idempotency-key"),
            Err(HeaderValueError::Duplicate)
        );
    }

    #[test]
    fn session_cookie_value_rejects_duplicate_candidate_cookies() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "SID=first; SID=second".parse().unwrap());
        assert!(session_cookie_value(&headers, &["SID"]).is_none());

        headers.insert(
            header::COOKIE,
            "SID=first; tng_session=second".parse().unwrap(),
        );
        assert!(session_cookie_value(&headers, &["SID", "tng_session"]).is_none());

        headers.insert(header::COOKIE, "SID=; other=valid".parse().unwrap());
        assert!(session_cookie_value(&headers, &["SID"]).is_none());
    }
}
