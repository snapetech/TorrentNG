use axum::{
    middleware,
    routing::{delete, get, post, put},
    Router,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    auth::require_auth, cache::Db, config::Config, metrics::SharedMetrics, rtorrent::Client,
};

use super::handlers;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub rt: Arc<Client>,
    pub db: Arc<Db>,
    pub events: broadcast::Sender<crate::api::ws::Event>,
    pub metrics: SharedMetrics,
}

pub fn build_router(state: AppState) -> Router {
    let qb = crate::qbcompat::build_router(state.clone());

    let static_dir = std::env::var("RTNG_STATIC_DIR").unwrap_or_else(|_| "static".to_owned());

    Router::new()
        // Native API — axum 0.7 uses :param syntax
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
        .route("/api/v1/tracker-health", get(handlers::tracker_health))
        .route("/api/v1/engine", get(handlers::engine_diagnostics))
        .route("/api/v1/engine/commands", get(handlers::engine_commands))
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
        // for explicit namespacing in rtorrentNG deployments.
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
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state)
}
