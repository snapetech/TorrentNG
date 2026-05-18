use axum::{
    body::Body,
    extract::{ConnectInfo, MatchedPath},
    http::{header, HeaderMap, HeaderName, HeaderValue, Request},
    middleware,
    response::Response,
    routing::{delete, get, post, put},
    Router,
};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::sync::{broadcast, RwLock};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    auth::require_auth, backend::TorrentBackend, cache::Db, config::Config, metrics::SharedMetrics,
    rtorrent::Client,
};

use super::handlers;

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
    pub qbit_rss_items: Arc<RwLock<serde_json::Map<String, serde_json::Value>>>,
}

pub fn build_router(state: AppState) -> Router {
    let qb = crate::qbcompat::build_router(state.clone());

    let static_dir = std::env::var("TNG_STATIC_DIR").unwrap_or_else(|_| "static".to_owned());

    Router::new()
        // Native API.
        .route(
            "/api/v1/torrents",
            get(handlers::list_torrents).post(handlers::add_torrent),
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
        .layer(RequestBodyLimitLayer::new(64 * 1024 * 1024))
        .layer(middleware::from_fn(request_log))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
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
        request_id = %request_id,
        method = %method,
        path = %path,
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

#[cfg(test)]
mod tests {
    use super::{request_id, skip_request_log};
    use axum::http::{HeaderMap, HeaderValue};

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
}
