use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use rt_api_model::{ApiError, TorrentSummary};

use crate::state::AppState;

/// `GET /api/v1/torrents` — list all torrents.
pub async fn list_torrents(State(state): State<AppState>) -> impl IntoResponse {
    let reg = state.registry.read().await;
    let summaries: Vec<TorrentSummary> = reg
        .iter()
        .map(|e| TorrentSummary {
            info_hash: e.info_hash.clone(),
            name: e.name.clone(),
            state: e.state.as_str().to_owned(),
            total_length: 0,
            downloaded: e.stats.downloaded as i64,
            uploaded: e.stats.uploaded as i64,
            ratio: e.stats.ratio(),
            save_path: e.save_path.clone(),
            category: e.category.clone(),
            tags: e.tags.clone(),
            added_at: e.added_at as i64,
            completed_at: e.completed_at.map(|t| t as i64),
            num_peers: 0,
            num_seeds: 0,
        })
        .collect();
    (StatusCode::OK, Json(summaries))
}

/// `GET /api/v1/torrents/{hash}` — get one torrent.
pub async fn get_torrent(
    State(state): State<AppState>,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    let reg = state.registry.read().await;
    match reg.get(&info_hash) {
        Some(e) => {
            let summary = TorrentSummary {
                info_hash: e.info_hash.clone(),
                name: e.name.clone(),
                state: e.state.as_str().to_owned(),
                total_length: 0,
                downloaded: e.stats.downloaded as i64,
                uploaded: e.stats.uploaded as i64,
                ratio: e.stats.ratio(),
                save_path: e.save_path.clone(),
                category: e.category.clone(),
                tags: e.tags.clone(),
                added_at: e.added_at as i64,
                completed_at: e.completed_at.map(|t| t as i64),
                num_peers: 0,
                num_seeds: 0,
            };
            (StatusCode::OK, Json(serde_json::to_value(summary).unwrap())).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(
                serde_json::to_value(ApiError::not_found(format!(
                    "torrent {info_hash} not found"
                )))
                .unwrap(),
            ),
        )
            .into_response(),
    }
}

/// `DELETE /api/v1/torrents/{hash}` — remove a torrent.
pub async fn delete_torrent(
    State(state): State<AppState>,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    let mut reg = state.registry.write().await;
    match reg.remove(&info_hash) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(
                serde_json::to_value(ApiError::not_found(format!(
                    "torrent {info_hash} not found"
                )))
                .unwrap(),
            ),
        )
            .into_response(),
    }
}

/// `GET /health` — liveness probe.
pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use rt_session::TorrentEntry;
    use tower::ServiceExt;

    use crate::router::build_router;

    async fn setup_app_with_torrent() -> (axum::Router, String) {
        let state = AppState::new();
        let hash = "a".repeat(40);
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.clone(),
                "my.torrent".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        (build_router(state), hash)
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_torrents_empty() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }

    #[tokio::test]
    async fn list_torrents_with_entry() {
        let (app, _hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/torrents/{hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_torrent_not_found() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/torrents/{hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_torrent_not_found() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/torrents/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
