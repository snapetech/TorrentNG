use axum::{routing::get, Router};

use crate::{
    handlers::{
        add_torrent, delete_torrent, get_torrent, health, list_torrents, metrics, pause_torrent,
        reannounce_torrent, recheck_torrent, resume_torrent,
    },
    state::AppState,
};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/api/v1/torrents", get(list_torrents).post(add_torrent))
        .route(
            "/api/v1/torrents/:hash",
            get(get_torrent).delete(delete_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/pause",
            axum::routing::post(pause_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/resume",
            axum::routing::post(resume_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/recheck",
            axum::routing::post(recheck_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/reannounce",
            axum::routing::post(reannounce_torrent),
        )
        .with_state(state)
}
