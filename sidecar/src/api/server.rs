use axum::body::to_bytes;
use axum::{
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, MatchedPath},
    http::{header, HeaderMap, HeaderName, HeaderValue, Request, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Router,
};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::sync::{broadcast, Mutex, RwLock, Semaphore};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    auth::{require_auth, AuthCredentials},
    backend::TorrentBackend,
    cache::Db,
    config::Config,
    metrics::SharedMetrics,
    rtorrent::Client,
};

use super::handlers;

/// Keep request-driven backend, cache, and compatibility work bounded across
/// the whole sidecar rather than allowing each mounted API surface to create
/// its own unbounded queue of HTTP futures.
pub const MAX_CONCURRENT_API_REQUESTS: usize = 256;

/// Multipart torrent uploads are buffered before they reach a backend. Keep
/// their aggregate memory footprint separate from ordinary request admission.
pub const MAX_CONCURRENT_LARGE_UPLOADS: usize = 4;

/// WebSocket connections are long-lived and intentionally bypass the ordinary
/// request permit. They therefore need an independent admission limit.
pub const MAX_WS_CLIENTS: usize = 256;

/// Query extractors materialize the complete URI before a handler can clamp
/// individual parameters. Reject oversized request targets at the HTTP
/// boundary so a client cannot turn query parsing into an unbounded allocation.
pub const MAX_REQUEST_URI_BYTES: usize = 16 * 1024;

/// JSON control-plane requests carry metadata, not torrent payloads. Bound
/// them independently from the larger multipart upload ceiling so malformed
/// JSON cannot consume the upload budget before a handler validates fields.
pub const MAX_JSON_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Form login carries only credentials. Keep it far below the global body
/// limit so unauthenticated clients cannot force a large allocation in the
/// compatibility extractor.
pub const MAX_AUTH_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub rt: Arc<Client>,
    pub backend: Arc<dyn TorrentBackend>,
    pub db: Arc<Db>,
    pub events: broadcast::Sender<crate::api::ws::Event>,
    pub metrics: SharedMetrics,
    pub qbit_search_plugins: Arc<RwLock<serde_json::Map<String, serde_json::Value>>>,
    pub qbit_search_jobs: Arc<RwLock<serde_json::Map<String, serde_json::Value>>>,
    pub qbit_next_search_id: Arc<AtomicU64>,
    pub login_attempt_limiter: crate::auth::LoginAttemptLimiter,
    pub auth_credentials: Arc<RwLock<AuthCredentials>>,
    pub configured_auth_credentials: AuthCredentials,
    pub auth_settings_path: PathBuf,
    pub auth_settings_write: Arc<Mutex<()>>,
    pub local_webui_session_token: Option<String>,
    pub public_bind: bool,
    /// Serializes read-modify-write control-plane records stored as one JSON
    /// value in the cache database. The Db mutex protects individual SQL
    /// statements; this lock protects the multi-statement operation.
    pub control_plane_write: Arc<Mutex<()>>,
    pub request_concurrency: Arc<Semaphore>,
    pub large_uploads: Arc<Semaphore>,
    pub ws_clients: Arc<Semaphore>,
}

