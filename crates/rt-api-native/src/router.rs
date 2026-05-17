use axum::{
    routing::{get, post},
    Router,
};

use crate::{
    handlers::{
        add_torrent, delete_torrent, diagnose_torrent, get_torrent, health, list_session_events,
        list_torrents, metrics, pause_torrent, reannounce_torrent, recheck_torrent, resume_torrent,
        storage_execute_plan, storage_preview_plan, stream_events,
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
        .route("/api/v1/torrents/:hash/diagnostics", get(diagnose_torrent))
        .route("/api/v1/events", get(stream_events))
        .route("/api/v1/session-events", get(list_session_events))
        .route("/api/v1/jobs", get(crate::handlers::list_jobs))
        .route("/api/v1/storage/plan", post(storage_preview_plan))
        .route("/api/v1/storage/execute", post(storage_execute_plan))
        .with_state(state)
}
