use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use std::collections::HashMap;

use crate::{
    model::{to_qbit_state, QbServerState, QbTorrentInfo},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

/// `POST /api/qb/v2/auth/login` — always succeeds (auth handled at sidecar layer).
pub async fn auth_login() -> impl IntoResponse {
    (StatusCode::OK, "Ok.")
}

// ---------------------------------------------------------------------------
// App info
// ---------------------------------------------------------------------------

pub async fn app_version() -> impl IntoResponse {
    (StatusCode::OK, "v5.0.0")
}

pub async fn app_webapi_version() -> impl IntoResponse {
    (StatusCode::OK, "2.9.3")
}

// ---------------------------------------------------------------------------
// Torrents
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TorrentsInfoQuery {
    pub filter: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub sort: Option<String>,
    pub reverse: Option<bool>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub hashes: Option<String>,
}

pub async fn torrents_info(
    State(state): State<AppState>,
    Query(q): Query<TorrentsInfoQuery>,
) -> impl IntoResponse {
    let reg = state.registry.read().await;
    let infos: Vec<QbTorrentInfo> = reg
        .iter()
        .filter(|e| {
            // Filter by hashes if provided
            if let Some(ref hashes_str) = q.hashes {
                let hashes: Vec<&str> = hashes_str.split('|').collect();
                if !hashes.contains(&e.info_hash.as_str()) {
                    return false;
                }
            }
            // Filter by category
            if let Some(ref cat) = q.category {
                if e.category.as_deref() != Some(cat.as_str()) {
                    return false;
                }
            }
            // Filter by state
            if let Some(ref filter) = q.filter {
                let qb_state = to_qbit_state(e.state.as_str());
                match filter.as_str() {
                    "all" => {}
                    "downloading" => {
                        if qb_state != "downloading" {
                            return false;
                        }
                    }
                    "seeding" | "uploading" => {
                        if qb_state != "uploading" {
                            return false;
                        }
                    }
                    "completed" => {
                        if e.completed_at.is_none() {
                            return false;
                        }
                    }
                    "paused" => {
                        if !matches!(qb_state, "pausedUP" | "pausedDL") {
                            return false;
                        }
                    }
                    _ => {}
                }
            }
            true
        })
        .map(|e| QbTorrentInfo {
            hash: e.info_hash.clone(),
            name: e.name.clone(),
            state: to_qbit_state(e.state.as_str()).to_owned(),
            size: 0,
            downloaded: e.stats.downloaded as i64,
            uploaded: e.stats.uploaded as i64,
            ratio: e.stats.ratio(),
            save_path: format!("{}/", e.save_path.trim_end_matches('/')),
            category: e.category.clone().unwrap_or_default(),
            tags: e.tags.join(","),
            added_on: e.added_at as i64,
            completion_on: e.completed_at.map(|t| t as i64).unwrap_or(-1),
            num_leechs: 0,
            num_seeds: 0,
            dlspeed: 0,
            upspeed: 0,
            eta: -1,
            progress: if e.completed_at.is_some() { 1.0 } else { 0.0 },
            priority: 0,
            amount_left: 0,
            auto_tmm: false,
            tracker: String::new(),
            trackers_count: 0,
        })
        .collect();

    // Apply limit/offset
    let offset = q.offset.unwrap_or(0);
    let infos = if offset < infos.len() {
        &infos[offset..]
    } else {
        &[]
    };
    let infos: Vec<_> = if let Some(limit) = q.limit {
        infos.iter().take(limit).cloned().collect()
    } else {
        infos.to_vec()
    };

    (StatusCode::OK, Json(infos))
}

/// `POST /api/qb/v2/torrents/pause` — pause by hashes (pipe-separated or "all").
pub async fn torrents_pause(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let _hashes = extract_hashes(&body);
    // Transition to paused handled by session; here we just accept the request.
    // Real implementation would send commands to session supervisor.
    drop(state);
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/resume`.
pub async fn torrents_resume(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let _hashes = extract_hashes(&body);
    drop(state);
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/delete`.
pub async fn torrents_delete(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let mut reg = state.registry.write().await;
    for hash in hashes {
        let _ = reg.remove(&hash);
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/reannounce` — no-op stub (tracker engine handles this).
pub async fn torrents_reannounce() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/recheck` — no-op stub.
pub async fn torrents_recheck() -> impl IntoResponse {
    StatusCode::OK
}

/// `GET /api/qb/v2/torrents/trackers` — stub: returns empty list.
pub async fn torrents_trackers() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!([])))
}

/// `GET /api/qb/v2/torrents/files` — stub: returns empty list.
pub async fn torrents_files() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!([])))
}