pub fn build_router(state: AppState) -> Router {
    let qb = crate::qbcompat::build_router(state.clone());

    let static_dir = std::env::var("TNG_STATIC_DIR")
        .or_else(|_| std::env::var("RTNG_STATIC_DIR"))
        .unwrap_or_else(|_| "static".to_owned());

    Router::new()
        // TorrentNG API.
        .route(
            "/api/v1/auth/login",
            post(crate::qbcompat::auth_login).layer(DefaultBodyLimit::max(
                crate::multipart::MAX_AUTH_REQUEST_BODY_BYTES,
            )),
        )
        .route("/api/v1/auth/logout", post(crate::qbcompat::auth_logout))
        .route(
            "/api/v1/auth/settings",
            get(crate::qbcompat::auth_settings)
                .put(crate::qbcompat::update_auth_settings)
                .delete(crate::qbcompat::reset_auth_settings)
                .layer(DefaultBodyLimit::max(
                    crate::multipart::MAX_AUTH_REQUEST_BODY_BYTES,
                )),
        )
        .route(
            "/api/v1/torrents",
            get(handlers::list_torrents)
                .post(handlers::add_torrent)
                .layer(DefaultBodyLimit::max(
                    crate::multipart::MAX_MULTIPART_REQUEST_BODY_BYTES,
                )),
        )
        .route(
            "/api/v1/torrents/live",
            get(handlers::live_torrent_stats),
        )
        .route(
            "/api/v1/torrents/{hash}",
            get(handlers::get_torrent)
                .put(handlers::update_torrent)
                .delete(handlers::delete_torrent),
        )
        .route(
            "/api/v1/torrents/{hash}/start",
            post(handlers::torrent_start),
        )
        .route("/api/v1/torrents/{hash}/stop", post(handlers::torrent_stop))
        .route(
            "/api/v1/torrents/{hash}/recheck",
            post(handlers::torrent_recheck),
        )
        .route(
            "/api/v1/torrents/{hash}/reannounce",
            post(handlers::torrent_reannounce),
        )
        .route(
            "/api/v1/torrents/{hash}/trackers",
            get(handlers::torrent_trackers).patch(handlers::patch_torrent_trackers),
        )
        .route(
            "/api/v1/torrents/{hash}/files",
            get(handlers::torrent_files).patch(handlers::set_file_priorities),
        )
        .route(
            "/api/v1/torrents/{hash}/category",
            put(handlers::set_torrent_category),
        )
        .route(
            "/api/v1/torrents/{hash}/tags",
            post(handlers::add_torrent_tags).delete(handlers::remove_torrent_tags),
        )
        .route(
            "/api/v1/categories",
            get(handlers::list_categories).post(handlers::upsert_category),
        )
        .route(
            "/api/v1/categories/{name}",
            delete(handlers::delete_category),
        )
        .route(
            "/api/v1/tags",
            get(handlers::list_tags).post(handlers::create_tag),
        )
        .route("/api/v1/tags/{name}", delete(handlers::delete_tag))
        .route("/api/v1/bulk/{action}", post(handlers::bulk_action))
        .route("/api/v1/storage", get(handlers::storage_roots))
        .route("/api/v1/transfer/info", get(handlers::transfer_info))
        .route("/api/v1/jobs", get(handlers::list_jobs))
        .route("/api/v1/logs", get(handlers::list_logs))
        .route("/api/v1/tracker-health", get(handlers::tracker_health))
        .route("/api/v1/sidebar-facets", get(handlers::sidebar_facets))
        .route("/api/v1/engine", get(handlers::engine_diagnostics))
        .route("/api/v1/engine/commands", get(handlers::engine_commands))
        .route(
            "/api/v1/engine/rtorrent-settings",
            get(handlers::get_rtorrent_settings).put(handlers::set_rtorrent_settings),
        )
        .route("/api/v1/engine/restart", post(handlers::restart_process))
        .route(
            "/api/v1/session/features",
            get(handlers::get_session_features)
                .patch(handlers::set_session_features)
                .put(handlers::set_session_features),
        )
        .route("/api/v1/cross-seed", post(handlers::cross_seed_helper))
        .route(
            "/api/v1/saved-views",
            get(handlers::list_saved_views).post(handlers::upsert_saved_view),
        )
        .route(
            "/api/v1/saved-views/{id}",
            delete(handlers::delete_saved_view),
        )
        .route(
            "/api/v1/ratio-groups",
            get(handlers::list_ratio_groups).post(handlers::upsert_ratio_group),
        )
        .route(
            "/api/v1/ratio-groups/{name}",
            post(handlers::apply_ratio_group).delete(handlers::delete_ratio_group),
        )
        .route(
            "/api/v1/workflows",
            get(handlers::list_workflows).post(handlers::upsert_workflow),
        )
        .route(
            "/api/v1/rss-rules",
            get(handlers::list_rss_rules).post(handlers::upsert_rss_rule),
        )
        .route("/api/v1/rss-rules/test", post(handlers::test_rss_rules))
        .route("/api/v1/rss-rules/apply", post(handlers::apply_rss_rules))
        .route("/api/v1/rss-rules/{id}", delete(handlers::delete_rss_rule))
        .route("/api/v1/workflow-runs", get(handlers::list_workflow_runs))
        .route(
            "/api/v1/workflows/{id}",
            post(handlers::run_workflow).delete(handlers::delete_workflow),
        )
        .route(
            "/api/v1/settings/user-agent",
            get(handlers::get_user_agent).put(handlers::set_user_agent),
        )
        // qBit compat. /api/v2 is the canonical qBittorrent path; /api/qb/v2 is kept
        // for explicit namespacing in TorrentNG deployments.
        .nest("/api/qb/v2", qb.clone())
        .nest("/api/v2", qb)
        // Infrastructure
        .route("/health", get(handlers::health))
        .route("/metrics", get(handlers::metrics_handler))
        .route("/ws", get(super::ws::handler))
        .fallback_service(
            ServeDir::new(&static_dir)
                .not_found_service(ServeFile::new(format!("{static_dir}/index.html"))),
        )
        .layer(RequestBodyLimitLayer::new(
            crate::multipart::MAX_MULTIPART_REQUEST_BODY_BYTES,
        ))
        .layer(DefaultBodyLimit::max(
            crate::multipart::MAX_DEFAULT_REQUEST_BODY_BYTES,
        ))
        .layer(middleware::from_fn(request_log))
        .layer(middleware::from_fn(json_body_guard))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .layer(middleware::from_fn_with_state(
            state.request_concurrency.clone(),
            request_concurrency_guard,
        ))
        .layer(middleware::from_fn(request_uri_guard))
        .with_state(state)
}

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

