use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha2::Sha256;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

use crate::api::server::AppState;

type SessionMac = Hmac<Sha256>;
const SESSION_TTL_SECS: u64 = 24 * 60 * 60;
const MAX_LOGIN_ATTEMPTS: u8 = 10;
const LOGIN_ATTEMPT_WINDOW: Duration = Duration::from_secs(60);
const MAX_TRACKED_LOGIN_CLIENTS: usize = 4_096;

/// Bounds unauthenticated login submissions by the TCP peer address. When
/// ConnectInfo is absent (for embedded/test routers), those requests share a
/// single bounded bucket instead of silently bypassing the control.
#[derive(Clone, Default)]
pub struct LoginAttemptLimiter {
    state: Arc<Mutex<LoginAttemptState>>,
}

#[derive(Default)]
struct LoginAttemptState {
    clients: HashMap<Option<IpAddr>, LoginAttemptWindow>,
}

struct LoginAttemptWindow {
    started: Instant,
    attempts: u8,
}

impl LoginAttemptLimiter {
    /// Reserve one login submission, returning whole seconds until retry when
    /// this peer has exhausted its window.
    pub(crate) async fn begin_attempt(&self, client: Option<IpAddr>) -> Result<(), u64> {
        self.begin_attempt_at(client, Instant::now()).await
    }

    async fn begin_attempt_at(&self, client: Option<IpAddr>, now: Instant) -> Result<(), u64> {
        let mut state = self.state.lock().await;
        if let Some(window) = state.clients.get_mut(&client) {
            let elapsed = now.saturating_duration_since(window.started);
            if elapsed >= LOGIN_ATTEMPT_WINDOW {
                *window = LoginAttemptWindow {
                    started: now,
                    attempts: 1,
                };
                return Ok(());
            }
            if window.attempts >= MAX_LOGIN_ATTEMPTS {
                let remaining = LOGIN_ATTEMPT_WINDOW.saturating_sub(elapsed);
                let seconds = remaining
                    .as_secs()
                    .saturating_add(u64::from(remaining.subsec_nanos() != 0))
                    .max(1);
                return Err(seconds);
            }
            window.attempts += 1;
            return Ok(());
        }

        if state.clients.len() >= MAX_TRACKED_LOGIN_CLIENTS {
            state.clients.retain(|_, window| {
                now.saturating_duration_since(window.started) < LOGIN_ATTEMPT_WINDOW
            });
            if state.clients.len() >= MAX_TRACKED_LOGIN_CLIENTS {
                if let Some(oldest) = state
                    .clients
                    .iter()
                    .min_by_key(|(_, window)| window.started)
                    .map(|(client, _)| *client)
                {
                    state.clients.remove(&oldest);
                }
            }
        }
        state.clients.insert(
            client,
            LoginAttemptWindow {
                started: now,
                attempts: 1,
            },
        );
        Ok(())
    }

    pub(crate) async fn clear(&self, client: Option<IpAddr>) {
        self.state.lock().await.clients.remove(&client);
    }
}

