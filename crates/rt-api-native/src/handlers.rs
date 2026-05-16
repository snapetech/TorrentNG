use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    Json,
};
use base64::Engine as _;
use rt_api_model::{
    AddTorrentRequest, AddTorrentResponse, ApiError, FileInfo, TorrentDetail, TorrentSummary,
};
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_session::TorrentState;

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
            total_length: e.total_length as i64,
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

/// `POST /api/v1/torrents` — add a v1/hybrid `.torrent` from base64 JSON.
pub async fn add_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AddTorrentRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal(
                    "native engine is not available".to_owned(),
                ))
                .unwrap(),
            ),
        )
            .into_response();
    };

    if let Some(magnet) = req
        .magnet
        .as_deref()
        .filter(|magnet| !magnet.trim().is_empty())
    {
        let magnet = match parse_magnet(magnet) {
            Ok(magnet) => magnet,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::to_value(ApiError::bad_request(e.to_string())).unwrap()),
                )
                    .into_response();
            }
        };
        let save_path = if req.save_path.trim().is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(req.save_path))
        };
        let paused = !req.start.unwrap_or(true);
        return match engine
            .add_magnet_with_labels(
                magnet,
                save_path,
                paused,
                req.category,
                req.tags.unwrap_or_default(),
            )
            .await
        {
            Ok(info_hash) => (
                StatusCode::CREATED,
                Json(serde_json::to_value(AddTorrentResponse { info_hash }).unwrap()),
            )
                .into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response(),
        };
    }

    let Some(torrent_b64) = req.torrent_b64.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ApiError::bad_request("torrent_b64 is required".to_owned()))
                    .unwrap(),
            ),
        )
            .into_response();
    };

    let raw = match base64::engine::general_purpose::STANDARD.decode(torrent_b64) {
        Ok(raw) => raw,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ApiError::bad_request(format!(
                        "invalid torrent_b64: {e}"
                    )))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    };
    let meta = match parse_torrent(&raw) {
        Ok(meta) => meta,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ApiError::bad_request(format!(
                        "invalid torrent metadata: {e}"
                    )))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    };

    let save_path = if req.save_path.trim().is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(req.save_path))
    };
    let paused = !req.start.unwrap_or(true);

    match engine
        .add_torrent_with_labels(
            meta,
            save_path,
            paused,
            req.category,
            req.tags.unwrap_or_default(),
        )
        .await
    {
        Ok(info_hash) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(AddTorrentResponse { info_hash }).unwrap()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
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
                total_length: e.total_length as i64,
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
            if let Some(engine) = &state.engine {
                match engine.torrent_metadata(info_hash.clone()).await {
                    Ok(meta) => {
                        let detail = TorrentDetail {
                            summary,
                            piece_length: meta.piece_length as i64,
                            piece_count: meta.piece_count as i64,
                            is_private: meta.is_private,
                            trackers: meta.trackers,
                            files: meta
                                .files
                                .into_iter()
                                .map(|file| FileInfo {
                                    file_index: file.index,
                                    path: file.path,
                                    length: file.length as i64,
                                    priority: 1,
                                })
                                .collect(),
                        };
                        (StatusCode::OK, Json(serde_json::to_value(detail).unwrap()))
                            .into_response()
                    }
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
                    )
                        .into_response(),
                }
            } else {
                (StatusCode::OK, Json(serde_json::to_value(summary).unwrap())).into_response()
            }
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
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    if let Some(engine) = &state.engine {
        match engine.remove_torrent(info_hash.clone(), false).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => not_found(info_hash),
        }
    } else {
        let mut reg = state.registry.write().await;
        let _ = reg.remove(&info_hash);
        StatusCode::NO_CONTENT.into_response()
    }
}

/// `POST /api/v1/torrents/{hash}/pause` — pause a torrent.
pub async fn pause_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    control_torrent(state, headers, info_hash, TorrentControl::Pause).await
}

/// `POST /api/v1/torrents/{hash}/resume` — resume a torrent.
pub async fn resume_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    control_torrent(state, headers, info_hash, TorrentControl::Resume).await
}

/// `POST /api/v1/torrents/{hash}/recheck` — force a piece hash recheck.
pub async fn recheck_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    control_torrent(state, headers, info_hash, TorrentControl::Recheck).await
}