async fn request_log(req: Request<Body>, next: middleware::Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    if skip_request_log(&path) {
        return next.run(req).await;
    }
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned());
    let remote_addr = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);
    let request_id = request_id(req.headers());
    let started = Instant::now();
    let mut response = next.run(req).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-request-id"), value);
    }
    let status = response.status();
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let response_size = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    tracing::info!(
        component = "http",
        operation = "request",
        request_id = %crate::url_redaction::redact_display(&request_id),
        method = %crate::url_redaction::redact_display(&method),
        path = %crate::url_redaction::redact_display(&path),
        route = route.as_deref(),
        remote_addr = remote_addr.map(|addr| addr.to_string()).as_deref(),
        status = status.as_u16(),
        duration_ms,
        response_size,
        result = if status.is_server_error() { "error" } else { "ok" },
        "http request completed"
    );
    response
}

async fn request_uri_guard(req: Request<Body>, next: middleware::Next) -> Response {
    if !request_uri_is_bounded(req.uri()) {
        return (
            StatusCode::URI_TOO_LONG,
            "request URI exceeds the maximum length",
        )
            .into_response();
    }
    next.run(req).await
}

async fn json_body_guard(req: Request<Body>, next: middleware::Next) -> Response {
    if has_duplicate_content_type(req.headers()) {
        return (
            StatusCode::BAD_REQUEST,
            "duplicate Content-Type headers are not accepted",
        )
            .into_response();
    }
    if !is_json_content_type(req.headers()) {
        return next.run(req).await;
    }
    let (mut parts, body) = req.into_parts();
    let body = match to_bytes(body, MAX_JSON_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "JSON request body exceeds the maximum length",
            )
                .into_response();
        }
    };
    // The original Content-Length describes the consumed request body. Remove
    // it before handing the rebuilt body to downstream extractors so a stale
    // client-provided value cannot make them interpret the new body framing.
    parts.headers.remove(header::CONTENT_LENGTH);
    next.run(Request::from_parts(parts, Body::from(body))).await
}

fn has_duplicate_content_type(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::CONTENT_TYPE)
        .iter()
        .nth(1)
        .is_some()
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("application/json")
                || value
                    .strip_prefix("application/")
                    .is_some_and(|suffix| suffix.ends_with("+json"))
        })
}

fn request_uri_is_bounded(uri: &axum::http::Uri) -> bool {
    let uri_bytes = uri
        .path()
        .len()
        .saturating_add(uri.query().map_or(0, |query| query.len().saturating_add(1)));
    uri_bytes <= MAX_REQUEST_URI_BYTES
}