/// `POST /api/qb/v2/torrents/setCategory`.
pub async fn torrents_set_category(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let category = params.get("category").cloned().unwrap_or_default();
    let mut reg = state.registry.write().await;
    for hash in &hashes {
        if let Some(e) = reg.get_mut(hash) {
            e.category = if category.is_empty() {
                None
            } else {
                Some(category.clone())
            };
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/addTags`.
pub async fn torrents_add_tags(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let new_tags: Vec<String> = params
        .get("tags")
        .map(|t| {
            t.split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut reg = state.registry.write().await;
    for hash in &hashes {
        if let Some(e) = reg.get_mut(hash) {
            for tag in &new_tags {
                if !e.tags.contains(tag) {
                    e.tags.push(tag.clone());
                }
            }
        }
    }
    StatusCode::OK
}

// ---------------------------------------------------------------------------
// Sync & Transfer
// ---------------------------------------------------------------------------

pub async fn sync_maindata(State(state): State<AppState>) -> impl IntoResponse {
    let reg = state.registry.read().await;
    let torrents: serde_json::Map<String, serde_json::Value> = reg
        .iter()
        .map(|e| {
            let info = QbTorrentInfo {
                hash: e.info_hash.clone(),
                name: e.name.clone(),
                state: to_qbit_state(e.state.as_str()).to_owned(),
                size: 0,
                downloaded: e.stats.downloaded as i64,
                uploaded: e.stats.uploaded as i64,
                ratio: e.stats.ratio(),
                save_path: format!("{}/", e.save_path.trim_end_matches('/')),
                category: e.category.clone().unwrap_or_default(),
                tags: e.tags.join(","),
                added_on: e.added_at as i64,
                completion_on: e.completed_at.map(|t| t as i64).unwrap_or(-1),
                num_leechs: 0,
                num_seeds: 0,
                dlspeed: 0,
                upspeed: 0,
                eta: -1,
                progress: if e.completed_at.is_some() { 1.0 } else { 0.0 },
                priority: 0,
                amount_left: 0,
                auto_tmm: false,
                tracker: String::new(),
                trackers_count: 0,
            };
            (e.info_hash.clone(), serde_json::to_value(info).unwrap())
        })
        .collect();
    let resp = serde_json::json!({
        "rid": 1,
        "full_update": true,
        "torrents": torrents,
        "torrents_removed": [],
        "server_state": QbServerState {
            dl_info_speed: 0,
            dl_info_data: 0,
            up_info_speed: 0,
            up_info_data: 0,
            connection_status: "connected".into(),
            free_space_on_disk: 0,
        }
    });
    (StatusCode::OK, Json(resp))
}

pub async fn transfer_info() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(QbServerState {
            dl_info_speed: 0,
            dl_info_data: 0,
            up_info_speed: 0,
            up_info_data: 0,
            connection_status: "connected".into(),
            free_space_on_disk: 0,
        }),
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_hashes(body: &str) -> Vec<String> {
    let params = parse_form_body(body);
    params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default()
}

fn extract_hashes_from_str(s: &str) -> Vec<String> {
    if s == "all" {
        return vec!["all".into()];
    }
    s.split('|')
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .collect()
}

fn parse_form_body(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            let k = it.next()?.to_owned();
            let v = it.next().unwrap_or("").to_owned();
            Some((k, v))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use rt_session::TorrentEntry;
    use tower::ServiceExt;

    use crate::router::build_qbit_router;

    async fn make_state_with(hash: &str) -> AppState {
        let state = AppState::new();
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.to_owned(),
                "name".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        state
    }

    #[tokio::test]
    async fn login_returns_ok() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/auth/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn app_version_ok() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/app/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn torrents_info_empty() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info")
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
    async fn torrents_info_with_entry() {
        let hash = "a".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["hash"].as_str().unwrap(), hash);
    }

    #[tokio::test]
    async fn sync_maindata_returns_full_update() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/sync/maindata")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["full_update"], true);
    }

    #[tokio::test]
    async fn transfer_info_ok() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/transfer/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["connection_status"].as_str().unwrap(), "connected");
    }
}
