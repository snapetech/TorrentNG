use axum::{routing::get, Router};

use crate::{
    handlers::{delete_torrent, get_torrent, health, list_torrents},
    state::AppState,
};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/torrents", get(list_torrents))
        .route(
            "/api/v1/torrents/:hash",
            get(get_torrent).delete(delete_torrent),
        )
        .with_state(state)
}