/// `POST /api/v1/torrents/{hash}/reannounce` — force tracker announce.
pub async fn reannounce_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    control_torrent(state, headers, info_hash, TorrentControl::Reannounce).await
}

/// `GET /api/v1/torrents/{hash}/diagnostics` — explain why a torrent is not seeding.
pub async fn diagnose_torrent(
    State(state): State<AppState>,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal(
                    "native engine is not available".to_owned(),
                ))
                .unwrap(),
            ),
        )
            .into_response();
    };
    match engine.diagnose_torrent(info_hash.clone()).await {
        Ok(diagnostic) => (
            StatusCode::OK,
            Json(serde_json::to_value(diagnostic).unwrap()),
        )
            .into_response(),
        Err(_) => not_found(info_hash),
    }
}

/// `GET /health` — native engine readiness probe.
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let torrent_count = state.registry.read().await.iter().count();
    let ready = state.engine.is_some();
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(serde_json::json!({
            "status": if ready { "ok" } else { "unavailable" },
            "ready": ready,
            "native_engine": ready,
            "torrent_count": torrent_count,
        })),
    )
}

/// `GET /metrics` — Prometheus text exposition for native engine state.
pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            "# native engine unavailable\n".to_owned(),
        );
    };
    match engine.stats().await {
        Ok(stats) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            render_metrics(&stats),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            format!("# failed to collect native engine metrics: {e}\n"),
        ),
    }
}

fn render_metrics(stats: &rt_engine::EngineStats) -> String {
    let mut out = String::new();
    metric(
        &mut out,
        "rtorrentng_torrents_total",
        "gauge",
        "Total torrents in session",
        stats.torrents_total,
    );
    metric(
        &mut out,
        "rtorrentng_torrents_seeding",
        "gauge",
        "Currently seeding torrents",
        stats.torrents_seeding,
    );
    metric(
        &mut out,
        "rtorrentng_torrents_downloading",
        "gauge",
        "Currently downloading torrents",
        stats.torrents_downloading,
    );
    metric(
        &mut out,
        "rtorrentng_torrents_paused",
        "gauge",
        "Paused or stopped torrents",
        stats.torrents_paused,
    );
    metric(
        &mut out,
        "rtorrentng_torrents_checking",
        "gauge",
        "Torrents checking pieces",
        stats.torrents_checking,
    );
    metric(
        &mut out,
        "rtorrentng_torrents_metadata_pending",
        "gauge",
        "Metadata-pending torrents",
        stats.torrents_metadata_pending,
    );
    metric(
        &mut out,
        "rtorrentng_torrents_queued",
        "gauge",
        "Queued torrents",
        stats.torrents_queued,
    );
    metric(
        &mut out,
        "rtorrentng_torrents_errored",
        "gauge",
        "Errored torrents",
        stats.torrents_error,
    );
    metric(
        &mut out,
        "rtorrentng_bytes_uploaded_total",
        "counter",
        "Uploaded bytes from session accounting",
        stats.bytes_uploaded,
    );
    metric(
        &mut out,
        "rtorrentng_bytes_downloaded_total",
        "counter",
        "Downloaded bytes from session accounting",
        stats.bytes_downloaded,
    );
    metric(
        &mut out,
        "rtorrentng_bytes_left",
        "gauge",
        "Bytes left across enabled torrent pieces",
        stats.bytes_left,
    );
    metric(
        &mut out,
        "rtorrentng_jobs_active",
        "gauge",
        "Active durable jobs",
        stats.jobs_active,
    );
    metric(
        &mut out,
        "rtorrentng_trackers_total",
        "gauge",
        "Persisted tracker rows",
        stats.trackers_total,
    );
    metric(
        &mut out,
        "rtorrentng_trackers_working",
        "gauge",
        "Trackers in working state",
        stats.trackers_working,
    );
    metric(
        &mut out,
        "rtorrentng_trackers_warning",
        "gauge",
        "Trackers with warning state",
        stats.trackers_warning,
    );
    metric(
        &mut out,
        "rtorrentng_trackers_error",
        "gauge",
        "Trackers with error state",
        stats.trackers_error,
    );
    out
}

fn metric(out: &mut String, name: &str, kind: &str, help: &str, value: u64) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
    out.push_str(name);
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

enum TorrentControl {
    Pause,
    Resume,
    Recheck,
    Reannounce,
}

