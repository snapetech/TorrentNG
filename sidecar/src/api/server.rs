use axum::{
    middleware,
    routing::{delete, get, post, put},
    Router,
};
use tower_http::limit::RequestBodyLimitLayer;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::{
    auth::require_auth,
    cache::Db,
    config::Config,
    metrics::SharedMetrics,
    rtorrent::Client,
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
    let native = Router::new()
        .route("/torrents",                   get(handlers::list_torrents).post(handlers::add_torrent))
        .route("/torrents/{hash}",            delete(handlers::delete_torrent))
        .route("/torrents/{hash}/start",      post(handlers::torrent_start))
        .route("/torrents/{hash}/stop",       post(handlers::torrent_stop))
        .route("/torrents/{hash}/recheck",    post(handlers::torrent_recheck))
        .route("/torrents/{hash}/reannounce", post(handlers::torrent_reannounce))
        .route("/torrents/{hash}/trackers",   get(handlers::torrent_trackers))
        .route("/torrents/{hash}/files",      get(handlers::torrent_files).patch(handlers::set_file_priorities))
        .route("/torrents/{hash}/category",   put(handlers::set_torrent_category))
        .route("/torrents/{hash}/tags",       post(handlers::add_torrent_tags).delete(handlers::remove_torrent_tags))
        .route("/categories",                 get(handlers::list_categories).post(handlers::upsert_category))
        .route("/categories/{name}",          delete(handlers::delete_category))
        .route("/tags",                       get(handlers::list_tags).post(handlers::create_tag))
        .route("/tags/{name}",                delete(handlers::delete_tag))
        .route("/bulk/{action}",              post(handlers::bulk_action))
        .route("/settings/user-agent",        get(handlers::get_user_agent).put(handlers::set_user_agent));

    let qb = crate::qbcompat::build_router(state.clone());

    Router::new()
        .nest("/api/v1", native)
        .nest("/api/qb/v2", qb)
        .route("/health",  get(handlers::health))
        .route("/metrics", get(handlers::metrics_handler))
        .route("/ws",      get(super::ws::handler))
        .layer(RequestBodyLimitLayer::new(64 * 1024 * 1024)) // 64MB max body
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state)
}
