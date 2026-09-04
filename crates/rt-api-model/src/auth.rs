use http::{header, HeaderMap};

/// Return whether a request carries one of the bearer-backed browser session
/// cookies.  The caller should invoke this only after it has validated the
/// cookie's value against the configured token set.
pub fn has_session_cookie(headers: &HeaderMap, names: &[&str]) -> bool {
    session_cookie_value(headers, names).is_some()
}

/// Decode one of the percent-encoded session-cookie values used by the
/// compatibility facades.
pub fn session_cookie_value(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())?;
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        if value.is_empty() || !names.contains(&name) {
            return None;
        }
        percent_decode(value)
    })
}

/// Reject browser cookie mutations that carry an explicit cross-site signal.
///
/// API clients using an Authorization header do not need this check.  Missing
/// browser metadata remains allowed for non-browser clients, while an
/// explicit Origin/Referer or Fetch-Metadata claim is fail-closed.  Comparing
/// the origin authority with Host keeps this independent of the deployment's
/// scheme and works behind TLS-terminating proxies without trusting a proxy
/// header supplied by the caller.
pub fn csrf_request_allowed(headers: &HeaderMap) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
    {
        return false;
    }

    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return headers.get(header::ORIGIN).is_none() && headers.get(header::REFERER).is_none();
    };

    for (name, require_origin_scheme) in [(header::ORIGIN, true), (header::REFERER, false)] {
        let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) else {
            continue;
        };
        if !same_origin_authority(value, host, require_origin_scheme) {
            return false;
        }
    }
    true
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
    fn csrf_accepts_same_host_origin_and_non_browser_requests() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "torrentng.example:443".parse().unwrap());
        headers.insert(header::ORIGIN, "https://TORRENTNG.EXAMPLE".parse().unwrap());
        assert!(csrf_request_allowed(&headers));
        headers.remove(header::ORIGIN);
        assert!(csrf_request_allowed(&headers));
    }
}