fn request_id(headers: &HeaderMap) -> String {
    rt_logging::correlation_id(
        headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        || format!("tng-{}", REQUEST_ID.fetch_add(1, Ordering::Relaxed)),
    )
}

fn skip_request_log(path: &str) -> bool {
    path == "/health"
        || path == "/metrics"
        || path == "/ws"
        || path == "/favicon.ico"
        || path.starts_with("/assets/")
        || path.starts_with("/static/")
        || is_static_asset_path(path)
}

fn is_static_asset_path(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, ext)| ext),
        Some("css" | "js" | "map" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico")
    )
}

/// Reject new ordinary work immediately when the sidecar is saturated. A
/// rejected request cannot accumulate behind a slow backend or cache worker.
async fn request_concurrency_guard(
    axum::extract::State(limiter): axum::extract::State<Arc<Semaphore>>,
    req: Request<Body>,
    next: middleware::Next,
) -> Response {
    if request_concurrency_bypass(req.uri().path()) {
        return next.run(req).await;
    }

    let Ok(_permit) = limiter.try_acquire() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "sidecar request capacity exhausted; retry later",
        )
            .into_response();
    };
    next.run(req).await
}

fn request_concurrency_bypass(path: &str) -> bool {
    // WebSockets own a separate client budget and remain open for the lifetime
    // of the connection. Ordinary HTTP responses, including health and
    // metrics, remain inside the shared request budget.
    path == "/ws"
}

#[cfg(test)]
mod tests {
    use super::{
        has_duplicate_content_type, is_json_content_type, request_concurrency_bypass, request_id,
        request_uri_is_bounded, skip_request_log, MAX_AUTH_BODY_BYTES, MAX_JSON_BODY_BYTES,
        MAX_REQUEST_URI_BYTES,
    };
    use axum::http::{header, HeaderMap, HeaderValue, Uri};

    #[test]
    fn request_log_skips_health_metrics_ws_and_static_assets() {
        for path in [
            "/health",
            "/metrics",
            "/ws",
            "/favicon.ico",
            "/assets/app.js",
            "/static/theme.css",
            "/index.css",
            "/logo.svg",
        ] {
            assert!(skip_request_log(path), "{path}");
        }
        assert!(!skip_request_log("/api/v1/torrents"));
        assert!(!skip_request_log("/api/qb/v2/log/main"));
    }

    #[test]
    fn request_id_accepts_bounded_safe_header_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-request-id",
            HeaderValue::from_static("client-123.trace/4"),
        );
        assert_eq!(request_id(&headers), "client-123.trace/4");

        headers.insert("x-request-id", HeaderValue::from_static("bad value"));
        assert!(request_id(&headers).starts_with("tng-"));

        headers.insert("x-request-id", HeaderValue::from_static(""));
        assert!(request_id(&headers).starts_with("tng-"));
    }

    #[test]
    fn websocket_is_the_only_long_lived_request_bypass() {
        assert!(request_concurrency_bypass("/ws"));
        assert!(!request_concurrency_bypass("/health"));
        assert!(!request_concurrency_bypass("/metrics"));
        assert!(!request_concurrency_bypass("/api/v1/torrents"));
    }

    #[test]
    fn json_content_type_detection_accepts_json_variants_only() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        assert!(is_json_content_type(&headers));
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json; charset=utf-8"),
        );
        assert!(is_json_content_type(&headers));
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        assert!(!is_json_content_type(&headers));
        assert_eq!(MAX_JSON_BODY_BYTES, 2 * 1024 * 1024);
        assert_eq!(MAX_AUTH_BODY_BYTES, 64 * 1024);
    }

    #[test]
    fn duplicate_content_type_headers_are_visible_to_the_boundary_guard() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.append(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        assert!(has_duplicate_content_type(&headers));
    }

    #[tokio::test]
    async fn request_uri_guard_rejects_oversized_targets_before_routing() {
        let uri: Uri = format!(
            "/api/v1/torrents?filter={}",
            "x".repeat(MAX_REQUEST_URI_BYTES)
        )
        .parse()
        .unwrap();
        assert!(!request_uri_is_bounded(&uri));

        let uri: Uri = "/api/v1/torrents?limit=100".parse().unwrap();
        assert!(request_uri_is_bounded(&uri));
    }
}