/// Tower middleware: require a valid Bearer token or signed session cookie.
/// Health remains public for orchestrator probes; metrics and control-plane
/// routes require credentials whenever API tokens are configured.
pub async fn require_auth(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();

    // Public endpoints — never require auth. The WebUI app shell and assets
    // must be public so the browser can render the login screen.
    if path == "/"
        || path == "/index.html"
        || path == "/favicon.ico"
        || path == "/health"
        || path.starts_with("/assets/")
    {
        return next.run(req).await;
    }

    // Several qBittorrent-compatible clients probe API support before logging in.
    if is_qbit_public_app_probe(&path) {
        return next.run(req).await;
    }

    // No-token mode is intended for loopback development and non-browser
    // clients. It still needs browser-origin checks: loopback-only does not
    // prevent a hostile website from posting cookie-free forms to localhost,
    // or opening a WebSocket and reading the event stream.
    if state.cfg.auth.api_tokens.is_empty() {
        if (path == "/ws" || (is_mutating(&req) && has_browser_request_headers(req.headers())))
            && !csrf_request_allowed(req.headers())
        {
            return (
                StatusCode::FORBIDDEN,
                "cross-origin browser request rejected",
            )
                .into_response();
        }
        return next.run(req).await;
    }

    // A bearer credential is not ambient browser state and is therefore not
    // subject to the cookie CSRF check below.
    if bearer_token(&req).is_some_and(|token| {
        state
            .cfg
            .auth
            .api_tokens
            .iter()
            .any(|allowed| tokens_match(allowed, &token))
    }) {
        return next.run(req).await;
    }

    // A reverse proxy may authenticate the user and pass that decision over a
    // loopback-only hop. This identity is ambient browser authentication, so
    // state changes and the private event stream still require same-origin
    // evidence. Config validation prevents clients from spoofing the header
    // over a public socket.
    if state.cfg.auth.trust_proxy_header && trusted_proxy_user(&req) {
        if (is_mutating(&req) || path == "/ws") && !csrf_request_allowed(req.headers()) {
            return (
                StatusCode::FORBIDDEN,
                "cross-site authenticated request rejected",
            )
                .into_response();
        }
        return next.run(req).await;
    }

    // Check the browser/qBit session cookie. qBit login issues this cookie when
    // the submitted username or password matches a configured API token.
    if cookie_token(&state, &req).is_some_and(|token| {
        state
            .cfg
            .auth
            .api_tokens
            .iter()
            .any(|allowed| tokens_match(allowed, &token))
    }) {
        // A cookie-authenticated WebSocket GET can expose the same private
        // event stream as an API read. Browsers attach cookies to same-site
        // cross-origin WebSocket handshakes, so the ordinary safe-method
        // exemption would permit cross-origin event reads from a sibling
        // subdomain. Require same-origin evidence for this route as well.
        if (is_mutating(&req) || path == "/ws") && !csrf_request_allowed(req.headers()) {
            return (StatusCode::FORBIDDEN, "cross-site cookie mutation rejected").into_response();
        }
        return next.run(req).await;
    }

    // Only the documented login/logout endpoints are public. Do not make an
    // accidentally added future auth route public by prefix matching.
    if is_public_auth_path(&path) {
        if is_mutating(&req)
            && has_browser_request_headers(req.headers())
            && !csrf_request_allowed(req.headers())
        {
            return (
                StatusCode::FORBIDDEN,
                "cross-origin browser authentication request rejected",
            )
                .into_response();
        }
        if is_public_login_path(&path) && req.method() == Method::POST {
            let peer_ip = peer_ip(&req);
            if let Err(retry_after_secs) = state.login_attempt_limiter.begin_attempt(peer_ip).await
            {
                let mut response = (StatusCode::TOO_MANY_REQUESTS, "Fails.").into_response();
                response.headers_mut().insert(
                    header::RETRY_AFTER,
                    HeaderValue::from_str(&retry_after_secs.to_string())
                        .expect("integer Retry-After is a valid header value"),
                );
                return response;
            }
        }
        return next.run(req).await;
    }

    (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
}

/// Create an opaque, expiring session cookie. The API token is never placed
/// in the cookie when a session secret is configured; qBittorrent only needs
/// a stable opaque SID value and the middleware can verify it against the
/// configured token set.
pub(crate) fn session_cookie_value(secret: Option<&str>, token: &str) -> String {
    let Some(secret) = secret.filter(|value| !value.is_empty()) else {
        // Local/no-auth compatibility fixtures historically used the token as
        // their cookie value. Keep that mode only when no signing secret was
        // configured; public binds reject this configuration.
        return urlencoding::encode(token).into_owned();
    };

    let expires = unix_now().saturating_add(SESSION_TTL_SECS);
    let mut nonce = [0_u8; 16];
    rand::rng().fill_bytes(&mut nonce);
    let nonce = hex::encode(nonce);
    let payload = format!("{token}.{expires}.{nonce}");
    let mut mac = SessionMac::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts keys of every length");
    mac.update(payload.as_bytes());
    format!(
        "tng1.{expires}.{nonce}.{}",
        hex::encode(mac.finalize().into_bytes())
    )
}

/// Constant-time credential comparison. A configured API token is a secret;
/// comparing it with `==` short-circuits on the first differing byte and
/// leaks how many leading bytes a guess got right to a network attacker who
/// can measure response timing.
pub(crate) fn tokens_match(allowed: &str, candidate: &str) -> bool {
    allowed.as_bytes().ct_eq(candidate.as_bytes()).into()
}

fn bearer_token(req: &Request<Body>) -> Option<String> {
    let mut parts = req
        .headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())?
        .split_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if parts.next().is_some() || !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    Some(token.to_owned())
}

fn trusted_proxy_user(req: &Request<Body>) -> bool {
    req.headers()
        .get("X-Remote-User")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let value = value.trim();
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        })
}

