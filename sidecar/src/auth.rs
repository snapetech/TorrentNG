use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::api::server::AppState;

/// Tower middleware: require a valid Bearer token or session cookie.
/// Skips /health and /metrics so monitoring never needs credentials.
pub async fn require_auth(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();

    // Public endpoints — never require auth
    if path == "/health" || path == "/metrics" {
        return next.run(req).await;
    }

    // If no API tokens configured, allow everything (development / no-auth mode)
    if state.cfg.auth.api_tokens.is_empty() {
        return next.run(req).await;
    }

    // Check Bearer token
    if let Some(auth_header) = req.headers().get("Authorization") {
        if let Ok(value) = auth_header.to_str() {
            if let Some(token) = value.strip_prefix("Bearer ") {
                if state.cfg.auth.api_tokens.iter().any(|t| t == token) {
                    return next.run(req).await;
                }
            }
        }
    }

    // Check session cookie (stub — full session management is Phase 4)
    if let Some(cookie) = req.headers().get("Cookie") {
        if let Ok(c) = cookie.to_str() {
            // Simple token-in-cookie check for browser sessions
            for part in c.split(';') {
                let part = part.trim();
                if let Some(val) = part.strip_prefix("rtng_session=") {
                    if state.cfg.auth.api_tokens.iter().any(|t| t == val) {
                        return next.run(req).await;
                    }
                }
            }
        }
    }

    // qBit compat login endpoint is always open
    if path.starts_with("/api/qb/v2/auth/") {
        return next.run(req).await;
    }

    (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
}
