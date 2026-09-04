use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::server::AppState;

type SessionMac = Hmac<Sha256>;
const SESSION_TTL_SECS: u64 = 24 * 60 * 60;

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

    // If no API tokens configured, allow everything (development / no-auth mode)
    if state.cfg.auth.api_tokens.is_empty() {
        return next.run(req).await;
    }

    // A reverse proxy may authenticate the user and pass that decision over a
    // loopback-only hop. Config validation rejects this mode on a non-loopback
    // listener, so a client cannot spoof the header over a public socket.
    if state.cfg.auth.trust_proxy_header && trusted_proxy_user(&req) {
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
            .any(|allowed| allowed == &token)
    }) {
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
            .any(|allowed| allowed == &token)
    }) {
        if is_mutating(&req) && !csrf_request_allowed(req.headers()) {
            return (StatusCode::FORBIDDEN, "cross-site cookie mutation rejected").into_response();
        }
        return next.run(req).await;
    }

    // Only the documented login/logout endpoints are public. Do not make an
    // accidentally added future auth route public by prefix matching.
    if is_public_auth_path(&path) {
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
    rand::thread_rng().fill_bytes(&mut nonce);
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

fn bearer_token(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
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
        Some(decoded)
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

fn csrf_request_allowed(headers: &axum::http::HeaderMap) -> bool {
    if headers
        .get("Sec-Fetch-Site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
    {
        return false;
    }
    let Some(host) = headers.get("Host").and_then(|value| value.to_str().ok()) else {
        return headers.get("Origin").is_none() && headers.get("Referer").is_none();
    };
    for (name, required) in [("Origin", true), ("Referer", false)] {
        let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) else {
            continue;
        };
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