fn peer_ip(req: &Request<Body>) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

fn cookie_token(state: &AppState, req: &Request<Body>) -> Option<String> {
    let cookie = req.headers().get("Cookie")?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let part = part.trim();
        let value = part
            .strip_prefix("tng_session=")
            .or_else(|| part.strip_prefix("SID="))?;
        let decoded = urlencoding::decode(value).ok()?.into_owned();
        if let Some(secret) = state.cfg.auth.secret_key.as_deref() {
            return verify_signed_session(secret, &state.cfg.auth.api_tokens, &decoded);
        }
        state
            .cfg
            .auth
            .api_tokens
            .iter()
            .find(|token| tokens_match(token, &decoded))
            .cloned()
    })
}

fn verify_signed_session(secret: &str, tokens: &[String], value: &str) -> Option<String> {
    let mut parts = value.split('.');
    let version = parts.next()?;
    let expires = parts.next()?.parse::<u64>().ok()?;
    let nonce = parts.next()?;
    let signature = parts.next()?;
    if version != "tng1"
        || parts.next().is_some()
        || expires < unix_now()
        || nonce.len() != 32
        || signature.len() != 64
        || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !signature.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let signature = hex::decode(signature).ok()?;
    let payload_suffix = format!(".{expires}.{nonce}");
    for token in tokens {
        let payload = format!("{token}{payload_suffix}");
        let mut mac = SessionMac::new_from_slice(secret.as_bytes()).ok()?;
        mac.update(payload.as_bytes());
        if mac.verify_slice(&signature).is_ok() {
            return Some(token.clone());
        }
    }
    None
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn is_mutating(req: &Request<Body>) -> bool {
    matches!(
        *req.method(),
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    )
}

fn has_browser_request_headers(headers: &HeaderMap) -> bool {
    headers.contains_key("Origin")
        || headers.contains_key("Referer")
        || headers.contains_key("Sec-Fetch-Site")
}

fn csrf_request_allowed(headers: &axum::http::HeaderMap) -> bool {
    if let Some(value) = headers.get("Sec-Fetch-Site") {
        if !value
            .to_str()
            .is_ok_and(|value| value.eq_ignore_ascii_case("same-origin"))
        {
            return false;
        }
    }
    // Fail closed rather than open: a mutating cookie-authenticated request
    // needs positive same-origin evidence. A missing Host header, or an
    // Origin/Referer-free request that Sec-Fetch-Site also didn't label, is
    // not proof of same-origin — it's simply a client that omitted the
    // headers this check relies on.
    let Some(host) = headers.get("Host").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let origin = headers.get("Origin").and_then(|value| value.to_str().ok());
    let referer = headers.get("Referer").and_then(|value| value.to_str().ok());
    if origin.is_none() && referer.is_none() {
        return false;
    }
    for (value, required) in [(origin, true), (referer, false)] {
        let Some(value) = value else { continue };
        if !same_origin_authority(value, host, required) {
            return false;
        }
    }
    true
}

fn same_origin_authority(value: &str, host: &str, required: bool) -> bool {
    let value = value.trim();
    if value.eq_ignore_ascii_case("null") {
        return false;
    }
    let Some(scheme_end) = value.find("://") else {
        return false;
    };
    if required && scheme_end == 0 {
        return false;
    }
    let authority = value[scheme_end + 3..]
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

fn is_qbit_public_app_probe(path: &str) -> bool {
    matches!(
        path,
        "/api/qb/v2/app/version"
            | "/api/qb/v2/app/webapiVersion"
            | "/api/v2/app/version"
            | "/api/v2/app/webapiVersion"
    )
}

fn is_public_auth_path(path: &str) -> bool {
    matches!(
        path,
        "/api/v1/auth/login"
            | "/api/v1/auth/logout"
            | "/api/qb/v2/auth/login"
            | "/api/qb/v2/auth/logout"
            | "/api/v2/auth/login"
            | "/api/v2/auth/logout"
    )
}

fn is_public_login_path(path: &str) -> bool {
    matches!(
        path,
        "/api/v1/auth/login" | "/api/qb/v2/auth/login" | "/api/v2/auth/login"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn tokens_match_accepts_equal_secrets() {
        assert!(tokens_match("same-token", "same-token"));
    }

    #[test]
    fn tokens_match_rejects_different_secrets_of_equal_length() {
        assert!(!tokens_match("token-aaaa", "token-bbbb"));
    }

    #[test]
    fn tokens_match_rejects_different_length_secrets() {
        assert!(!tokens_match("short", "much-longer-token"));
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_but_credential_shape_is_exact() {
        let request = Request::builder()
            .header("Authorization", "bearer opaque-token")
            .body(Body::empty())
            .unwrap();
        assert_eq!(bearer_token(&request).as_deref(), Some("opaque-token"));

        let request = Request::builder()
            .header("Authorization", "Bearer opaque-token extra")
            .body(Body::empty())
            .unwrap();
        assert!(bearer_token(&request).is_none());
    }

    #[test]
    fn csrf_allows_same_origin_request_via_origin_header() {
        let h = headers(&[("Host", "example.com"), ("Origin", "https://example.com")]);
        assert!(csrf_request_allowed(&h));
    }

    #[test]
    fn csrf_allows_same_origin_request_via_referer_when_origin_absent() {
        let h = headers(&[
            ("Host", "example.com"),
            ("Referer", "https://example.com/page"),
        ]);
        assert!(csrf_request_allowed(&h));
    }

    #[test]
    fn csrf_rejects_cross_origin_request() {
        let h = headers(&[("Host", "example.com"), ("Origin", "https://evil.example")]);
        assert!(!csrf_request_allowed(&h));
    }

    #[test]
    fn csrf_rejects_sec_fetch_site_cross_site_even_with_matching_origin() {
        let h = headers(&[
            ("Host", "example.com"),
            ("Origin", "https://example.com"),
            ("Sec-Fetch-Site", "cross-site"),
        ]);
        assert!(!csrf_request_allowed(&h));
    }

    #[test]
    fn csrf_rejects_same_site_metadata_even_with_matching_authority() {
        let h = headers(&[
            ("Host", "example.com"),
            ("Origin", "https://example.com"),
            ("Sec-Fetch-Site", "same-site"),
        ]);
        assert!(!csrf_request_allowed(&h));
    }

    #[test]
    fn csrf_fails_closed_without_origin_or_referer() {
        // No Origin/Referer is not proof of same-origin; some clients simply
        // omit both. Absent evidence must not be treated as a same-origin pass.
        let h = headers(&[("Host", "example.com")]);
        assert!(!csrf_request_allowed(&h));
    }

    #[test]
    fn csrf_fails_closed_without_host() {
        let h = headers(&[("Origin", "https://example.com")]);
        assert!(!csrf_request_allowed(&h));
    }

    #[test]
    fn public_login_path_match_is_exact() {
        assert!(is_public_login_path("/api/v1/auth/login"));
        assert!(is_public_login_path("/api/qb/v2/auth/login"));
        assert!(is_public_login_path("/api/v2/auth/login"));
        assert!(!is_public_login_path("/api/v2/auth/logout"));
        assert!(!is_public_login_path("/api/v2/auth/login/extra"));
    }

    #[tokio::test]
    async fn login_attempt_limiter_is_per_peer_and_expires() {
        let limiter = LoginAttemptLimiter::default();
        let first: IpAddr = "192.0.2.10".parse().unwrap();
        let second: IpAddr = "192.0.2.11".parse().unwrap();
        let now = Instant::now();

        for _ in 0..MAX_LOGIN_ATTEMPTS {
            assert!(limiter.begin_attempt_at(Some(first), now).await.is_ok());
        }
        assert_eq!(
            limiter.begin_attempt_at(Some(first), now).await,
            Err(LOGIN_ATTEMPT_WINDOW.as_secs())
        );
        assert!(limiter.begin_attempt_at(Some(second), now).await.is_ok());
        assert!(limiter
            .begin_attempt_at(Some(first), now + LOGIN_ATTEMPT_WINDOW)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn login_attempt_limiter_bounds_tracked_peers() {
        let limiter = LoginAttemptLimiter::default();
        let now = Instant::now();
        {
            let mut state = limiter.state.lock().await;
            for address in 0..MAX_TRACKED_LOGIN_CLIENTS as u32 {
                state.clients.insert(
                    Some(IpAddr::V4(std::net::Ipv4Addr::from(address))),
                    LoginAttemptWindow {
                        started: now,
                        attempts: 1,
                    },
                );
            }
        }

        let new_peer = Some(IpAddr::V4("203.0.113.7".parse().unwrap()));
        assert!(limiter.begin_attempt_at(new_peer, now).await.is_ok());
        assert_eq!(
            limiter.state.lock().await.clients.len(),
            MAX_TRACKED_LOGIN_CLIENTS
        );
    }
}