async fn control_torrent(
    state: AppState,
    headers: HeaderMap,
    info_hash: String,
    control: TorrentControl,
) -> axum::response::Response {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    if let Some(engine) = &state.engine {
        let result = match control {
            TorrentControl::Pause => engine.pause_torrent(info_hash.clone()).await,
            TorrentControl::Resume => engine.resume_torrent(info_hash.clone()).await,
            TorrentControl::Recheck => engine.recheck_torrent(info_hash.clone()).await,
            TorrentControl::Reannounce => engine.reannounce_torrent(info_hash.clone()).await,
        };
        return match result {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => not_found(info_hash),
        };
    }

    let mut reg = state.registry.write().await;
    if let Some(entry) = reg.get_mut(&info_hash) {
        match control {
            TorrentControl::Pause => {
                let _ = entry.transition(TorrentState::Paused);
            }
            TorrentControl::Resume => {
                let _ = entry.transition(TorrentState::Downloading);
            }
            TorrentControl::Recheck => {
                let _ = entry.transition(TorrentState::Checking);
            }
            TorrentControl::Reannounce => {}
        }
        StatusCode::NO_CONTENT.into_response()
    } else {
        not_found(info_hash)
    }
}

fn require_mutation_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<axum::response::Response> {
    if state.api_tokens.is_empty()
        || presented_token(headers).is_some_and(|token| token_allowed(state, token))
    {
        return None;
    }
    Some(
        (
            StatusCode::UNAUTHORIZED,
            Json(
                serde_json::to_value(ApiError::new(
                    "UNAUTHORIZED",
                    "missing or invalid API token",
                ))
                .unwrap(),
            ),
        )
            .into_response(),
    )
}

fn token_allowed(state: &AppState, token: &str) -> bool {
    state.api_tokens.iter().any(|allowed| allowed == token)
}

fn presented_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(extract_session_cookie)
        })
}

fn extract_session_cookie(cookie: &str) -> Option<&str> {
    cookie.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("rtng_session=")
    })
}

async fn torrent_exists(state: &AppState, info_hash: &str) -> bool {
    state.registry.read().await.get(info_hash).is_some()
}

fn not_found(info_hash: String) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(
            serde_json::to_value(ApiError::not_found(format!(
                "torrent {info_hash} not found"
            )))
            .unwrap(),
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use rt_session::{SessionRegistry, TorrentEntry};
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

    async fn setup_authed_app_with_torrent() -> (axum::Router, String) {
        let state = AppState {
            registry: std::sync::Arc::new(tokio::sync::RwLock::new(SessionRegistry::new())),
            engine: None,
            api_tokens: std::sync::Arc::new(vec!["secret-token".to_owned()]),
        };
        let hash = "b".repeat(40);
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.clone(),
                "secure.torrent".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        (build_router(state), hash)
    }

    #[tokio::test]
    async fn health_reports_unavailable_without_engine() {
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
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn metrics_reports_unavailable_without_engine() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn diagnostics_without_engine_returns_unavailable() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/torrents/{hash}/diagnostics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn render_metrics_includes_engine_stats() {
        let stats = rt_engine::EngineStats {
            torrents_total: 2,
            torrents_seeding: 1,
            jobs_active: 3,
            trackers_error: 4,
            ..Default::default()
        };
        let rendered = render_metrics(&stats);
        assert!(rendered.contains("rtorrentng_torrents_total 2"));
        assert!(rendered.contains("rtorrentng_torrents_seeding 1"));
        assert!(rendered.contains("rtorrentng_jobs_active 3"));
        assert!(rendered.contains("rtorrentng_trackers_error 4"));
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
    async fn add_torrent_without_engine_returns_unavailable() {
        let state = AppState::new();
        let app = build_router(state);
        let body = serde_json::json!({
            "save_path": "/data",
            "torrent_b64": base64::engine::general_purpose::STANDARD.encode(b"not a torrent"),
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/torrents")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
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
    async fn mutating_endpoint_requires_configured_token() {
        let (app, hash) = setup_authed_app_with_torrent().await;
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
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mutating_endpoint_accepts_bearer_token() {
        let (app, hash) = setup_authed_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/torrents/{hash}"))
                    .header(header::AUTHORIZATION, "Bearer secret-token")
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

    #[tokio::test]
    async fn pause_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/torrents/{hash}/pause"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn resume_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/torrents/{hash}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn recheck_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/torrents/{hash}/recheck"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn reannounce_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/torrents/{hash}/reannounce"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }
}
