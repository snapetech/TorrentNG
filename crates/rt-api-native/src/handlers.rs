use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use base64::Engine as _;
use futures::Stream;
use rt_api_model::{
    AddTorrentRequest, AddTorrentResponse, ApiError, FileInfo, TorrentDetail, TorrentSummary,
};
use rt_engine::{
    EngineGlobalLimits, EngineJob, EngineNetworkFeatures, EngineStorageRoot, EngineTorrentLimits,
    QueueMove,
};
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_metrics::MemoryClass;
use rt_session::{TorrentEntry, TorrentState};
use rt_storage::{
    runtime::StorageRuntime, DeletePlanRequest, ImportPlanRequest, MovePlanRequest, PlanIssue,
    PlannedStorageAction, StoragePlan, StoragePlanStep, STORAGE_LATENCY_BUCKETS_NS,
};
use serde::{Deserialize, Deserializer, Serialize};

use crate::state::{AppState, JsonMap};

/// `POST /api/v1/auth/login` — native WebUI session probe.
pub async fn auth_login(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let token = auth_form_token(&body);
    if !state.api_tokens.is_empty() {
        let Some(token) = token.as_deref() else {
            return (
                StatusCode::UNAUTHORIZED,
                Json(
                    serde_json::to_value(ApiError::new(
                        "UNAUTHORIZED",
                        "missing or invalid API token",
                    ))
                    .unwrap(),
                ),
            )
                .into_response();
        };
        if !token_allowed(&state, token) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(
                    serde_json::to_value(ApiError::new(
                        "UNAUTHORIZED",
                        "missing or invalid API token",
                    ))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    }
    let cookie = token
        .filter(|token| !token.is_empty())
        .map(|token| {
            format!(
                "tng_session={}; HttpOnly; SameSite=Lax; Path=/",
                cookie_component_encode(&token)
            )
        })
        .unwrap_or_else(|| "tng_session=; Max-Age=0; HttpOnly; SameSite=Lax; Path=/".to_owned());
    (
        StatusCode::OK,
        [(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap())],
        "Ok.",
    )
        .into_response()
}

/// `POST /api/v1/auth/logout` — native WebUI logout probe.
pub async fn auth_logout() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            HeaderValue::from_static("tng_session=; Max-Age=0; HttpOnly; SameSite=Lax; Path=/"),
        )],
    )
}

/// `GET /api/v1/torrents` — list all torrents.
pub async fn list_torrents(State(state): State<AppState>) -> impl IntoResponse {
    if let Some(engine) = &state.engine {
        let torrent_count = state.registry.read().await.iter().count();
        let estimate = estimate_torrent_summary_snapshot_bytes(torrent_count);
        match engine
            .reserve_memory(MemoryClass::ApiSnapshot, estimate)
            .await
        {
            Ok(Some(_lease)) => {
                let reg = state.registry.read().await;
                let summaries: Vec<TorrentSummary> = reg.iter().map(torrent_summary).collect();
                return (StatusCode::OK, Json(summaries)).into_response();
            }
            Ok(None) => return api_snapshot_budget_exhausted(),
            Err(e) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
                )
                    .into_response();
            }
        }
    }

    let reg = state.registry.read().await;
    let summaries: Vec<TorrentSummary> = reg.iter().map(torrent_summary).collect();
    (StatusCode::OK, Json(summaries)).into_response()
}

fn api_snapshot_budget_exhausted() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(
            serde_json::to_value(ApiError::internal(
                "api snapshot memory budget exhausted".to_owned(),
            ))
            .unwrap(),
        ),
    )
        .into_response()
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
    let summary = {
        let reg = state.registry.read().await;
        match reg.get(&info_hash) {
            Some(e) => torrent_summary(e),
            None => return not_found(info_hash),
        }
    };
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(serde_json::to_value(summary).unwrap())).into_response();
    };

    let base_estimate = estimate_torrent_detail_base_snapshot_bytes();
    let _base_lease = match engine
        .reserve_memory(MemoryClass::ApiSnapshot, base_estimate)
        .await
    {
        Ok(Some(lease)) => lease,
        Ok(None) => return api_snapshot_budget_exhausted(),
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
            )
                .into_response();
        }
    };

    match engine.torrent_metadata(info_hash.clone()).await {
        Ok(meta) => {
            let extra_estimate = estimate_torrent_detail_snapshot_bytes(&summary, &meta)
                .saturating_sub(base_estimate);
            let _extra_lease = if extra_estimate > 0 {
                match engine
                    .reserve_memory(MemoryClass::ApiSnapshot, extra_estimate)
                    .await
                {
                    Ok(Some(lease)) => Some(lease),
                    Ok(None) => return api_snapshot_budget_exhausted(),
                    Err(e) => {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
                        )
                            .into_response();
                    }
                }
            } else {
                None
            };
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
            (StatusCode::OK, Json(serde_json::to_value(detail).unwrap())).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateTorrentRequest {
    pub name: Option<String>,
    pub save_path: Option<String>,
}

/// `PUT /api/v1/torrents/{hash}` — update mutable torrent metadata.
pub async fn update_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Json(req): Json<UpdateTorrentRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }

    let name = normalize_optional_text(req.name);
    let save_path = req
        .save_path
        .map(|save_path| save_path.trim().to_owned())
        .filter(|save_path| !save_path.is_empty())
        .map(PathBuf::from);
    if name.is_none() && save_path.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ApiError::bad_request(
                    "name or save_path is required".to_owned(),
                ))
                .unwrap(),
            ),
        )
            .into_response();
    }

    if let Some(engine) = &state.engine {
        return match engine
            .update_torrent_fields(info_hash.clone(), name, save_path)
            .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response(),
        };
    }

    let mut reg = state.registry.write().await;
    match reg.get_mut(&info_hash) {
        Some(entry) => {
            if let Some(name) = name {
                entry.name = name;
            }
            if let Some(save_path) = save_path {
                entry.save_path = save_path.to_string_lossy().to_string();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        None => not_found(info_hash),
    }
}

#[derive(Debug, Deserialize)]
pub struct DeleteTorrentQuery {
    #[serde(default)]
    pub delete_files: bool,
}

/// `DELETE /api/v1/torrents/{hash}` — remove a torrent.
pub async fn delete_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Query(query): Query<DeleteTorrentQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    if let Some(engine) = &state.engine {
        match engine
            .remove_torrent(info_hash.clone(), query.delete_files)
            .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => not_found(info_hash),
        }
    } else {
        let mut reg = state.registry.write().await;
        let _ = reg.remove(&info_hash);
        StatusCode::NO_CONTENT.into_response()
    }
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Deserialize)]
pub struct SetCategoryRequest {
    pub category: Option<String>,
}

/// `PUT /api/v1/torrents/{hash}/category` — update the persisted torrent category.
pub async fn set_torrent_category(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Json(req): Json<SetCategoryRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    let category = req
        .category
        .map(|category| category.trim().to_owned())
        .filter(|category| !category.is_empty());
    if let Some(engine) = &state.engine {
        return match engine
            .update_torrent_labels(info_hash.clone(), Some(category), Vec::new(), Vec::new())
            .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response(),
        };
    }

    let mut reg = state.registry.write().await;
    match reg.get_mut(&info_hash) {
        Some(entry) => {
            entry.category = category;
            StatusCode::NO_CONTENT.into_response()
        }
        None => not_found(info_hash),
    }
}

#[derive(Debug, Serialize)]
pub struct TorrentLimitsResponse {
    pub download_limit: Option<i64>,
    pub upload_limit: Option<i64>,
    pub max_connections: Option<i64>,
    pub seed_ratio_limit: Option<f64>,
    pub seed_idle_limit: Option<i64>,
    pub sequential_download: bool,
    pub sequential_download_from_piece: Option<i64>,
    pub first_last_piece_prio: bool,
    pub force_start: bool,
    pub super_seeding: bool,
    pub auto_tmm: bool,
    pub auto_management: bool,
}

impl From<EngineTorrentLimits> for TorrentLimitsResponse {
    fn from(limits: EngineTorrentLimits) -> Self {
        Self {
            download_limit: limits.download_limit,
            upload_limit: limits.upload_limit,
            max_connections: limits.max_connections,
            seed_ratio_limit: limits.seed_ratio_limit,
            seed_idle_limit: limits.seed_idle_limit,
            sequential_download: limits.sequential_download,
            sequential_download_from_piece: limits.sequential_download_from_piece,
            first_last_piece_prio: limits.first_last_piece_prio,
            force_start: limits.force_start,
            super_seeding: limits.super_seeding,
            auto_tmm: limits.auto_tmm,
            auto_management: limits.auto_management,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateTorrentLimitsRequest {
    #[serde(default, deserialize_with = "deserialize_present_value")]
    pub download_limit: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    pub upload_limit: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    pub max_connections: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    pub seed_ratio_limit: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    pub seed_idle_limit: Option<serde_json::Value>,
    pub sequential_download: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    pub sequential_download_from_piece: Option<serde_json::Value>,
    pub first_last_piece_prio: Option<bool>,
    pub force_start: Option<bool>,
    pub super_seeding: Option<bool>,
    pub auto_tmm: Option<bool>,
    pub auto_management: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AddTorrentPeersRequest {
    #[serde(default)]
    pub peers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct QueueOrderRequest {
    pub hashes: Vec<String>,
    #[serde(rename = "move")]
    pub queue_move: String,
}

fn deserialize_present_value<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

#[derive(Debug, Serialize)]
pub struct TransferLimitsResponse {
    pub download_limit: i64,
    pub upload_limit: i64,
    pub speed_limits_mode: bool,
}

impl From<EngineGlobalLimits> for TransferLimitsResponse {
    fn from(limits: EngineGlobalLimits) -> Self {
        Self {
            download_limit: limits.download_limit,
            upload_limit: limits.upload_limit,
            speed_limits_mode: limits.speed_limits_mode,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateTransferLimitsRequest {
    pub download_limit: Option<i64>,
    pub upload_limit: Option<i64>,
    pub speed_limits_mode: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct NetworkFeaturesResponse {
    pub dht: bool,
    pub pex: bool,
}

impl From<EngineNetworkFeatures> for NetworkFeaturesResponse {
    fn from(features: EngineNetworkFeatures) -> Self {
        Self {
            dht: features.dht,
            pex: features.pex,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateNetworkFeaturesRequest {
    pub dht: Option<bool>,
    pub pex: Option<bool>,
}

/// `GET /api/v1/transfer/limits` — read global transfer limits.
pub async fn transfer_limits(State(state): State<AppState>) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal("native engine is not available")).unwrap(),
            ),
        )
            .into_response();
    };
    match engine.global_limits().await {
        Ok(limits) => (StatusCode::OK, Json(TransferLimitsResponse::from(limits))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `GET /api/v1/transfer/info` — qBit-compatible aggregate transfer counters.
pub async fn transfer_info(State(state): State<AppState>) -> impl IntoResponse {
    let hashes = {
        let reg = state.registry.read().await;
        reg.iter()
            .map(|entry| entry.info_hash.clone())
            .collect::<Vec<_>>()
    };
    let (dl_info_speed, up_info_speed) = native_session_peer_rates(&state, &hashes).await;
    let reg = state.registry.read().await;
    let mut dl_info_data = 0i64;
    let mut up_info_data = 0i64;
    for entry in reg.iter() {
        dl_info_data = dl_info_data.saturating_add(entry.stats.downloaded as i64);
        up_info_data = up_info_data.saturating_add(entry.stats.uploaded as i64);
    }
    drop(reg);

    let (dl_rate_limit, up_rate_limit) = if let Some(engine) = &state.engine {
        match engine.global_limits().await {
            Ok(limits) => (
                if limits.download_limit > 0 {
                    limits.download_limit
                } else {
                    -1
                },
                if limits.upload_limit > 0 {
                    limits.upload_limit
                } else {
                    -1
                },
            ),
            Err(_) => (-1, -1),
        }
    } else {
        (-1, -1)
    };

    Json(TransferInfoResponse {
        dl_info_speed,
        up_info_speed,
        dl_info_data,
        up_info_data,
        dl_rate_limit,
        up_rate_limit,
        dht_nodes: 0,
        connection_status: if state.engine.is_some() {
            "connected".to_owned()
        } else {
            "firewalled".to_owned()
        },
    })
    .into_response()
}

async fn native_session_peer_rates(state: &AppState, hashes: &[String]) -> (i64, i64) {
    let Some(engine) = &state.engine else {
        return (0, 0);
    };
    let mut download_rate = 0i64;
    let mut upload_rate = 0i64;
    for hash in hashes {
        if let Ok(peers) = engine.torrent_peers(hash.clone()).await {
            for peer in peers {
                download_rate = download_rate.saturating_add(peer.download_rate);
                upload_rate = upload_rate.saturating_add(peer.upload_rate);
            }
        }
    }
    (download_rate, upload_rate)
}

/// `PUT /api/v1/transfer/limits` — merge global transfer limits.
pub async fn update_transfer_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateTransferLimitsRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal("native engine is not available")).unwrap(),
            ),
        )
            .into_response();
    };
    let mut limits = match engine.global_limits().await {
        Ok(limits) => limits,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response()
        }
    };
    if let Some(value) = req.download_limit {
        limits.download_limit = value.max(0);
    }
    if let Some(value) = req.upload_limit {
        limits.upload_limit = value.max(0);
    }
    if let Some(value) = req.speed_limits_mode {
        limits.speed_limits_mode = value;
    }
    match engine.update_global_limits(limits).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `GET /api/v1/session/features` — read runtime DHT/PEX feature switches.
pub async fn session_features(State(state): State<AppState>) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal("native engine is not available")).unwrap(),
            ),
        )
            .into_response();
    };
    match engine.network_features().await {
        Ok(features) => (
            StatusCode::OK,
            Json(NetworkFeaturesResponse::from(features)),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `PUT /api/v1/session/features` — merge runtime DHT/PEX feature switches.
pub async fn update_session_features(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateNetworkFeaturesRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal("native engine is not available")).unwrap(),
            ),
        )
            .into_response();
    };
    let mut features = match engine.network_features().await {
        Ok(features) => features,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response()
        }
    };
    if let Some(value) = req.dht {
        features.dht = value;
    }
    if let Some(value) = req.pex {
        features.pex = value;
    }
    match engine.update_network_features(features).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `GET /api/v1/torrents/{hash}/limits` — read persisted per-torrent limits.
pub async fn torrent_limits(
    State(state): State<AppState>,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal("native engine is not available")).unwrap(),
            ),
        )
            .into_response();
    };
    match engine.torrent_limits(info_hash).await {
        Ok(limits) => (StatusCode::OK, Json(TorrentLimitsResponse::from(limits))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `PUT /api/v1/torrents/{hash}/limits` — merge and persist per-torrent limits.
pub async fn update_torrent_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Json(req): Json<UpdateTorrentLimitsRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal("native engine is not available")).unwrap(),
            ),
        )
            .into_response();
    };

    let mut limits = match engine.torrent_limits(info_hash.clone()).await {
        Ok(limits) => limits,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response()
        }
    };
    if let Err(e) = merge_torrent_limits(&mut limits, req) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response();
    }

    match engine.update_torrent_limits(info_hash, limits).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `POST /api/v1/torrents/{hash}/peers` — add explicit peers to a torrent.
pub async fn add_torrent_peers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Json(req): Json<AddTorrentPeersRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    let peers = req
        .peers
        .iter()
        .filter_map(|peer| peer.trim().parse::<SocketAddr>().ok())
        .collect::<Vec<_>>();
    if peers.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("peers is required")).unwrap()),
        )
            .into_response();
    }
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal("native engine is not available")).unwrap(),
            ),
        )
            .into_response();
    };
    match engine.add_peers(info_hash, peers).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `POST /api/v1/torrents/queue` — move torrents in the persisted queue order.
pub async fn update_torrent_queue(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<QueueOrderRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let queue_move = match req.queue_move.trim().to_ascii_lowercase().as_str() {
        "up" => QueueMove::Up,
        "down" => QueueMove::Down,
        "top" => QueueMove::Top,
        "bottom" => QueueMove::Bottom,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request("invalid queue move")).unwrap()),
            )
                .into_response()
        }
    };
    let hashes = req
        .hashes
        .into_iter()
        .map(|hash| hash.trim().to_owned())
        .filter(|hash| !hash.is_empty())
        .collect::<Vec<_>>();
    if hashes.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("hashes is required")).unwrap()),
        )
            .into_response();
    }
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal("native engine is not available")).unwrap(),
            ),
        )
            .into_response();
    };
    match engine.update_queue_order(hashes, queue_move).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

fn merge_torrent_limits(
    limits: &mut EngineTorrentLimits,
    req: UpdateTorrentLimitsRequest,
) -> Result<(), String> {
    if let Some(value) = req.download_limit {
        limits.download_limit = nullable_i64(value, "download_limit")?;
    }
    if let Some(value) = req.upload_limit {
        limits.upload_limit = nullable_i64(value, "upload_limit")?;
    }
    if let Some(value) = req.max_connections {
        limits.max_connections = nullable_i64(value, "max_connections")?;
    }
    if let Some(value) = req.seed_ratio_limit {
        limits.seed_ratio_limit = nullable_f64(value, "seed_ratio_limit")?;
    }
    if let Some(value) = req.seed_idle_limit {
        limits.seed_idle_limit = nullable_i64(value, "seed_idle_limit")?;
    }
    if let Some(value) = req.sequential_download {
        limits.sequential_download = value;
    }
    if let Some(value) = req.sequential_download_from_piece {
        limits.sequential_download_from_piece =
            nullable_i64(value, "sequential_download_from_piece")?;
    }
    if let Some(value) = req.first_last_piece_prio {
        limits.first_last_piece_prio = value;
    }
    if let Some(value) = req.force_start {
        limits.force_start = value;
    }
    if let Some(value) = req.super_seeding {
        limits.super_seeding = value;
    }
    if let Some(value) = req.auto_tmm {
        limits.auto_tmm = value;
    }
    if let Some(value) = req.auto_management {
        limits.auto_management = value;
    }
    Ok(())
}

fn nullable_i64(value: serde_json::Value, field: &str) -> Result<Option<i64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_i64()
        .map(Some)
        .ok_or_else(|| format!("{field} must be an integer or null"))
}

fn nullable_f64(value: serde_json::Value, field: &str) -> Result<Option<f64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_f64()
        .map(Some)
        .ok_or_else(|| format!("{field} must be a number or null"))
}

#[derive(Debug, Deserialize)]
pub struct PatchTagsRequest {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

/// `PATCH /api/v1/torrents/{hash}/tags` — add or remove persisted torrent tags.
pub async fn patch_torrent_tags(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Json(req): Json<PatchTagsRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }

    let add_tags = normalize_tags(req.add);
    let remove_tags = normalize_tags(req.remove);
    if let Some(engine) = &state.engine {
        return match engine
            .update_torrent_labels(info_hash.clone(), None, add_tags, remove_tags)
            .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response(),
        };
    }

    let mut reg = state.registry.write().await;
    match reg.get_mut(&info_hash) {
        Some(entry) => {
            for tag in add_tags {
                if !entry.tags.contains(&tag) {
                    entry.tags.push(tag);
                }
            }
            if !remove_tags.is_empty() {
                entry.tags.retain(|tag| !remove_tags.contains(tag));
            }
            StatusCode::NO_CONTENT.into_response()
        }
        None => not_found(info_hash),
    }
}

/// `POST /api/v1/torrents/{hash}/tags` — add persisted torrent tags.
pub async fn add_torrent_tags(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Json(req): Json<TagsRequest>,
) -> impl IntoResponse {
    patch_torrent_tags(
        State(state),
        headers,
        Path(info_hash),
        Json(PatchTagsRequest {
            add: req.tags,
            remove: Vec::new(),
        }),
    )
    .await
    .into_response()
}

/// `DELETE /api/v1/torrents/{hash}/tags` — remove persisted torrent tags.
pub async fn remove_torrent_tags(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Json(req): Json<TagsRequest>,
) -> impl IntoResponse {
    patch_torrent_tags(
        State(state),
        headers,
        Path(info_hash),
        Json(PatchTagsRequest {
            add: Vec::new(),
            remove: req.tags,
        }),
    )
    .await
    .into_response()
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for tag in tags {
        let tag = tag.trim().to_owned();
        if !tag.is_empty() && !normalized.contains(&tag) {
            normalized.push(tag);
        }
    }
    normalized
}

/// `GET /api/v1/torrents/{hash}/files` — list files for one torrent.
pub async fn list_torrent_files(
    State(state): State<AppState>,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(serde_json::json!([]))).into_response();
    };
    match engine.torrent_metadata(info_hash).await {
        Ok(meta) => {
            let files: Vec<FileInfo> = meta
                .files
                .into_iter()
                .map(|file| FileInfo {
                    file_index: file.index,
                    path: file.path,
                    length: file.length as i64,
                    priority: 1,
                })
                .collect();
            (StatusCode::OK, Json(serde_json::to_value(files).unwrap())).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct FilePriorityPatchItem {
    pub index: u32,
    pub priority: Option<i64>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PatchFilesRequest {
    #[serde(default)]
    pub files: Vec<FilePriorityPatchItem>,
}

/// `PATCH /api/v1/torrents/{hash}/files` — update file priorities and/or paths.
pub async fn patch_torrent_files(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Json(req): Json<PatchFilesRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if req.files.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("files must not be empty")).unwrap()),
        )
            .into_response();
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    let Some(engine) = &state.engine else {
        return StatusCode::NO_CONTENT.into_response();
    };

    let mut failures = Vec::new();
    for item in req.files {
        let mut changed = false;
        if let Some(priority) = item.priority {
            changed = true;
            if let Err(e) = engine
                .update_file_priorities(info_hash.clone(), vec![item.index], priority)
                .await
            {
                failures.push(format!("{} priority: {e}", item.index));
            }
        }
        if let Some(path) = item
            .path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            changed = true;
            if let Err(e) = engine
                .rename_file_path(info_hash.clone(), item.index, path.to_owned())
                .await
            {
                failures.push(format!("{} path: {e}", item.index));
            }
        }
        if !changed {
            failures.push(format!("{}: priority or path is required", item.index));
        }
    }
    if failures.is_empty() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ApiError::bad_request(format!(
                    "failed to update file priorities: {}",
                    failures.join("; ")
                )))
                .unwrap(),
            ),
        )
            .into_response()
    }
}

/// `GET /api/v1/torrents/{hash}/trackers` — list tracker announce URLs for one torrent.
pub async fn list_torrent_trackers(
    State(state): State<AppState>,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(serde_json::json!([]))).into_response();
    };
    match engine.torrent_metadata(info_hash).await {
        Ok(meta) => (
            StatusCode::OK,
            Json(serde_json::to_value(meta.trackers).unwrap()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct TrackerEditPatchItem {
    pub orig_url: String,
    pub new_url: String,
}

#[derive(Debug, Deserialize)]
pub struct PatchTrackersRequest {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default)]
    pub edit: Vec<TrackerEditPatchItem>,
}

/// `PATCH /api/v1/torrents/{hash}/trackers` — replace tracker URLs after applying a patch.
pub async fn patch_torrent_trackers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
    Json(req): Json<PatchTrackersRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    let Some(engine) = &state.engine else {
        return StatusCode::NO_CONTENT.into_response();
    };

    let mut trackers = match engine.torrent_metadata(info_hash.clone()).await {
        Ok(meta) => meta.trackers,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response()
        }
    };
    for edit in req.edit {
        let orig_url = edit.orig_url.trim();
        let new_url = edit.new_url.trim();
        if orig_url.is_empty() || new_url.is_empty() {
            continue;
        }
        for tracker in &mut trackers {
            if tracker == orig_url {
                *tracker = new_url.to_owned();
            }
        }
    }
    for remove in req.remove {
        let remove = remove.trim();
        if !remove.is_empty() {
            trackers.retain(|tracker| tracker != remove);
        }
    }
    for add in req.add {
        let add = add.trim();
        if !add.is_empty() && !trackers.iter().any(|tracker| tracker == add) {
            trackers.push(add.to_owned());
        }
    }

    match engine.update_torrent_trackers(info_hash, trackers).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoragePlanRequest {
    operation: String,
    source: Option<PathBuf>,
    destination: Option<PathBuf>,
    target: Option<PathBuf>,
    bytes: Option<u64>,
    available_bytes: Option<u64>,
    hardlink_or_copy: Option<bool>,
    dry_run: Option<bool>,
    dry_run_approved: Option<bool>,
    affected_torrents: Option<Vec<String>>,
    roots: Option<Vec<PathBuf>>,
    completed_steps: Option<Vec<usize>>,
}

#[derive(Debug, Serialize)]
pub struct StoragePlanResponse {
    operation: String,
    job_id: Option<String>,
    plan: StoragePlanView,
}

#[derive(Debug, Serialize)]
pub struct StoragePlanView {
    dry_run: bool,
    can_apply: bool,
    issues: Vec<String>,
    steps: Vec<StoragePlanStepView>,
    rollback_steps: Vec<StoragePlanStepView>,
}

#[derive(Debug, Serialize)]
pub struct StoragePlanStepView {
    action: String,
    source: Option<String>,
    destination: Option<String>,
    bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct JobsResponse {
    jobs: Vec<JobView>,
}

#[derive(Debug, Serialize)]
pub struct JobView {
    job_id: String,
    kind: String,
    state: String,
    dry_run: bool,
    affected_torrents: Vec<String>,
    total: i64,
    done: i64,
    checkpoint: i64,
    byte_offset: Option<i64>,
    verified_bytes: i64,
    error: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    updated_at: i64,
    finished_at: Option<i64>,
    progress: f64,
}

#[derive(Debug, Serialize)]
pub struct StorageRootsResponse {
    roots: Vec<StorageRootView>,
}

#[derive(Debug, Serialize)]
pub struct StorageRootView {
    id: String,
    path: PathBuf,
    profile: String,
    total_bytes: u64,
    available_bytes: u64,
    used_bytes: u64,
    readonly: bool,
    ok: bool,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CategoryRequest {
    name: String,
    save_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CategoryView {
    name: String,
    save_path: String,
    torrent_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct TagRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
pub struct BulkRequest {
    hashes: Vec<String>,
    dry_run: Option<bool>,
    category: Option<String>,
    save_path: Option<PathBuf>,
    tags: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct BulkResponse {
    applied: Vec<String>,
    errors: Vec<String>,
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub struct CrossSeedRequest {
    hashes: Vec<String>,
    trackers: Vec<String>,
    reannounce: Option<bool>,
    dry_run: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UserAgentRequest {
    user_agent: String,
}

#[derive(Debug, Deserialize)]
pub struct TagsRequest {
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TransferInfoResponse {
    dl_info_speed: i64,
    up_info_speed: i64,
    dl_info_data: i64,
    up_info_data: i64,
    dl_rate_limit: i64,
    up_rate_limit: i64,
    dht_nodes: i64,
    connection_status: String,
}

#[derive(Debug, Deserialize)]
pub struct DryRunRequest {
    dry_run: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RssSampleRequest {
    title: String,
    link: Option<String>,
    dry_run: Option<bool>,
}

pub trait JsonStore: Send + Sync + 'static {
    fn store(state: &AppState) -> Arc<tokio::sync::RwLock<JsonMap>>;
}

macro_rules! json_store {
    ($name:ident, $field:ident) => {
        pub struct $name;

        impl JsonStore for $name {
            fn store(state: &AppState) -> Arc<tokio::sync::RwLock<JsonMap>> {
                state.$field.clone()
            }
        }
    };
}

json_store!(SavedViewsStore, saved_views);
json_store!(RatioGroupsStore, ratio_groups);
json_store!(WorkflowsStore, workflows);
json_store!(RssRulesStore, rss_rules);

fn json_item_id(value: &serde_json::Value) -> Option<String> {
    ["id", "name"].into_iter().find_map(|field| {
        value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    })
}

pub async fn list_json_map<S: JsonStore>(State(state): State<AppState>) -> impl IntoResponse {
    let items = S::store(&state)
        .read()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    Json(items)
}

pub async fn upsert_json_map<S: JsonStore>(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(value): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let Some(id) = json_item_id(&value) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ApiError::bad_request(
                    "item id or name must not be empty".to_owned(),
                ))
                .unwrap(),
            ),
        )
            .into_response();
    };
    let mut value = value;
    if value.get("id").is_some()
        && value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        if let Some(object) = value.as_object_mut() {
            object.insert("id".to_owned(), serde_json::json!(slug_id(&id)));
        }
    }
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or(id);
    S::store(&state).write().await.insert(id, value);
    list_json_map::<S>(State(state)).await.into_response()
}

pub async fn delete_saved_json<S: JsonStore>(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    S::store(&state).write().await.remove(&id);
    list_json_map::<S>(State(state)).await.into_response()
}

pub async fn run_json_workflow<S: JsonStore>(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<DryRunRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let dry_run = req.dry_run.unwrap_or(false);
    let value = S::store(&state).read().await.get(&id).cloned();
    let Some(value) = value else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::to_value(ApiError::not_found(id)).unwrap()),
        )
            .into_response();
    };
    if !value
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("rule is disabled")).unwrap()),
        )
            .into_response();
    }
    let matched = matching_hashes_for_json_rule(&state, &value).await;
    let applied = if dry_run {
        matched.clone()
    } else {
        apply_json_rule_action(&state, &value, &matched).await
    };
    let run = serde_json::json!({
        "id": format!("run-{}", unix_now()),
        "rule_id": id,
        "rule_name": value.get("name").and_then(serde_json::Value::as_str).unwrap_or(""),
        "action": value.get("action").and_then(serde_json::Value::as_str).unwrap_or("set_category"),
        "kind": "native_json_workflow",
        "dry_run": dry_run,
        "matched": matched,
        "applied": applied,
        "errors": Vec::<String>::new(),
        "started_at": unix_now(),
    });
    state.workflow_runs.write().await.push(run.clone());
    Json(BulkResponse {
        applied: run["applied"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        errors: Vec::new(),
        dry_run,
    })
    .into_response()
}

pub async fn list_workflow_runs(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.workflow_runs.read().await.clone())
}

pub async fn test_rss_rules(
    State(state): State<AppState>,
    Json(req): Json<RssSampleRequest>,
) -> impl IntoResponse {
    let title = req.title.trim();
    if title.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("title must not be empty")).unwrap()),
        )
            .into_response();
    }
    let matches = rss_rule_matches(&state, &req).await;
    Json(serde_json::json!({ "dry_run": req.dry_run.unwrap_or(true), "matches": matches }))
        .into_response()
}

pub async fn apply_rss_rules(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RssSampleRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let dry_run = req.dry_run.unwrap_or(true);
    let matches = rss_rule_matches(&state, &req).await;
    let applied = matches
        .iter()
        .filter_map(|rule| {
            rule.get("rule_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    if !dry_run {
        state.workflow_runs.write().await.push(serde_json::json!({
            "id": format!("rss-{}", unix_now()),
            "kind": "rss_rules",
            "dry_run": false,
            "applied": applied,
            "started_at": unix_now(),
        }));
    }
    Json(BulkResponse {
        applied,
        errors: Vec::new(),
        dry_run,
    })
    .into_response()
}

impl From<EngineStorageRoot> for StorageRootView {
    fn from(root: EngineStorageRoot) -> Self {
        Self {
            id: root.id,
            path: root.path,
            profile: root.profile,
            total_bytes: root.total_bytes,
            available_bytes: root.available_bytes,
            used_bytes: root.used_bytes,
            readonly: false,
            ok: root.ok,
            error: root.error,
        }
    }
}

/// `GET /api/v1/storage` — list configured storage roots and live capacity.
pub async fn storage(State(state): State<AppState>) -> impl IntoResponse {
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
    match engine.list_storage_roots().await {
        Ok(roots) => (
            StatusCode::OK,
            Json(StorageRootsResponse {
                roots: roots.into_iter().map(StorageRootView::from).collect(),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `GET /api/v1/categories` — list known categories.
pub async fn categories(State(state): State<AppState>) -> impl IntoResponse {
    let reg = state.registry.read().await;
    let mut categories = state.categories.read().await.clone();
    for entry in reg.iter() {
        if let Some(category) = &entry.category {
            categories
                .entry(category.clone())
                .or_insert_with(|| entry.save_path.clone());
        }
    }
    let rows = categories
        .into_iter()
        .map(|(name, save_path)| {
            let torrent_count = reg
                .iter()
                .filter(|entry| entry.category.as_deref() == Some(name.as_str()))
                .count();
            CategoryView {
                name,
                save_path,
                torrent_count,
            }
        })
        .collect::<Vec<_>>();
    (StatusCode::OK, Json(rows)).into_response()
}

/// `POST /api/v1/categories` — create or update a category.
pub async fn upsert_category(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CategoryRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let name = req.name.trim();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("name is required")).unwrap()),
        )
            .into_response();
    }
    state
        .categories
        .write()
        .await
        .insert(name.to_owned(), req.save_path.unwrap_or_default());
    categories(State(state)).await.into_response()
}

/// `DELETE /api/v1/categories/{name}` — remove a category.
pub async fn delete_category(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    state.categories.write().await.remove(&name);
    let hashes = {
        let reg = state.registry.read().await;
        reg.iter()
            .filter(|entry| entry.category.as_deref() == Some(name.as_str()))
            .map(|entry| entry.info_hash.clone())
            .collect::<Vec<_>>()
    };
    {
        let mut reg = state.registry.write().await;
        for hash in hashes {
            let Some(entry) = reg.get_mut(&hash) else {
                continue;
            };
            if entry.category.as_deref() == Some(name.as_str()) {
                entry.category = None;
            }
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /api/v1/tags` — list known tag names.
pub async fn tags(State(state): State<AppState>) -> impl IntoResponse {
    let mut tags = BTreeSet::<String>::new();
    tags.extend(state.tags.read().await.iter().cloned());
    let reg = state.registry.read().await;
    for entry in reg.iter() {
        tags.extend(entry.tags.iter().filter(|tag| !tag.is_empty()).cloned());
    }
    (StatusCode::OK, Json(tags.into_iter().collect::<Vec<_>>())).into_response()
}

/// `POST /api/v1/tags` — create a global tag.
pub async fn create_tag(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TagRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let name = req.name.trim();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("name is required")).unwrap()),
        )
            .into_response();
    }
    let mut tags = state.tags.write().await;
    if !tags.iter().any(|tag| tag == name) {
        tags.push(name.to_owned());
        tags.sort();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `DELETE /api/v1/tags/{name}` — delete a global tag and remove it from registry entries.
pub async fn delete_tag(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    state.tags.write().await.retain(|tag| tag != &name);
    let hashes = {
        let reg = state.registry.read().await;
        reg.iter()
            .filter(|entry| entry.tags.contains(&name))
            .map(|entry| entry.info_hash.clone())
            .collect::<Vec<_>>()
    };
    let mut reg = state.registry.write().await;
    for hash in hashes {
        if let Some(entry) = reg.get_mut(&hash) {
            entry.tags.retain(|tag| tag != &name);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/v1/bulk/{action}` — apply a native bulk operation.
pub async fn bulk_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(action): Path<String>,
    Json(req): Json<BulkRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let dry_run = req.dry_run.unwrap_or(false);
    if !matches!(
        action.as_str(),
        "start" | "stop" | "recheck" | "reannounce" | "set-category" | "set-location" | "set-tags"
    ) {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ApiError::bad_request(format!(
                    "unsupported bulk action {action}"
                )))
                .unwrap(),
            ),
        )
            .into_response();
    }
    if action == "set-category"
        && req
            .category
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("category is required")).unwrap()),
        )
            .into_response();
    }
    if action == "set-location"
        && req
            .save_path
            .as_ref()
            .map(|path| path.display().to_string().trim().is_empty())
            .unwrap_or(true)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("save_path is required")).unwrap()),
        )
            .into_response();
    }
    if action == "set-tags" && req.tags.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request("tags is required")).unwrap()),
        )
            .into_response();
    }
    let hashes = if dry_run {
        preview_hashes(&state, &req.hashes).await
    } else {
        resolve_hashes(&state, &req.hashes).await
    };
    let mut errors = Vec::new();
    if hashes.is_empty() {
        errors.push("hashes is required".to_owned());
    }
    if dry_run {
        return (
            StatusCode::OK,
            Json(BulkResponse {
                applied: hashes,
                errors,
                dry_run,
            }),
        )
            .into_response();
    }
    for hash in &hashes {
        let result = run_bulk_action(&state, hash, &action, &req).await;
        if let Err(error) = result {
            errors.push(format!("{hash}: {error}"));
        }
    }
    (
        StatusCode::OK,
        Json(BulkResponse {
            applied: hashes,
            errors,
            dry_run,
        }),
    )
        .into_response()
}

/// `POST /api/v1/cross-seed` — preview or record cross-seed tracker updates.
pub async fn cross_seed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CrossSeedRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let dry_run = req.dry_run.unwrap_or(true);
    let hashes = resolve_hashes(&state, &req.hashes).await;
    let mut errors = Vec::new();
    if hashes.is_empty() {
        errors.push("hashes is required".to_owned());
    }
    if req.trackers.is_empty() {
        errors.push("trackers is required".to_owned());
    }
    if !dry_run && errors.is_empty() {
        for hash in &hashes {
            if let Some(engine) = &state.engine {
                if let Err(e) = engine
                    .update_torrent_trackers(hash.clone(), req.trackers.clone())
                    .await
                {
                    errors.push(format!("{hash}: {e}"));
                }
                if req.reannounce.unwrap_or(true) {
                    let _ = engine.reannounce_torrent(hash.clone()).await;
                }
            }
        }
    }
    (
        StatusCode::OK,
        Json(BulkResponse {
            applied: hashes,
            errors,
            dry_run,
        }),
    )
        .into_response()
}

/// `GET /api/v1/tracker-health` — aggregate tracker health from cached torrents.
pub async fn tracker_health(State(state): State<AppState>) -> impl IntoResponse {
    let reg = state.registry.read().await;
    let mut rows = BTreeMap::<String, serde_json::Value>::new();
    for entry in reg.iter() {
        if let Some(tracker) = entry
            .category
            .as_deref()
            .filter(|value| value.contains("://"))
        {
            let row = rows.entry(tracker.to_owned()).or_insert_with(|| {
                serde_json::json!({
                    "tracker": tracker,
                    "torrent_count": 0usize,
                    "active_count": 0usize,
                    "complete_count": 0usize,
                    "error_count": 0usize,
                    "seed_count": 0usize,
                    "peer_count": 0usize,
                    "last_updated": entry.added_at,
                })
            });
            increment_json_usize(row, "torrent_count");
            if matches!(
                entry.state,
                TorrentState::Downloading | TorrentState::Seeding
            ) {
                increment_json_usize(row, "active_count");
            }
            if entry.amount_left == 0 {
                increment_json_usize(row, "complete_count");
                increment_json_usize(row, "seed_count");
            }
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({ "trackers": rows.into_values().collect::<Vec<_>>() })),
    )
        .into_response()
}

/// `GET /api/v1/sidebar-facets` — aggregate sidebar filter counts.
pub async fn sidebar_facets(State(state): State<AppState>) -> impl IntoResponse {
    let reg = state.registry.read().await;
    let mut status = BTreeMap::<String, usize>::new();
    let mut media_type = BTreeMap::<String, usize>::new();
    for entry in reg.iter() {
        *status
            .entry(format!("{:?}", entry.state).to_ascii_lowercase())
            .or_default() += 1;
        *media_type
            .entry(infer_media_type(&entry.name).to_owned())
            .or_default() += 1;
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": status, "media_type": media_type })),
    )
        .into_response()
}

/// `GET /api/v1/logs` — project durable session events as operator logs.
pub async fn logs(
    State(state): State<AppState>,
    Query(query): Query<SessionEventsQuery>,
) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "logs": Vec::<serde_json::Value>::new() })),
        )
            .into_response();
    };
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let levels = query.level.iter().cloned().collect::<Vec<_>>();
    match engine
        .session_events_filtered(
            query.torrent.clone(),
            query.kind.clone(),
            levels,
            query.last_known_id,
            limit,
        )
        .await
    {
        Ok(events) => {
            let logs = events
                .into_iter()
                .filter_map(|row| {
                    let payload = serde_json::from_str::<serde_json::Value>(&row.payload).ok()?;
                    let level = payload
                        .get("level")
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| level_from_kind(&row.kind).to_owned());
                    Some(serde_json::json!({
                        "event_id": row.event_id.unwrap_or_default(),
                        "occurred_at": row.occurred_at,
                        "level": level,
                        "kind": row.kind,
                        "message": row.message.unwrap_or_default(),
                        "payload": payload.to_string(),
                    }))
                })
                .collect::<Vec<_>>();
            (StatusCode::OK, Json(serde_json::json!({ "logs": logs }))).into_response()
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
        )
            .into_response(),
    }
}

pub async fn get_user_agent(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({ "user_agent": state.user_agent.read().await.clone() })),
    )
        .into_response()
}

pub async fn set_user_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UserAgentRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    *state.user_agent.write().await = req.user_agent;
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /api/v1/engine` — native engine diagnostics for the WebUI.
pub async fn engine_diagnostics(State(state): State<AppState>) -> impl IntoResponse {
    let user_agent = state.user_agent.read().await.clone();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "backend": {
                "type": "native",
                "name": "TorrentNG Native",
                "version": env!("CARGO_PKG_VERSION"),
                "connected": state.engine.is_some(),
                "capabilities": native_webui_backend_capabilities(),
            },
            "provenance": {
                "daemon_version": env!("CARGO_PKG_VERSION"),
                "sidecar_version": env!("CARGO_PKG_VERSION"),
                "rtorrent_version": null,
                "libtorrent_version": null,
                "xmlrpc_backend": "native",
                "packaged_rtorrent_version": null,
                "packaged_libtorrent_version": null,
                "patch_set": ["torrentng-native"],
            },
            "capabilities": native_capability_rows(),
            "http": {
                "user_agent": probe_value(Ok(serde_json::json!(user_agent))),
                "current_open": probe_value(Ok(serde_json::json!(0))),
                "max_total_connections": probe_value(Ok(serde_json::json!(0))),
                "max_host_connections": probe_value(Ok(serde_json::json!(0))),
                "max_cache_connections": probe_value(Ok(serde_json::json!(0))),
                "dns_cache_timeout": probe_value(Ok(serde_json::json!(60))),
                "proxy_address": probe_value(Ok(serde_json::json!(""))),
                "ca_path": probe_value(Ok(serde_json::json!(""))),
                "ca_cert": probe_value(Ok(serde_json::json!(""))),
                "ssl_verify_peer": probe_value(Ok(serde_json::json!(true))),
                "ssl_verify_host": probe_value(Ok(serde_json::json!(true))),
            },
            "dht": {
                "enabled": probe_value(Ok(serde_json::json!("auto"))),
                "port": probe_value(Ok(serde_json::json!(0))),
                "override_port": probe_value(Ok(serde_json::json!(0))),
                "listen_port": probe_value(Ok(serde_json::json!(0))),
                "listen_range": probe_value(Ok(serde_json::json!("0-0"))),
                "pex": probe_value(Ok(serde_json::json!(true))),
                "udp_trackers": probe_value(Ok(serde_json::json!(true))),
                "statistics": probe_value(Ok(serde_json::json!("native"))),
            },
            "drift": Vec::<serde_json::Value>::new(),
        })),
    )
        .into_response()
}

/// `GET /api/v1/engine/commands` — rTorrent-style command surface projection.
pub async fn engine_commands(State(_state): State<AppState>) -> impl IntoResponse {
    let commands = vec![
        "torrent.start",
        "torrent.stop",
        "torrent.recheck",
        "torrent.reannounce",
        "torrent.set_category",
        "torrent.set_location",
        "storage.plan",
        "storage.execute",
    ];
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "count": commands.len(),
            "commands": commands,
            "error": null,
        })),
    )
        .into_response()
}

/// `GET /api/v1/engine/rtorrent-settings` — native compatibility settings.
pub async fn rtorrent_settings(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "settings": {
                "system.client_version": env!("CARGO_PKG_VERSION"),
                "system.user_agent": state.user_agent.read().await.clone(),
                "session.name": "TorrentNG",
                "network.http.dns_cache_timeout": 60,
            },
            "error": null,
        })),
    )
        .into_response()
}

/// `PUT /api/v1/engine/rtorrent-settings` — accept compatible settings overlays.
pub async fn save_rtorrent_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut req): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if let Some(user_agent) = req
        .get("settings")
        .and_then(|settings| settings.get("system.user_agent"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            req.get("system.user_agent")
                .and_then(serde_json::Value::as_str)
        })
    {
        *state.user_agent.write().await = user_agent.to_owned();
    }
    if !req.is_object() {
        req = serde_json::json!({ "value": req });
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "applied": req, "error": null })),
    )
        .into_response()
}

/// `POST /api/v1/engine/restart` — acknowledge native restart requests.
pub async fn restart_engine(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "ok": true,
            "restart_required": false,
            "message": "native engine restart is supervised by torrentngd",
        })),
    )
        .into_response()
}

/// `POST /api/v1/storage/plan` — preview a move/import/delete storage plan.
pub async fn storage_preview_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StoragePlanRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    match build_storage_plan(&req, true) {
        Ok(plan) => {
            if let Some(response) = validate_storage_plan_roots(&plan, req.roots.as_deref()) {
                return response;
            }
            if let Err(e) = validate_completed_steps(&plan, req.completed_steps.as_deref()) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                Json(storage_plan_response(&req.operation, &plan, None)),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `POST /api/v1/storage/execute` — execute a storage plan through durable engine jobs.
pub async fn storage_execute_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StoragePlanRequest>,
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
    let roots = match req.roots.clone() {
        Some(roots) if !roots.is_empty() => roots,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ApiError::bad_request(
                        "roots must include at least one configured storage root for execution"
                            .to_owned(),
                    ))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    };
    let plan = match build_storage_plan(&req, false) {
        Ok(plan) => plan,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response();
        }
    };
    if let Some(response) = validate_storage_plan_roots(&plan, Some(&roots)) {
        return response;
    }
    if let Err(e) = validate_completed_steps(&plan, req.completed_steps.as_deref()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response();
    }
    match engine
        .execute_storage_plan(
            normalize_storage_operation(&req.operation)
                .unwrap_or_else(|| req.operation.to_ascii_lowercase()),
            req.affected_torrents.unwrap_or_default(),
            plan.clone(),
            roots,
            req.completed_steps.unwrap_or_default(),
        )
        .await
    {
        Ok(job_id) => (
            StatusCode::ACCEPTED,
            Json(storage_plan_response(&req.operation, &plan, Some(job_id))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `GET /api/v1/jobs` — list active durable engine jobs.
pub async fn list_jobs(State(state): State<AppState>) -> impl IntoResponse {
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
    match engine.list_jobs().await {
        Ok(jobs) => (
            StatusCode::OK,
            Json(
                serde_json::to_value(JobsResponse {
                    jobs: jobs.into_iter().map(JobView::from).collect(),
                })
                .unwrap(),
            ),
        )
            .into_response(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
        )
            .into_response(),
    }
}

fn build_storage_plan(req: &StoragePlanRequest, preview: bool) -> Result<StoragePlan, String> {
    let dry_run = if preview {
        true
    } else {
        req.dry_run.unwrap_or(false)
    };
    match normalize_storage_operation(&req.operation).as_deref() {
        Some("move") => Ok(rt_storage::plan_move(&MovePlanRequest {
            source: req
                .source
                .clone()
                .ok_or_else(|| "source is required for move".to_owned())?,
            destination: req
                .destination
                .clone()
                .ok_or_else(|| "destination is required for move".to_owned())?,
            bytes: req.bytes.unwrap_or(0),
            available_bytes: req.available_bytes,
            dry_run,
        })),
        Some("import") => Ok(rt_storage::plan_import(&ImportPlanRequest {
            source: req
                .source
                .clone()
                .ok_or_else(|| "source is required for import".to_owned())?,
            destination: req
                .destination
                .clone()
                .ok_or_else(|| "destination is required for import".to_owned())?,
            bytes: req.bytes.unwrap_or(0),
            available_bytes: req.available_bytes,
            hardlink_or_copy: req.hardlink_or_copy.unwrap_or(false),
            dry_run,
        })),
        Some("delete") => Ok(rt_storage::plan_delete(&DeletePlanRequest {
            target: req
                .target
                .clone()
                .ok_or_else(|| "target is required for delete".to_owned())?,
            bytes: req.bytes.unwrap_or(0),
            dry_run,
            dry_run_approved: req.dry_run_approved.unwrap_or(!dry_run),
        })),
        _ => Err("operation must be one of move, import, or delete".to_owned()),
    }
}

fn validate_completed_steps(
    plan: &StoragePlan,
    completed_steps: Option<&[usize]>,
) -> Result<(), String> {
    let Some(completed_steps) = completed_steps else {
        return Ok(());
    };
    if let Some(index) = completed_steps
        .iter()
        .copied()
        .find(|index| *index >= plan.steps.len())
    {
        return Err(format!(
            "completed storage-plan step {index} is outside plan length {}",
            plan.steps.len()
        ));
    }
    Ok(())
}

fn validate_storage_plan_roots(
    plan: &StoragePlan,
    roots: Option<&[PathBuf]>,
) -> Option<axum::response::Response> {
    let roots = roots?;
    if roots.is_empty() {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ApiError::bad_request(
                        "roots must not be empty".to_owned(),
                    ))
                    .unwrap(),
                ),
            )
                .into_response(),
        );
    }
    let mut dry_run_plan = plan.clone();
    dry_run_plan.dry_run = true;
    match rt_storage::execute_storage_plan_under_roots(&dry_run_plan, roots) {
        Ok(_) => None,
        Err(e) => Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e.to_string())).unwrap()),
            )
                .into_response(),
        ),
    }
}

fn storage_plan_response(
    operation: &str,
    plan: &StoragePlan,
    job_id: Option<String>,
) -> StoragePlanResponse {
    StoragePlanResponse {
        operation: normalize_storage_operation(operation)
            .unwrap_or_else(|| operation.to_ascii_lowercase()),
        job_id,
        plan: StoragePlanView::from_plan(plan),
    }
}

impl StoragePlanView {
    fn from_plan(plan: &StoragePlan) -> Self {
        Self {
            dry_run: plan.dry_run,
            can_apply: plan.can_apply,
            issues: plan.issues.iter().map(storage_plan_issue_label).collect(),
            steps: plan
                .steps
                .iter()
                .map(StoragePlanStepView::from_step)
                .collect(),
            rollback_steps: plan
                .rollback_steps
                .iter()
                .map(StoragePlanStepView::from_step)
                .collect(),
        }
    }
}

impl StoragePlanStepView {
    fn from_step(step: &StoragePlanStep) -> Self {
        Self {
            action: storage_plan_step_action_label(&step.action).to_owned(),
            source: step.source.as_ref().map(|path| path.display().to_string()),
            destination: step
                .destination
                .as_ref()
                .map(|path| path.display().to_string()),
            bytes: step.bytes,
        }
    }
}

impl From<EngineJob> for JobView {
    fn from(job: EngineJob) -> Self {
        let progress = if job.total > 0 {
            (job.done as f64 / job.total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            job_id: job.job_id,
            kind: job.kind,
            state: job.state,
            dry_run: job.dry_run,
            affected_torrents: job.affected_torrents,
            total: job.total,
            done: job.done,
            checkpoint: job.checkpoint,
            byte_offset: job.byte_offset,
            verified_bytes: job.verified_bytes,
            error: job.error,
            created_at: job.created_at,
            started_at: job.started_at,
            updated_at: job.updated_at,
            finished_at: job.finished_at,
            progress,
        }
    }
}

fn normalize_storage_operation(operation: &str) -> Option<String> {
    match operation.trim().to_ascii_lowercase().as_str() {
        "move" => Some("move".to_owned()),
        "import" => Some("import".to_owned()),
        "delete" => Some("delete".to_owned()),
        _ => None,
    }
}

fn storage_plan_step_action_label(action: &PlannedStorageAction) -> &'static str {
    match action {
        PlannedStorageAction::ImportExisting => "import_existing",
        PlannedStorageAction::Rename => "rename",
        PlannedStorageAction::CopyVerifyRename => "copy_verify_rename",
        PlannedStorageAction::SafeDelete => "safe_delete",
    }
}

fn storage_plan_issue_label(issue: &PlanIssue) -> String {
    match issue {
        PlanIssue::SourceMissing(path) => format!("source missing: {}", path.display()),
        PlanIssue::DestinationExists(path) => format!("destination exists: {}", path.display()),
        PlanIssue::InsufficientCapacity { needed, available } => {
            format!("insufficient capacity: needed {needed}, available {available}")
        }
        PlanIssue::DeleteRequiresDryRunApproval => {
            "delete requires execute confirmation".to_owned()
        }
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
            "engine": {
                "mode": if ready { "native" } else { "unavailable" },
                "source_of_truth": if ready { "sqlite_session_db" } else { "registry_only" },
                "track1_sidecar_required": false,
                "capabilities": native_engine_capabilities(),
            },
        })),
    )
}

fn native_engine_capabilities() -> serde_json::Value {
    let utp_outgoing_policy = std::env::var("TNG_UTP_OUTGOING").ok().unwrap_or_else(|| {
        if std::env::var_os("TNG_ENABLE_UTP_OUTGOING").is_some() {
            "prefer".to_owned()
        } else {
            "auto".to_owned()
        }
    });
    let utp_outgoing_enabled = utp_policy_allows_peer_wire(&utp_outgoing_policy);
    let utp_metadata_policy = std::env::var("TNG_UTP_METADATA")
        .ok()
        .or_else(|| std::env::var("TNG_UTP_OUTGOING").ok())
        .unwrap_or_else(|| {
            if std::env::var_os("TNG_ENABLE_UTP_OUTGOING").is_some() {
                "prefer".to_owned()
            } else {
                "off".to_owned()
            }
        });
    let utp_metadata_enabled = utp_policy_allows_peer_wire(&utp_metadata_policy);
    let utp_incoming_enabled = std::env::var("TNG_UTP_INCOMING")
        .ok()
        .map(|value| utp_incoming_env_enabled(&value))
        .unwrap_or(false);
    let utp_transport = utp_outgoing_enabled || utp_metadata_enabled || utp_incoming_enabled;
    let mut utp_transport_paths = Vec::new();
    if utp_outgoing_enabled {
        utp_transport_paths.push("outgoing_peer_wire");
    }
    if utp_metadata_enabled {
        utp_transport_paths.push("metadata_fetch");
    }
    if utp_incoming_enabled {
        utp_transport_paths.push("incoming_peer_wire");
    }

    serde_json::json!({
        "torrent_identity": {
            "v1": true,
            "v2": true,
            "hybrid": true,
            "hash_lengths": [40, 64],
            "magnet_xt": ["btih", "btmh"],
        },
        "metadata": {
            "torrent_files": true,
            "magnets": true,
            "pure_v2_metadata_placeholders": true,
            "pure_v2_metadata_completion": true,
        },
        "session": {
            "durable_torrents": true,
            "durable_files": true,
            "durable_trackers": true,
            "durable_limits": true,
            "durable_labels": true,
            "event_log": true,
            "crash_restore": true,
        },
        "jobs": {
            "durable_recheck": true,
            "pause_resume_cancel": true,
            "crash_recovery": true,
            "storage_throttled": true,
        },
        "storage": {
            "root_registry": true,
            "mount_identity": true,
            "dry_run_import": true,
            "safe_move": true,
            "safe_delete_after_dry_run": true,
            "v2_file_root_verify": true,
        },
        "networking": {
            "tcp_peer_wire": true,
            "http_trackers": true,
            "udp_trackers": true,
            "dht": true,
            "utp_packet_codec": true,
            "utp_udp_stream": true,
            "utp_outgoing_opt_in": true,
            "utp_incoming_opt_in": true,
            "utp_outgoing_policy": utp_outgoing_policy,
            "utp_outgoing_enabled": utp_outgoing_enabled,
            "utp_metadata_policy": utp_metadata_policy,
            "utp_metadata_enabled": utp_metadata_enabled,
            "utp_incoming_enabled": utp_incoming_enabled,
            "utp_transport": utp_transport,
            "utp_transport_paths": utp_transport_paths,
            "private_torrent_dht_pex_lsd_default_off": true,
        },
        "compatibility": {
            "native_rest": true,
            "native_sse": true,
            "qbittorrent_v2": true,
            "transmission_rpc": true,
            "deluge_rpc": true,
        },
        "migration": {
            "rtorrent": true,
            "qbittorrent": true,
            "transmission": true,
            "dry_run_reports": true,
            "atomic_db_import": true,
        },
        "operations": {
            "prometheus_metrics": true,
            "diagnostics": true,
            "bounded_shutdown": true,
            "api_token_auth": true,
            "scale_certification": true,
        },
    })
}

fn native_webui_backend_capabilities() -> serde_json::Value {
    serde_json::json!({
        "supports_tags": true,
        "supports_categories": true,
        "supports_file_priority": true,
        "supports_tracker_edit": true,
        "supports_recheck": true,
        "supports_torrent_export": false,
        "supports_webseed_reads": false,
        "supports_piece_state_reads": true,
        "supports_piece_hash_reads": true,
        "supports_peer_snapshots": true,
        "supports_peer_add": true,
        "supports_peer_ban": false,
        "supports_queue_order": true,
        "supports_per_torrent_limits": true,
        "supports_global_limits": true,
        "supports_share_limits": true,
        "supports_mode_flags": true,
        "supports_location_update": true,
        "supports_torrent_rename": true,
        "supports_file_rename": false,
        "supports_runtime_user_agent": true,
        "supports_config_overlay": false,
        "supports_restart": false,
    })
}

fn native_capability_rows() -> Vec<serde_json::Value> {
    [
        ("Native torrent lifecycle", "implemented"),
        ("Storage roots and move planning", "implemented"),
        ("Categories and tags", "implemented"),
        ("Tracker edit and reannounce", "implemented"),
        ("Workflow/RSS compatibility stores", "implemented"),
        ("uTP transport policy", "implemented"),
        ("Peer snapshots", "implemented"),
        ("rTorrent process restart", "not_applicable"),
    ]
    .into_iter()
    .map(|(name, status)| {
        serde_json::json!({
            "name": name,
            "status": status,
            "detail": null,
        })
    })
    .collect()
}

fn utp_policy_allows_peer_wire(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off" | "tcp" | "tcp-only"
    )
}

fn utp_incoming_env_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
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

/// `GET /api/v1/events` — server-sent torrent delta stream.
pub async fn stream_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = futures::stream::unfold(
        EventStreamState::new(state),
        |mut stream_state| async move {
            loop {
                stream_state.tick.tick().await;
                let delta = torrent_delta(&stream_state.state, &stream_state.previous).await;
                if delta.torrents.is_empty() && delta.removed.is_empty() {
                    continue;
                }

                stream_state.seq = stream_state.seq.saturating_add(1);
                stream_state.previous = delta.current;
                let payload = serde_json::json!({
                    "seq": stream_state.seq,
                    "torrents": delta.torrents,
                    "removed": delta.removed,
                });
                let event = Event::default()
                    .event("torrent_delta")
                    .id(stream_state.seq.to_string())
                    .json_data(payload)
                    .expect("torrent delta serializes");
                return Some((Ok(event), stream_state));
            }
        },
    );

    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Debug, Deserialize)]
pub struct SessionEventsQuery {
    limit: Option<usize>,
    torrent: Option<String>,
    kind: Option<String>,
    level: Option<String>,
    last_known_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SessionEventResponse {
    id: i64,
    timestamp: i64,
    torrent: Option<String>,
    kind: String,
    level: String,
    message: Option<String>,
    payload: serde_json::Value,
}

/// `GET /api/v1/session-events` — recent durable engine events.
pub async fn list_session_events(
    State(state): State<AppState>,
    Query(query): Query<SessionEventsQuery>,
) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Vec::<SessionEventResponse>::new()),
        );
    };
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let levels = query.level.iter().cloned().collect::<Vec<_>>();
    match engine
        .session_events_filtered(
            query.torrent.clone(),
            query.kind.clone(),
            levels,
            query.last_known_id,
            limit,
        )
        .await
    {
        Ok(events) => {
            let events = events
                .into_iter()
                .filter_map(session_event_response)
                .collect::<Vec<_>>();
            (StatusCode::OK, Json(events))
        }
        Err(e) => {
            tracing::warn!(component = "api", operation = "session_events", result = "error", error = %e, "failed to list session events");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Vec::<SessionEventResponse>::new()),
            )
        }
    }
}

fn session_event_response(row: rt_db::SessionEventRow) -> Option<SessionEventResponse> {
    let payload = serde_json::from_str::<serde_json::Value>(&row.payload)
        .unwrap_or_else(|_| serde_json::json!({}));
    let level = payload
        .get("level")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| level_from_kind(&row.kind).to_owned());
    Some(SessionEventResponse {
        id: row.event_id.unwrap_or_default(),
        timestamp: row.occurred_at,
        torrent: row.info_hash,
        kind: row.kind,
        level,
        message: row.message,
        payload,
    })
}

fn level_from_kind(kind: &str) -> &'static str {
    let lower = kind.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("failed") {
        "error"
    } else if lower.contains("warn") {
        "warn"
    } else {
        "info"
    }
}

struct EventStreamState {
    state: AppState,
    previous: BTreeMap<String, String>,
    seq: u64,
    tick: tokio::time::Interval,
}

impl EventStreamState {
    fn new(state: AppState) -> Self {
        EventStreamState {
            state,
            previous: BTreeMap::new(),
            seq: 0,
            tick: tokio::time::interval(Duration::from_secs(1)),
        }
    }
}

struct TorrentDelta {
    current: BTreeMap<String, String>,
    torrents: Vec<TorrentSummary>,
    removed: Vec<String>,
}

async fn torrent_delta(state: &AppState, previous: &BTreeMap<String, String>) -> TorrentDelta {
    let _lease = if let Some(engine) = &state.engine {
        let torrent_count = state.registry.read().await.iter().count();
        match engine
            .reserve_memory(
                MemoryClass::ApiSnapshot,
                estimate_torrent_delta_snapshot_bytes(torrent_count),
            )
            .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return TorrentDelta {
                    current: BTreeMap::new(),
                    torrents: Vec::new(),
                    removed: Vec::new(),
                };
            }
        }
    } else {
        None
    };

    let reg = state.registry.read().await;
    let mut current = BTreeMap::new();
    let mut torrents = Vec::new();
    for entry in reg.iter() {
        let summary = torrent_summary(entry);
        let encoded = serde_json::to_string(&summary).expect("torrent summary serializes");
        if previous.get(&summary.info_hash) != Some(&encoded) {
            torrents.push(summary);
        }
        current.insert(entry.info_hash.clone(), encoded);
    }
    let removed = previous
        .keys()
        .filter(|hash| !current.contains_key(*hash))
        .cloned()
        .collect();

    TorrentDelta {
        current,
        torrents,
        removed,
    }
}

fn estimate_torrent_summary_snapshot_bytes(torrent_count: usize) -> u64 {
    // Conservative enough to cover Vec growth and cloned strings for typical
    // summaries without letting a huge API snapshot bypass governor pressure.
    (torrent_count as u64).saturating_mul(1024)
}

fn estimate_torrent_delta_snapshot_bytes(torrent_count: usize) -> u64 {
    (torrent_count as u64).saturating_mul(1536)
}

fn estimate_torrent_detail_base_snapshot_bytes() -> u64 {
    64 * 1024
}

fn estimate_torrent_detail_snapshot_bytes(
    summary: &TorrentSummary,
    meta: &rt_engine::EngineTorrentMetadata,
) -> u64 {
    let summary_strings = summary
        .info_hash
        .len()
        .saturating_add(summary.name.len())
        .saturating_add(summary.state.len())
        .saturating_add(summary.save_path.len())
        .saturating_add(summary.category.as_ref().map_or(0, String::len))
        .saturating_add(summary.tags.iter().map(String::len).sum::<usize>());
    let tracker_strings = meta.trackers.iter().map(String::len).sum::<usize>();
    let webseed_strings = meta.webseeds.iter().map(String::len).sum::<usize>();
    let file_strings = meta.files.iter().map(|file| file.path.len()).sum::<usize>();
    let structured = 2048usize
        .saturating_add(meta.trackers.len().saturating_mul(256))
        .saturating_add(meta.webseeds.len().saturating_mul(256))
        .saturating_add(meta.files.len().saturating_mul(512))
        .saturating_add(meta.piece_hashes.len().saturating_mul(64))
        .saturating_add(meta.piece_states.len().saturating_mul(8));
    structured
        .saturating_add(summary_strings)
        .saturating_add(tracker_strings)
        .saturating_add(webseed_strings)
        .saturating_add(file_strings)
        .max(estimate_torrent_detail_base_snapshot_bytes() as usize) as u64
}

fn torrent_summary(e: &TorrentEntry) -> TorrentSummary {
    TorrentSummary {
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
    }
}

fn render_metrics(stats: &rt_engine::EngineStats) -> String {
    let mut out = String::new();
    metric(
        &mut out,
        "torrentng_torrents_total",
        "gauge",
        "Total torrents in session",
        stats.torrents_total,
    );
    metric(
        &mut out,
        "torrentng_torrents_seeding",
        "gauge",
        "Currently seeding torrents",
        stats.torrents_seeding,
    );
    metric(
        &mut out,
        "torrentng_torrents_downloading",
        "gauge",
        "Currently downloading torrents",
        stats.torrents_downloading,
    );
    metric(
        &mut out,
        "torrentng_torrents_paused",
        "gauge",
        "Paused or stopped torrents",
        stats.torrents_paused,
    );
    metric(
        &mut out,
        "torrentng_torrents_checking",
        "gauge",
        "Torrents checking pieces",
        stats.torrents_checking,
    );
    metric(
        &mut out,
        "torrentng_torrents_metadata_pending",
        "gauge",
        "Metadata-pending torrents",
        stats.torrents_metadata_pending,
    );
    metric(
        &mut out,
        "torrentng_torrents_queued",
        "gauge",
        "Queued torrents",
        stats.torrents_queued,
    );
    metric(
        &mut out,
        "torrentng_torrents_errored",
        "gauge",
        "Errored torrents",
        stats.torrents_error,
    );
    metric(
        &mut out,
        "torrentng_torrents_activity_hot",
        "gauge",
        "Torrents currently classified as hot by the activity tier policy",
        stats.torrents_activity_hot,
    );
    metric(
        &mut out,
        "torrentng_torrents_activity_warm",
        "gauge",
        "Torrents currently classified as warm by the activity tier policy",
        stats.torrents_activity_warm,
    );
    metric(
        &mut out,
        "torrentng_torrents_activity_dormant",
        "gauge",
        "Torrents currently classified as dormant by the activity tier policy",
        stats.torrents_activity_dormant,
    );
    metric(
        &mut out,
        "torrentng_torrent_tasks_active",
        "gauge",
        "Active per-torrent runtime tasks",
        stats.torrent_tasks_active,
    );
    metric(
        &mut out,
        "torrentng_fastresume_dirty_pieces",
        "gauge",
        "Pieces validated since the last completed durability barrier",
        stats.fastresume_dirty_pieces,
    );
    metric(
        &mut out,
        "torrentng_completed_piece_verify_from_memory_total",
        "counter",
        "Completed piece verifications hashed from assembled in-memory piece data",
        stats.completed_piece_verify_from_memory,
    );
    metric(
        &mut out,
        "torrentng_completed_piece_verify_from_disk_total",
        "counter",
        "Completed piece verifications that fell back to disk re-read",
        stats.completed_piece_verify_from_disk,
    );
    metric(
        &mut out,
        "torrentng_bytes_uploaded_total",
        "counter",
        "Uploaded bytes from session accounting",
        stats.bytes_uploaded,
    );
    metric(
        &mut out,
        "torrentng_bytes_downloaded_total",
        "counter",
        "Downloaded bytes from session accounting",
        stats.bytes_downloaded,
    );
    metric(
        &mut out,
        "torrentng_bytes_left",
        "gauge",
        "Bytes left across enabled torrent pieces",
        stats.bytes_left,
    );
    metric(
        &mut out,
        "torrentng_jobs_active",
        "gauge",
        "Active durable jobs",
        stats.jobs_active,
    );
    metric(
        &mut out,
        "torrentng_trackers_total",
        "gauge",
        "Persisted tracker rows",
        stats.trackers_total,
    );
    metric(
        &mut out,
        "torrentng_trackers_working",
        "gauge",
        "Trackers in working state",
        stats.trackers_working,
    );
    metric(
        &mut out,
        "torrentng_trackers_warning",
        "gauge",
        "Trackers with warning state",
        stats.trackers_warning,
    );
    metric(
        &mut out,
        "torrentng_trackers_error",
        "gauge",
        "Trackers with error state",
        stats.trackers_error,
    );
    metric(
        &mut out,
        "torrentng_dht_routing_nodes",
        "gauge",
        "DHT routing table nodes retained by the native DHT task",
        stats.dht_routing_nodes,
    );
    metric(
        &mut out,
        "torrentng_dht_announced_peer_sets",
        "gauge",
        "Info-hash peer sets retained from DHT announce_peer queries",
        stats.dht_announced_peer_sets,
    );
    metric(
        &mut out,
        "torrentng_dht_announced_peers",
        "gauge",
        "DHT announced peers retained across all info-hash peer sets",
        stats.dht_announced_peers,
    );
    metric(
        &mut out,
        "torrentng_dht_tracked_torrents",
        "gauge",
        "Torrents registered with the native DHT task",
        stats.dht_tracked_torrents,
    );
    metric(
        &mut out,
        "torrentng_dht_outstanding_requests",
        "gauge",
        "DHT KRPC requests currently awaiting responses",
        stats.dht_outstanding_requests,
    );
    metric(
        &mut out,
        "torrentng_dht_queried_nodes",
        "gauge",
        "DHT nodes remembered as queried across active lookups",
        stats.dht_queried_nodes,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_capacity",
        "gauge",
        "Configured open-file cache capacity across running torrent schedulers",
        stats.storage_file_pool_capacity,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_open_files",
        "gauge",
        "Open files across running torrent scheduler caches",
        stats.storage_file_pool_open_files,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_memory_bytes",
        "gauge",
        "Approximate memory used by open-file cache metadata across running torrent schedulers",
        stats.storage_file_pool_memory_bytes,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_hits_total",
        "counter",
        "Open-file cache hits across running torrent schedulers",
        stats.storage_file_pool_hits,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_misses_total",
        "counter",
        "Open-file cache misses across running torrent schedulers",
        stats.storage_file_pool_misses,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_evictions_total",
        "counter",
        "Open-file cache evictions across running torrent schedulers",
        stats.storage_file_pool_evictions,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_idle_closes_total",
        "counter",
        "Idle open-file cache closes across running torrent schedulers",
        stats.storage_file_pool_idle_closes,
    );
    metric(
        &mut out,
        "torrentng_storage_io_queue_depth",
        "gauge",
        "Queued disk I/O jobs across running torrent schedulers",
        stats.storage_io_queue_depth,
    );
    metric(
        &mut out,
        "torrentng_storage_hash_queue_depth",
        "gauge",
        "Queued hashing jobs across running torrent schedulers",
        stats.storage_hash_queue_depth,
    );
    metric(
        &mut out,
        "torrentng_storage_device_queue_capacity",
        "gauge",
        "Configured process-level device queue permits across running torrent schedulers",
        stats.storage_device_queue_capacity,
    );
    metric(
        &mut out,
        "torrentng_storage_device_queue_available",
        "gauge",
        "Currently available process-level device queue permits across running torrent schedulers",
        stats.storage_device_queue_available,
    );
    metric(
        &mut out,
        "torrentng_storage_queued_disk_bytes",
        "gauge",
        "Estimated process-owned bytes represented by queued disk and hash jobs",
        stats.storage_queued_disk_bytes,
    );
    metric(
        &mut out,
        "torrentng_storage_queue_full_total",
        "counter",
        "Disk or hash jobs denied because the per-mount storage queue was full",
        stats.storage_queue_full,
    );
    metric(
        &mut out,
        "torrentng_storage_dirty_files",
        "gauge",
        "Dirty files tracked by running torrent schedulers",
        stats.storage_dirty_files,
    );
    metric(
        &mut out,
        "torrentng_storage_read_ops_total",
        "counter",
        "Positioned read operations across running torrent schedulers",
        stats.storage_read_ops,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_read_ops_by_class_total",
        "counter",
        "Positioned read operations by I/O class across running torrent schedulers",
        &stats.storage_read_ops_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_write_ops_total",
        "counter",
        "Positioned write operations across running torrent schedulers",
        stats.storage_write_ops,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_write_ops_by_class_total",
        "counter",
        "Positioned write operations by I/O class across running torrent schedulers",
        &stats.storage_write_ops_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_bytes_read_total",
        "counter",
        "Bytes read through running torrent schedulers",
        stats.storage_bytes_read,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_bytes_read_by_class_total",
        "counter",
        "Bytes read by I/O class through running torrent schedulers",
        &stats.storage_bytes_read_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_bytes_written_total",
        "counter",
        "Bytes written through running torrent schedulers",
        stats.storage_bytes_written,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_bytes_written_by_class_total",
        "counter",
        "Bytes written by I/O class through running torrent schedulers",
        &stats.storage_bytes_written_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_read_ops_total",
        "counter",
        "Backend disk read operations across running torrent schedulers",
        stats.storage_backend_read_ops,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_backend_read_ops_by_class_total",
        "counter",
        "Backend disk read operations by I/O class across running torrent schedulers",
        &stats.storage_backend_read_ops_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_bytes_read_total",
        "counter",
        "Bytes read from backend disk operations across running torrent schedulers",
        stats.storage_backend_bytes_read,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_backend_bytes_read_by_class_total",
        "counter",
        "Bytes read from backend disk operations by I/O class across running torrent schedulers",
        &stats.storage_backend_bytes_read_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds_total",
        "counter",
        "Total read queue plus execution latency across running torrent schedulers",
        stats.storage_read_latency_ns,
    );
    latency_histogram(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds",
        "Read queue plus execution latency across running torrent schedulers",
        &stats.storage_read_latency_buckets,
        stats.storage_read_ops,
        stats.storage_read_latency_ns,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds_by_class_total",
        "counter",
        "Total read queue plus execution latency by I/O class",
        &stats.storage_read_latency_ns_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds_total",
        "counter",
        "Total write queue plus execution latency across running torrent schedulers",
        stats.storage_write_latency_ns,
    );
    latency_histogram(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds",
        "Write queue plus execution latency across running torrent schedulers",
        &stats.storage_write_latency_buckets,
        stats.storage_write_ops,
        stats.storage_write_latency_ns,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds_by_class_total",
        "counter",
        "Total write queue plus execution latency by I/O class",
        &stats.storage_write_latency_ns_by_class,
    );
    metric_by_device(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds_by_device_total",
        "counter",
        "Total read queue plus execution latency by storage device",
        &stats.storage_device_latencies,
        |device| device.read_latency_ns,
    );
    latency_histogram_by_device(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds_by_device",
        "Read queue plus execution latency histogram by storage device",
        &stats.storage_device_latencies,
        |device| &device.read_latency_buckets,
        |device| device.read_latency_ns,
    );
    metric_by_device(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds_by_device_total",
        "counter",
        "Total write queue plus execution latency by storage device",
        &stats.storage_device_latencies,
        |device| device.write_latency_ns,
    );
    latency_histogram_by_device(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds_by_device",
        "Write queue plus execution latency histogram by storage device",
        &stats.storage_device_latencies,
        |device| &device.write_latency_buckets,
        |device| device.write_latency_ns,
    );
    metric_by_device(
        &mut out,
        "torrentng_storage_sync_latency_nanoseconds_by_device_total",
        "counter",
        "Total sync queue plus execution latency by storage device",
        &stats.storage_device_latencies,
        |device| device.sync_latency_ns,
    );
    latency_histogram_by_device(
        &mut out,
        "torrentng_storage_sync_latency_nanoseconds_by_device",
        "Sync queue plus execution latency histogram by storage device",
        &stats.storage_device_latencies,
        |device| &device.sync_latency_buckets,
        |device| device.sync_latency_ns,
    );
    metric_by_device(
        &mut out,
        "torrentng_storage_hash_latency_nanoseconds_by_device_total",
        "counter",
        "Total hashing queue plus execution latency by storage device",
        &stats.storage_device_latencies,
        |device| device.hash_latency_ns,
    );
    latency_histogram_by_device(
        &mut out,
        "torrentng_storage_hash_latency_nanoseconds_by_device",
        "Hashing queue plus execution latency histogram by storage device",
        &stats.storage_device_latencies,
        |device| &device.hash_latency_buckets,
        |device| device.hash_latency_ns,
    );
    metric(
        &mut out,
        "torrentng_storage_sync_latency_nanoseconds_total",
        "counter",
        "Total sync queue plus execution latency across running torrent schedulers",
        stats.storage_sync_latency_ns,
    );
    latency_histogram(
        &mut out,
        "torrentng_storage_sync_latency_nanoseconds",
        "Sync queue plus execution latency across running torrent schedulers",
        &stats.storage_sync_latency_buckets,
        stats.storage_sync_ops,
        stats.storage_sync_latency_ns,
    );
    metric(
        &mut out,
        "torrentng_storage_hash_latency_nanoseconds_total",
        "counter",
        "Total hashing queue plus execution latency across running torrent schedulers",
        stats.storage_hash_latency_ns,
    );
    latency_histogram(
        &mut out,
        "torrentng_storage_hash_latency_nanoseconds",
        "Hashing queue plus execution latency across running torrent schedulers",
        &stats.storage_hash_latency_buckets,
        stats.storage_hash_ops,
        stats.storage_hash_latency_ns,
    );
    metric(
        &mut out,
        "torrentng_storage_sync_ops_total",
        "counter",
        "Data sync operations across running torrent schedulers",
        stats.storage_sync_ops,
    );
    metric(
        &mut out,
        "torrentng_storage_hash_ops_total",
        "counter",
        "Hashing operations across running torrent schedulers",
        stats.storage_hash_ops,
    );
    metric(
        &mut out,
        "torrentng_storage_preallocation_failures_total",
        "counter",
        "Preallocation failures across running torrent schedulers",
        stats.storage_preallocation_failures,
    );
    metric(
        &mut out,
        "torrentng_storage_preallocation_fallbacks_total",
        "counter",
        "Preallocation fallback events across running torrent schedulers",
        stats.storage_preallocation_fallbacks,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_cache_entries",
        "gauge",
        "Peer-read readahead cache entries across running torrent schedulers",
        stats.storage_peer_read_cache_entries,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_cache_hits_total",
        "counter",
        "Peer-read readahead cache hits across running torrent schedulers",
        stats.storage_peer_read_cache_hits,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_cache_misses_total",
        "counter",
        "Peer-read readahead cache misses across running torrent schedulers",
        stats.storage_peer_read_cache_misses,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_cache_evictions_total",
        "counter",
        "Peer-read readahead cache evictions across running torrent schedulers",
        stats.storage_peer_read_cache_evictions,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_enabled",
        "gauge",
        "Running torrent schedulers with peer-read elevator enabled",
        stats.storage_peer_read_elevator_enabled,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_queue_depth",
        "gauge",
        "Configured peer-read elevator queue depth across running torrent schedulers",
        stats.storage_peer_read_elevator_queue_depth,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_queued",
        "gauge",
        "Currently queued peer-read elevator requests across running torrent schedulers",
        stats.storage_peer_read_elevator_queued,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_queue_full_total",
        "counter",
        "Peer-read elevator requests denied because the elevator queue was full",
        stats.storage_peer_read_elevator_queue_full,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_batches_total",
        "counter",
        "Peer-read elevator backend batches across running torrent schedulers",
        stats.storage_peer_read_elevator_batches,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_coalesced_requests_total",
        "counter",
        "Peer-read elevator logical requests coalesced into existing backend batches",
        stats.storage_peer_read_elevator_coalesced_requests,
    );
    metric(
        &mut out,
        "torrentng_storage_page_cache_advise_sequential_total",
        "counter",
        "Successful POSIX_FADV_SEQUENTIAL hints issued by storage schedulers",
        stats.storage_page_cache_advise_sequential,
    );
    metric(
        &mut out,
        "torrentng_storage_page_cache_advise_willneed_total",
        "counter",
        "Successful POSIX_FADV_WILLNEED hints issued by storage schedulers",
        stats.storage_page_cache_advise_willneed,
    );
    metric(
        &mut out,
        "torrentng_storage_page_cache_advise_dontneed_total",
        "counter",
        "Successful POSIX_FADV_DONTNEED hints issued by storage schedulers",
        stats.storage_page_cache_advise_dontneed,
    );
    metric(
        &mut out,
        "torrentng_storage_page_cache_advise_failures_total",
        "counter",
        "Failed page-cache advice calls observed by storage schedulers",
        stats.storage_page_cache_advise_failures,
    );
    metric(
        &mut out,
        "torrentng_storage_sparse_data_extents_total",
        "counter",
        "Sparse-file data extents discovered during storage recheck",
        stats.storage_sparse_data_extents,
    );
    metric(
        &mut out,
        "torrentng_storage_sparse_hole_bytes_total",
        "counter",
        "Sparse-file hole bytes skipped during storage recheck",
        stats.storage_sparse_hole_bytes,
    );
    metric(
        &mut out,
        "torrentng_storage_sparse_seek_fallbacks_total",
        "counter",
        "Sparse-file extent probes that fell back to contiguous reads",
        stats.storage_sparse_seek_fallbacks,
    );
    metric(
        &mut out,
        "torrentng_piece_assembly_buffers",
        "gauge",
        "In-memory piece assembly buffers across running torrents",
        stats.piece_assembly_buffers,
    );
    metric(
        &mut out,
        "torrentng_piece_assembly_bytes",
        "gauge",
        "In-memory piece assembly bytes across running torrents",
        stats.piece_assembly_bytes,
    );
    metric(
        &mut out,
        "torrentng_piece_assembly_evictions_total",
        "counter",
        "In-memory piece assembly buffers evicted by torrent-task budgets",
        stats.piece_assembly_evictions,
    );
    metric(
        &mut out,
        "torrentng_peer_request_window_reductions_total",
        "counter",
        "Peer request refill windows reduced because memory pressure limited in-flight piece data",
        stats.peer_request_window_reductions,
    );
    metric(
        &mut out,
        "torrentng_peer_rx_buffer_bytes",
        "gauge",
        "Expected peer receive buffer bytes reserved by outstanding block requests",
        stats.peer_rx_buffer_bytes,
    );
    metric(
        &mut out,
        "torrentng_peer_tx_buffer_bytes",
        "gauge",
        "Peer upload buffer bytes currently owned by torrent tasks",
        stats.peer_tx_buffer_bytes,
    );
    metric(
        &mut out,
        "torrentng_peer_command_queue_depth",
        "gauge",
        "Queued peer commands across active peer tasks",
        stats.peer_command_queue_depth,
    );
    metric(
        &mut out,
        "torrentng_peer_command_queue_capacity",
        "gauge",
        "Total peer command queue capacity across active peer tasks",
        stats.peer_command_queue_capacity,
    );
    metric(
        &mut out,
        "torrentng_peer_command_queue_full_total",
        "counter",
        "Nonblocking peer command sends denied because peer command queues were full",
        stats.peer_command_queue_full,
    );
    metric(
        &mut out,
        "torrentng_tracker_peer_cache_entries",
        "gauge",
        "Tracker-discovered peer addresses retained across running torrents",
        stats.tracker_peer_cache_entries,
    );
    metric(
        &mut out,
        "torrentng_tracker_peer_cache_drops_total",
        "counter",
        "Tracker-discovered peer addresses dropped because bounded peer caches were full",
        stats.tracker_peer_cache_drops,
    );
    if let Some(resources) = &stats.resources {
        metric(
            &mut out,
            "torrentng_memory_cap_bytes",
            "gauge",
            "Configured process-owned memory cap enforced by the resource governor",
            resources.total_cap_bytes,
        );
        metric(
            &mut out,
            "torrentng_memory_used_bytes",
            "gauge",
            "Process-owned bytes currently leased through the resource governor",
            resources.total_used_bytes,
        );
        metric(
            &mut out,
            "torrentng_memory_pressure_state",
            "gauge",
            "Resource governor memory pressure state: normal=0 constrained=1 critical=2",
            match resources.pressure {
                rt_metrics::MemoryPressure::Normal => 0,
                rt_metrics::MemoryPressure::Constrained => 1,
                rt_metrics::MemoryPressure::Critical => 2,
            },
        );
        for class in resources.classes {
            metric_with_label(
                &mut out,
                "torrentng_memory_class_cap_bytes",
                "gauge",
                "Configured process-owned memory cap by resource class",
                "class",
                class.class.as_str(),
                class.cap_bytes,
            );
            metric_with_label(
                &mut out,
                "torrentng_memory_class_used_bytes",
                "gauge",
                "Process-owned bytes currently leased by resource class",
                "class",
                class.class.as_str(),
                class.used_bytes,
            );
            metric_with_label(
                &mut out,
                "torrentng_memory_class_denied_allocations_total",
                "counter",
                "Denied resource governor allocation attempts by resource class",
                "class",
                class.class.as_str(),
                class.denied_allocations,
            );
        }
    }
    for (rank, torrent) in stats.hot_torrent_memory_top.iter().enumerate() {
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_memory_estimated_bytes",
            "gauge",
            "Estimated process-owned memory attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.estimated_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_piece_assembly_bytes",
            "gauge",
            "Piece assembly bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.piece_assembly_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_peer_buffer_bytes",
            "gauge",
            "Peer rx/tx buffer bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.peer_buffer_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_tracker_peer_bytes",
            "gauge",
            "Tracker peer-cache bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.tracker_peer_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_peer_command_queue_bytes",
            "gauge",
            "Peer command queue bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.peer_command_queue_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_storage_cache_bytes",
            "gauge",
            "Per-torrent storage cache bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.storage_cache_bytes,
        );
    }
    let storage = StorageRuntime::global();
    metric_with_label(
        &mut out,
        "torrentng_storage_backend_selected",
        "gauge",
        "Selected global storage backend; value is 1 for the active backend",
        "backend",
        storage.backend_kind().as_str(),
        1,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_fixed_buffers_supported",
        "gauge",
        "Whether the selected storage backend supports fixed registered buffers",
        u64::from(storage.backend_supports_fixed_buffers()),
    );
    metric(
        &mut out,
        "torrentng_storage_backend_registered_files_supported",
        "gauge",
        "Whether the selected storage backend supports registered file slots",
        u64::from(storage.backend_supports_registered_files()),
    );
    metric(
        &mut out,
        "torrentng_storage_backend_max_batch_len",
        "gauge",
        "Maximum number of storage backend jobs submitted as one batch",
        storage.backend_max_batch_len() as u64,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_fixed_buffer_bytes",
        "gauge",
        "Bytes in each registered fixed buffer for the selected storage backend",
        storage.backend_fixed_buffer_len() as u64,
    );
    let fixed_buffer_strategy = storage.backend_fixed_buffer_strategy();
    metric_with_label(
        &mut out,
        "torrentng_storage_backend_fixed_buffer_strategy",
        "gauge",
        "Selected fixed-buffer strategy; value is 1 for the active strategy",
        "strategy",
        fixed_buffer_strategy.as_str(),
        1,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_fixed_buffer_worker_copy",
        "gauge",
        "Whether fixed-buffer submissions copy through backend-private worker buffers",
        u64::from(fixed_buffer_strategy.uses_worker_copy()),
    );
    metric(
        &mut out,
        "torrentng_storage_backend_frame_pool_slots_supported",
        "gauge",
        "Whether fixed-buffer submissions use registered storage frame-pool slots directly",
        u64::from(fixed_buffer_strategy.uses_frame_pool_slots()),
    );
    metric(
        &mut out,
        "torrentng_storage_handles_open",
        "gauge",
        "Open file handles in the global storage runtime cache",
        storage.handles_open() as u64,
    );
    metric(
        &mut out,
        "torrentng_storage_frame_bytes_in_use",
        "gauge",
        "Frame-pool bytes currently checked out by storage I/O",
        storage.frame_in_use_bytes(),
    );
    metric(
        &mut out,
        "torrentng_storage_frame_bytes_cap",
        "gauge",
        "Frame-pool byte cap for storage I/O",
        storage.frame_cap_bytes(),
    );
    let utp = rt_utp::stats_snapshot();
    metric(
        &mut out,
        "torrentng_utp_connects_total",
        "counter",
        "Outbound uTP streams that completed the SYN/STATE handshake",
        utp.connects,
    );
    metric(
        &mut out,
        "torrentng_utp_accepts_total",
        "counter",
        "Inbound uTP streams accepted by listeners or shared endpoints",
        utp.accepts,
    );
    metric(
        &mut out,
        "torrentng_utp_bytes_sent_total",
        "counter",
        "uTP UDP payload bytes sent including protocol headers",
        utp.bytes_sent,
    );
    metric(
        &mut out,
        "torrentng_utp_bytes_received_total",
        "counter",
        "uTP application payload bytes delivered to streams",
        utp.bytes_received,
    );
    metric(
        &mut out,
        "torrentng_utp_send_timeouts_total",
        "counter",
        "uTP send or close operations that timed out waiting for acknowledgement",
        utp.send_timeouts,
    );
    metric(
        &mut out,
        "torrentng_utp_recv_timeouts_total",
        "counter",
        "uTP receive operations that timed out waiting for packets",
        utp.recv_timeouts,
    );
    metric(
        &mut out,
        "torrentng_utp_retransmits_total",
        "counter",
        "uTP packet retransmission attempts",
        utp.retransmits,
    );
    metric(
        &mut out,
        "torrentng_utp_route_drops_total",
        "counter",
        "uTP datagrams dropped because no stream route or queue slot was available",
        utp.route_drops,
    );
    metric(
        &mut out,
        "torrentng_utp_rtt_samples_total",
        "counter",
        "uTP RTT samples observed from packet timestamp deltas",
        utp.rtt_samples,
    );
    metric(
        &mut out,
        "torrentng_utp_rtt_us",
        "gauge",
        "Last smoothed uTP RTT in microseconds",
        utp.rtt_us,
    );
    metric(
        &mut out,
        "torrentng_utp_rtt_min_us",
        "gauge",
        "Minimum observed smoothed uTP RTT in microseconds",
        utp.rtt_min_us,
    );
    metric(
        &mut out,
        "torrentng_utp_rtt_max_us",
        "gauge",
        "Maximum observed smoothed uTP RTT in microseconds",
        utp.rtt_max_us,
    );
    metric(
        &mut out,
        "torrentng_utp_rtt_var_us",
        "gauge",
        "Last smoothed uTP RTT variance in microseconds",
        utp.rtt_var_us,
    );
    metric(
        &mut out,
        "torrentng_utp_retransmit_timeout_us",
        "gauge",
        "Current uTP retransmission timeout in microseconds",
        utp.retransmit_timeout_us,
    );
    metric(
        &mut out,
        "torrentng_utp_congestion_window_bytes",
        "gauge",
        "Last observed uTP congestion window in bytes",
        utp.congestion_window_bytes,
    );
    metric(
        &mut out,
        "torrentng_utp_congestion_base_delay_us",
        "gauge",
        "Last observed uTP base delay in microseconds",
        utp.congestion_base_delay_us,
    );
    metric(
        &mut out,
        "torrentng_utp_congestion_current_delay_us",
        "gauge",
        "Last observed uTP current delay in microseconds",
        utp.congestion_current_delay_us,
    );
    metric(
        &mut out,
        "torrentng_utp_bytes_in_flight",
        "gauge",
        "Last observed uTP bytes in flight",
        utp.bytes_in_flight,
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

fn metric_by_class(out: &mut String, name: &str, kind: &str, help: &str, values: &[u64; 6]) {
    const IO_CLASSES: [&str; 6] = [
        "metadata",
        "recheck",
        "move_copy",
        "peer_write",
        "peer_read",
        "foreground",
    ];

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
    for (class, value) in IO_CLASSES.iter().zip(values) {
        out.push_str(name);
        out.push_str("{class=\"");
        out.push_str(class);
        out.push_str("\"} ");
        out.push_str(&value.to_string());
        out.push('\n');
    }
}

fn metric_with_label(
    out: &mut String,
    name: &str,
    kind: &str,
    help: &str,
    label: &str,
    label_value: &str,
    value: u64,
) {
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
    out.push('{');
    out.push_str(label);
    out.push_str("=\"");
    push_label_value(out, label_value);
    out.push_str("\"} ");
    out.push_str(&value.to_string());
    out.push('\n');
}

fn metric_with_two_labels(
    out: &mut String,
    name: &str,
    kind: &str,
    help: &str,
    first: (&str, &str),
    second: (&str, &str),
    value: u64,
) {
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
    out.push('{');
    out.push_str(first.0);
    out.push_str("=\"");
    push_label_value(out, first.1);
    out.push_str("\",");
    out.push_str(second.0);
    out.push_str("=\"");
    push_label_value(out, second.1);
    out.push_str("\"} ");
    out.push_str(&value.to_string());
    out.push('\n');
}

fn push_label_value(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
}

fn metric_by_device(
    out: &mut String,
    name: &str,
    kind: &str,
    help: &str,
    values: &[rt_engine::StorageDeviceLatencyStats],
    value: impl Fn(&rt_engine::StorageDeviceLatencyStats) -> u64,
) {
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
    for device in values {
        out.push_str(name);
        out.push_str("{device=\"");
        push_label_value(out, &device.device_id);
        out.push_str("\",profile=\"");
        push_label_value(out, &device.profile);
        out.push_str("\"} ");
        out.push_str(&value(device).to_string());
        out.push('\n');
    }
}

fn latency_histogram(
    out: &mut String,
    name: &str,
    help: &str,
    buckets: &[u64; STORAGE_LATENCY_BUCKETS_NS.len()],
    count: u64,
    sum_ns: u64,
) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" histogram\n");
    for (upper_bound, count) in STORAGE_LATENCY_BUCKETS_NS.iter().zip(buckets) {
        out.push_str(name);
        out.push_str("_bucket{le=\"");
        if *upper_bound == u64::MAX {
            out.push_str("+Inf");
        } else {
            out.push_str(&upper_bound.to_string());
        }
        out.push_str("\"} ");
        out.push_str(&count.to_string());
        out.push('\n');
    }
    out.push_str(name);
    out.push_str("_sum ");
    out.push_str(&sum_ns.to_string());
    out.push('\n');
    out.push_str(name);
    out.push_str("_count ");
    out.push_str(&count.to_string());
    out.push('\n');
}

fn latency_histogram_by_device(
    out: &mut String,
    name: &str,
    help: &str,
    values: &[rt_engine::StorageDeviceLatencyStats],
    buckets: impl Fn(&rt_engine::StorageDeviceLatencyStats) -> &[u64; STORAGE_LATENCY_BUCKETS_NS.len()],
    sum_ns: impl Fn(&rt_engine::StorageDeviceLatencyStats) -> u64,
) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" histogram\n");
    for device in values {
        let buckets = buckets(device);
        for (upper_bound, count) in STORAGE_LATENCY_BUCKETS_NS.iter().zip(buckets) {
            out.push_str(name);
            out.push_str("_bucket{device=\"");
            push_label_value(out, &device.device_id);
            out.push_str("\",profile=\"");
            push_label_value(out, &device.profile);
            out.push_str("\",le=\"");
            if *upper_bound == u64::MAX {
                out.push_str("+Inf");
            } else {
                out.push_str(&upper_bound.to_string());
            }
            out.push_str("\"} ");
            out.push_str(&count.to_string());
            out.push('\n');
        }
        out.push_str(name);
        out.push_str("_sum{device=\"");
        push_label_value(out, &device.device_id);
        out.push_str("\",profile=\"");
        push_label_value(out, &device.profile);
        out.push_str("\"} ");
        out.push_str(&sum_ns(device).to_string());
        out.push('\n');
        out.push_str(name);
        out.push_str("_count{device=\"");
        push_label_value(out, &device.device_id);
        out.push_str("\",profile=\"");
        push_label_value(out, &device.profile);
        out.push_str("\"} ");
        out.push_str(&buckets[STORAGE_LATENCY_BUCKETS_NS.len() - 1].to_string());
        out.push('\n');
    }
}

async fn resolve_hashes(state: &AppState, hashes: &[String]) -> Vec<String> {
    let reg = state.registry.read().await;
    if hashes.iter().any(|hash| hash == "all") {
        return reg.iter().map(|entry| entry.info_hash.clone()).collect();
    }
    hashes
        .iter()
        .map(|hash| hash.trim())
        .filter(|hash| !hash.is_empty())
        .filter(|hash| reg.get(hash).is_some())
        .map(ToOwned::to_owned)
        .collect()
}

async fn preview_hashes(state: &AppState, hashes: &[String]) -> Vec<String> {
    if hashes.iter().any(|hash| hash == "all") {
        return state
            .registry
            .read()
            .await
            .iter()
            .map(|entry| entry.info_hash.clone())
            .collect();
    }
    hashes
        .iter()
        .map(|hash| hash.trim())
        .filter(|hash| !hash.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

async fn matching_hashes_for_json_rule(state: &AppState, rule: &serde_json::Value) -> Vec<String> {
    let category = rule
        .get("category")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let tracker = rule
        .get("tracker")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let reg = state.registry.read().await;
    reg.iter()
        .filter(|entry| {
            category
                .map(|category| entry.category.as_deref() == Some(category))
                .unwrap_or(true)
        })
        .filter(|entry| {
            tracker
                .map(|tracker| {
                    entry
                        .category
                        .as_deref()
                        .map(|value| value.contains(tracker))
                        .unwrap_or(false)
                })
                .unwrap_or(true)
        })
        .map(|entry| entry.info_hash.clone())
        .collect()
}

async fn apply_json_rule_action(
    state: &AppState,
    rule: &serde_json::Value,
    hashes: &[String],
) -> Vec<String> {
    let action = rule
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("set_category");
    let mut applied = Vec::new();
    for hash in hashes {
        let result = match action {
            "set_category" => {
                let category = rule
                    .get("category")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                if let Some(engine) = &state.engine {
                    engine
                        .update_torrent_labels(hash.clone(), Some(category), Vec::new(), Vec::new())
                        .await
                } else {
                    set_registry_category(state, hash, category).await
                }
            }
            "set_location" => {
                let save_path = rule
                    .get("target_path")
                    .or_else(|| rule.get("save_path"))
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from);
                if let Some(engine) = &state.engine {
                    match save_path {
                        Some(path) => {
                            engine
                                .update_torrent_fields(hash.clone(), None, Some(path))
                                .await
                        }
                        None => Err("target_path is required".to_owned()),
                    }
                } else {
                    set_registry_location(state, hash, save_path).await
                }
            }
            "webhook" | "script" => Ok(()),
            _ => Ok(()),
        };
        if result.is_ok() {
            applied.push(hash.clone());
        }
    }
    applied
}

async fn run_bulk_action(
    state: &AppState,
    hash: &str,
    action: &str,
    req: &BulkRequest,
) -> Result<(), String> {
    if !torrent_exists(state, hash).await {
        return Err("torrent not found".to_owned());
    }
    if let Some(engine) = &state.engine {
        return match action {
            "start" => engine.resume_torrent(hash.to_owned()).await,
            "stop" => engine.pause_torrent(hash.to_owned()).await,
            "recheck" => engine.recheck_torrent(hash.to_owned()).await,
            "reannounce" => engine.reannounce_torrent(hash.to_owned()).await,
            "set-category" => {
                engine
                    .update_torrent_labels(
                        hash.to_owned(),
                        Some(req.category.clone()),
                        Vec::new(),
                        Vec::new(),
                    )
                    .await
            }
            "set-tags" => {
                let tags = normalize_tags(req.tags.clone().unwrap_or_default());
                let existing = state
                    .registry
                    .read()
                    .await
                    .get(hash)
                    .map(|entry| entry.tags.clone())
                    .unwrap_or_default();
                let remove_tags = existing
                    .into_iter()
                    .filter(|tag| !tags.contains(tag))
                    .collect::<Vec<_>>();
                engine
                    .update_torrent_labels(hash.to_owned(), None, tags, remove_tags)
                    .await
            }
            "set-location" => {
                let save_path = req
                    .save_path
                    .clone()
                    .ok_or_else(|| "save_path is required".to_owned())?;
                engine
                    .update_torrent_fields(hash.to_owned(), None, Some(save_path))
                    .await
            }
            _ => Err(format!("unsupported bulk action {action}")),
        };
    }
    match action {
        "start" => transition_registry_torrent(state, hash, TorrentState::Downloading).await,
        "stop" => transition_registry_torrent(state, hash, TorrentState::Paused).await,
        "recheck" => transition_registry_torrent(state, hash, TorrentState::Checking).await,
        "reannounce" => Ok(()),
        "set-category" => set_registry_category(state, hash, req.category.clone()).await,
        "set-location" => set_registry_location(state, hash, req.save_path.clone()).await,
        "set-tags" => set_registry_tags(state, hash, req.tags.clone().unwrap_or_default()).await,
        _ => Err(format!("unsupported bulk action {action}")),
    }
}

async fn transition_registry_torrent(
    state: &AppState,
    hash: &str,
    target: TorrentState,
) -> Result<(), String> {
    let mut reg = state.registry.write().await;
    let entry = reg
        .get_mut(hash)
        .ok_or_else(|| "torrent not found".to_owned())?;
    entry.transition(target).map_err(|e| e.to_string())
}

async fn set_registry_category(
    state: &AppState,
    hash: &str,
    category: Option<String>,
) -> Result<(), String> {
    let mut reg = state.registry.write().await;
    let entry = reg
        .get_mut(hash)
        .ok_or_else(|| "torrent not found".to_owned())?;
    entry.category = category;
    Ok(())
}

async fn set_registry_location(
    state: &AppState,
    hash: &str,
    save_path: Option<PathBuf>,
) -> Result<(), String> {
    let Some(save_path) = save_path else {
        return Err("save_path is required".to_owned());
    };
    let mut reg = state.registry.write().await;
    let entry = reg
        .get_mut(hash)
        .ok_or_else(|| "torrent not found".to_owned())?;
    entry.save_path = save_path.display().to_string();
    Ok(())
}

async fn set_registry_tags(state: &AppState, hash: &str, tags: Vec<String>) -> Result<(), String> {
    let mut reg = state.registry.write().await;
    let entry = reg
        .get_mut(hash)
        .ok_or_else(|| "torrent not found".to_owned())?;
    entry.tags = normalize_tags(tags);
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn increment_json_usize(value: &mut serde_json::Value, field: &str) {
    let current = value
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    if let Some(object) = value.as_object_mut() {
        object.insert(field.to_owned(), serde_json::json!(current + 1));
    }
}

fn infer_media_type(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if [".mkv", ".mp4", ".avi", ".mov", ".webm"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        "video"
    } else if [".flac", ".mp3", ".ogg", ".m4a", ".wav"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        "audio"
    } else if [".zip", ".rar", ".7z", ".tar", ".gz"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        "archive"
    } else {
        "other"
    }
}

async fn rss_rule_matches(state: &AppState, req: &RssSampleRequest) -> Vec<serde_json::Value> {
    let rules = state.rss_rules.read().await;
    let haystack =
        format!("{} {}", req.title, req.link.as_deref().unwrap_or_default()).to_ascii_lowercase();
    rules
        .values()
        .filter_map(|rule| {
            if !rule
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true)
            {
                return None;
            }
            let include_matches = rule
                .get("contains")
                .or_else(|| rule.get("pattern"))
                .or_else(|| rule.get("include"))
                .or_else(|| rule.get("mustContain"))
                .and_then(serde_json::Value::as_str)
                .map(|needle| {
                    needle
                        .split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .any(|part| haystack.contains(&part.to_ascii_lowercase()))
                })
                .unwrap_or(true);
            let exclude_matches = rule
                .get("exclude")
                .or_else(|| rule.get("mustNotContain"))
                .and_then(serde_json::Value::as_str)
                .map(|needle| {
                    needle
                        .split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .any(|part| haystack.contains(&part.to_ascii_lowercase()))
                })
                .unwrap_or(false);
            if !(include_matches && !exclude_matches) {
                return None;
            }
            let rule_id = rule
                .get("id")
                .and_then(serde_json::Value::as_str)
                .or_else(|| rule.get("name").and_then(serde_json::Value::as_str))
                .unwrap_or_default();
            let rule_name = rule
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(rule_id);
            Some(serde_json::json!({
                "rule_id": rule_id,
                "rule_name": rule_name,
                "matched": true,
                "reason": "include matched",
                "category": rule.get("category").cloned().unwrap_or(serde_json::Value::Null),
                "save_path": rule.get("save_path").cloned().unwrap_or(serde_json::Value::Null),
                "tags": rule.get("tags").cloned().unwrap_or_else(|| serde_json::json!([])),
                "start": rule.get("start").and_then(serde_json::Value::as_bool).unwrap_or(true),
            }))
        })
        .collect()
}

fn slug_id(value: &str) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        format!("item-{}", unix_now())
    } else {
        slug
    }
}

fn probe_value(result: Result<serde_json::Value, String>) -> serde_json::Value {
    match result {
        Ok(value) => serde_json::json!({ "ok": true, "value": value, "error": null }),
        Err(error) => serde_json::json!({ "ok": false, "value": null, "error": error }),
    }
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
        || presented_token(headers).is_some_and(|token| token_allowed(state, &token))
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

fn auth_form_token(body: &str) -> Option<String> {
    let mut username = None;
    let mut password = None;
    for pair in body.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match form_component_decode(key).as_deref() {
            Some("username") => username = form_component_decode(value),
            Some("password") => password = form_component_decode(value),
            _ => {}
        }
    }
    password.or(username)
}

fn form_component_decode(input: &str) -> Option<String> {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'+' => {
                out.push(b' ');
                idx += 1;
            }
            b'%' if idx + 2 < bytes.len() => {
                let hi = hex_value(bytes[idx + 1])?;
                let lo = hex_value(bytes[idx + 2])?;
                out.push((hi << 4) | lo);
                idx += 3;
            }
            b'%' => return None,
            byte => {
                out.push(byte);
                idx += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn cookie_component_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'0'..=b'9'
            | b'a'..=b'z'
            | b'A'..=b'Z'
            | b'!'
            | b'#'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'/'
            | b':'
            | b'<'
            | b'='
            | b'>'
            | b'?'
            | b'@'
            | b'['
            | b']'
            | b'^'
            | b'_'
            | b'`'
            | b'{'
            | b'|'
            | b'}'
            | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn presented_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(ToOwned::to_owned)
        .or_else(|| {
            headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(extract_session_cookie)
        })
}

fn extract_session_cookie(cookie: &str) -> Option<String> {
    cookie.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("tng_session=")
            .and_then(form_component_decode)
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

    #[test]
    fn session_event_response_projects_level_and_payload() {
        let event = rt_db::SessionEventRow {
            event_id: Some(12),
            occurred_at: 1_700_000_000,
            info_hash: Some("a".repeat(40)),
            kind: "tracker_warning".to_owned(),
            message: Some("tracker warning".to_owned()),
            payload: r#"{"tracker":"udp://tracker","level":"warn"}"#.to_owned(),
        };

        let projected = session_event_response(event).unwrap();
        assert_eq!(projected.id, 12);
        assert_eq!(projected.level, "warn");
        assert_eq!(projected.kind, "tracker_warning");
        assert_eq!(projected.payload["tracker"], "udp://tracker");
    }

    #[test]
    fn level_from_kind_is_conservative() {
        assert_eq!(level_from_kind("torrent_added"), "info");
        assert_eq!(level_from_kind("tracker_warning"), "warn");
        assert_eq!(level_from_kind("storage_failed"), "error");
    }

    #[test]
    fn storage_plan_preview_projects_move_steps() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let req = StoragePlanRequest {
            operation: "move".to_owned(),
            source: Some(source),
            destination: Some(destination),
            target: None,
            bytes: Some(7),
            available_bytes: Some(7),
            hardlink_or_copy: None,
            dry_run: Some(true),
            dry_run_approved: None,
            affected_torrents: None,
            roots: None,
            completed_steps: None,
        };

        let plan = build_storage_plan(&req, true).unwrap();
        let response = storage_plan_response(&req.operation, &plan, None);

        assert_eq!(response.operation, "move");
        assert!(response.plan.can_apply);
        assert!(response.plan.dry_run);
        assert!(!response.plan.steps.is_empty());
        assert_eq!(response.plan.steps[0].action, "rename");
    }

    #[test]
    fn storage_plan_preview_projects_staged_import_copy() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let req = StoragePlanRequest {
            operation: "import".to_owned(),
            source: Some(source),
            destination: Some(destination.clone()),
            target: None,
            bytes: Some(7),
            available_bytes: Some(7),
            hardlink_or_copy: Some(false),
            dry_run: Some(true),
            dry_run_approved: None,
            affected_torrents: None,
            roots: None,
            completed_steps: None,
        };

        let plan = build_storage_plan(&req, true).unwrap();
        let response = storage_plan_response(&req.operation, &plan, None);

        assert_eq!(response.operation, "import");
        assert!(response.plan.can_apply);
        assert_eq!(response.plan.steps.len(), 2);
        assert_eq!(response.plan.steps[0].action, "copy_verify_rename");
        let destination = destination.display().to_string();
        assert_ne!(
            response.plan.steps[0].destination.as_deref(),
            Some(destination.as_str())
        );
        assert_eq!(response.plan.steps[1].action, "rename");
        assert_eq!(
            response.plan.steps[1].destination.as_deref(),
            Some(destination.as_str())
        );
        assert_eq!(response.plan.rollback_steps.len(), 1);
        assert_eq!(response.plan.rollback_steps[0].action, "safe_delete");
    }

    #[test]
    fn storage_plan_completed_steps_are_bounded_by_plan() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let req = StoragePlanRequest {
            operation: "move".to_owned(),
            source: Some(source),
            destination: Some(destination),
            target: None,
            bytes: Some(7),
            available_bytes: Some(7),
            hardlink_or_copy: None,
            dry_run: Some(false),
            dry_run_approved: None,
            affected_torrents: None,
            roots: None,
            completed_steps: Some(vec![1]),
        };

        let plan = build_storage_plan(&req, false).unwrap();

        assert!(validate_completed_steps(&plan, req.completed_steps.as_deref()).is_err());
    }

    #[test]
    fn storage_plan_completed_steps_accept_sorted_unique_subset() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let req = StoragePlanRequest {
            operation: "import".to_owned(),
            source: Some(source),
            destination: Some(destination),
            target: None,
            bytes: Some(7),
            available_bytes: Some(7),
            hardlink_or_copy: Some(false),
            dry_run: Some(false),
            dry_run_approved: None,
            affected_torrents: None,
            roots: None,
            completed_steps: Some(vec![0, 1]),
        };

        let plan = build_storage_plan(&req, false).unwrap();

        assert!(validate_completed_steps(&plan, req.completed_steps.as_deref()).is_ok());
    }

    #[test]
    fn storage_plan_root_validation_rejects_escape() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("source.bin");
        let destination = allowed.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let req = StoragePlanRequest {
            operation: "move".to_owned(),
            source: Some(source),
            destination: Some(destination),
            target: None,
            bytes: Some(7),
            available_bytes: None,
            hardlink_or_copy: None,
            dry_run: Some(true),
            dry_run_approved: None,
            affected_torrents: None,
            roots: Some(vec![allowed.path().to_path_buf()]),
            completed_steps: None,
        };

        let plan = build_storage_plan(&req, true).unwrap();
        assert!(validate_storage_plan_roots(&plan, req.roots.as_deref()).is_some());
    }

    #[test]
    fn job_view_projects_progress_and_checkpoint_fields() {
        let view = JobView::from(EngineJob {
            job_id: "job-1".to_owned(),
            kind: "storage_plan".to_owned(),
            state: "running".to_owned(),
            dry_run: false,
            affected_torrents: vec!["a".repeat(40)],
            total: 10,
            done: 4,
            checkpoint: 3,
            byte_offset: Some(2048),
            verified_bytes: 1024,
            error: None,
            created_at: 1,
            started_at: Some(2),
            updated_at: 3,
            finished_at: None,
        });

        assert_eq!(view.job_id, "job-1");
        assert_eq!(view.kind, "storage_plan");
        assert_eq!(view.progress, 0.4);
        assert_eq!(view.checkpoint, 3);
        assert_eq!(view.byte_offset, Some(2048));
        assert_eq!(view.affected_torrents.len(), 1);
    }

    async fn setup_authed_app_with_torrent() -> (axum::Router, String) {
        let state = AppState::with_tokens(None, vec!["secret-token".to_owned()]);
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
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["ready"], false);
        assert_eq!(body["engine"]["mode"], "unavailable");
        assert_eq!(body["engine"]["track1_sidecar_required"], false);
        assert_eq!(
            body["engine"]["capabilities"]["torrent_identity"]["hash_lengths"],
            serde_json::json!([40, 64])
        );
        assert_eq!(
            body["engine"]["capabilities"]["compatibility"]["transmission_rpc"],
            true
        );
    }

    #[test]
    fn native_engine_capabilities_cover_rewrite_surface() {
        let capabilities = native_engine_capabilities();
        assert_eq!(capabilities["torrent_identity"]["v2"], true);
        assert_eq!(
            capabilities["metadata"]["pure_v2_metadata_completion"],
            true
        );
        assert_eq!(capabilities["session"]["crash_restore"], true);
        assert_eq!(capabilities["jobs"]["durable_recheck"], true);
        assert_eq!(capabilities["storage"]["v2_file_root_verify"], true);
        assert_eq!(capabilities["networking"]["dht"], true);
        assert_eq!(capabilities["networking"]["utp_packet_codec"], true);
        assert_eq!(capabilities["networking"]["utp_udp_stream"], true);
        assert_eq!(capabilities["networking"]["utp_outgoing_opt_in"], true);
        assert_eq!(capabilities["networking"]["utp_incoming_opt_in"], true);
        assert_eq!(capabilities["networking"]["utp_outgoing_policy"], "auto");
        assert_eq!(capabilities["networking"]["utp_outgoing_enabled"], true);
        assert_eq!(capabilities["networking"]["utp_metadata_policy"], "off");
        assert_eq!(capabilities["networking"]["utp_metadata_enabled"], false);
        assert_eq!(capabilities["networking"]["utp_transport"], true);
        assert_eq!(
            capabilities["networking"]["utp_transport_paths"][0],
            "outgoing_peer_wire"
        );
        assert_eq!(capabilities["compatibility"]["qbittorrent_v2"], true);
        assert_eq!(capabilities["migration"]["transmission"], true);
        assert_eq!(capabilities["operations"]["prometheus_metrics"], true);
    }

    #[test]
    fn utp_capability_helpers_match_runtime_policy_values() {
        for enabled in [
            "1",
            "true",
            "yes",
            "on",
            "prefer",
            "prefer-utp",
            "utp-prefer",
            "only",
            "utp",
            "utp-only",
            "auto",
        ] {
            assert!(utp_policy_allows_peer_wire(enabled), "{enabled}");
        }
        for disabled in ["0", "false", "no", "off", "tcp", "tcp-only"] {
            assert!(!utp_policy_allows_peer_wire(disabled), "{disabled}");
        }
        assert!(utp_incoming_env_enabled("on"));
        assert!(utp_incoming_env_enabled("1"));
        assert!(!utp_incoming_env_enabled("utp-only"));
        assert!(!utp_incoming_env_enabled("prefer"));
        assert!(!utp_incoming_env_enabled("tcp-only"));
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
        let mut stats = rt_engine::EngineStats {
            torrents_total: 2,
            torrents_seeding: 1,
            torrents_activity_hot: 2,
            torrents_activity_warm: 3,
            torrents_activity_dormant: 4,
            torrent_tasks_active: 5,
            fastresume_dirty_pieces: 6,
            completed_piece_verify_from_memory: 7,
            completed_piece_verify_from_disk: 8,
            jobs_active: 3,
            trackers_error: 4,
            dht_routing_nodes: 45,
            dht_announced_peer_sets: 46,
            dht_announced_peers: 47,
            dht_tracked_torrents: 48,
            dht_outstanding_requests: 49,
            dht_queried_nodes: 50,
            storage_file_pool_memory_bytes: 57,
            storage_file_pool_hits: 5,
            storage_read_ops: 6,
            storage_hash_ops: 7,
            storage_queued_disk_bytes: 54,
            storage_queue_full: 56,
            storage_peer_read_cache_hits: 8,
            storage_peer_read_cache_evictions: 58,
            piece_assembly_evictions: 9,
            storage_backend_read_ops: 14,
            storage_backend_bytes_read: 15,
            storage_read_latency_ns: 18,
            storage_write_latency_ns: 19,
            storage_sync_latency_ns: 20,
            storage_hash_latency_ns: 21,
            storage_device_queue_capacity: 62,
            storage_device_queue_available: 63,
            storage_peer_read_elevator_enabled: 24,
            storage_peer_read_elevator_queue_depth: 25,
            storage_peer_read_elevator_queued: 26,
            storage_peer_read_elevator_queue_full: 55,
            storage_peer_read_elevator_batches: 27,
            storage_peer_read_elevator_coalesced_requests: 28,
            storage_page_cache_advise_sequential: 29,
            storage_page_cache_advise_willneed: 30,
            storage_page_cache_advise_dontneed: 31,
            storage_page_cache_advise_failures: 32,
            storage_sparse_data_extents: 33,
            storage_sparse_hole_bytes: 34,
            storage_sparse_seek_fallbacks: 35,
            peer_request_window_reductions: 40,
            peer_rx_buffer_bytes: 41,
            peer_tx_buffer_bytes: 42,
            peer_command_queue_depth: 51,
            peer_command_queue_capacity: 52,
            peer_command_queue_full: 53,
            tracker_peer_cache_entries: 43,
            tracker_peer_cache_drops: 44,
            ..Default::default()
        };
        stats.storage_read_ops_by_class[4] = 10;
        stats.storage_write_ops_by_class[3] = 11;
        stats.storage_bytes_read_by_class[4] = 12;
        stats.storage_bytes_written_by_class[3] = 13;
        stats.storage_backend_read_ops_by_class[4] = 16;
        stats.storage_backend_bytes_read_by_class[4] = 17;
        stats.storage_read_latency_ns_by_class[4] = 22;
        stats.storage_write_latency_ns_by_class[3] = 23;
        stats.storage_read_latency_buckets[7] = 6;
        stats.storage_write_latency_buckets[7] = 11;
        stats.storage_sync_latency_buckets[7] = 2;
        stats.storage_hash_latency_buckets[7] = 7;
        stats.storage_device_latencies = vec![rt_engine::StorageDeviceLatencyStats {
            device_id: "pool\"a\\disk\n1".to_owned(),
            profile: "hdd".to_owned(),
            read_latency_ns: 36,
            write_latency_ns: 37,
            sync_latency_ns: 38,
            hash_latency_ns: 39,
            read_latency_buckets: {
                let mut buckets = [0; STORAGE_LATENCY_BUCKETS_NS.len()];
                buckets[7] = 6;
                buckets
            },
            write_latency_buckets: {
                let mut buckets = [0; STORAGE_LATENCY_BUCKETS_NS.len()];
                buckets[7] = 11;
                buckets
            },
            sync_latency_buckets: {
                let mut buckets = [0; STORAGE_LATENCY_BUCKETS_NS.len()];
                buckets[7] = 2;
                buckets
            },
            hash_latency_buckets: {
                let mut buckets = [0; STORAGE_LATENCY_BUCKETS_NS.len()];
                buckets[7] = 7;
                buckets
            },
        }];
        stats.hot_torrent_memory_top = vec![rt_engine::HotTorrentMemoryStats {
            info_hash: "abc\"def\\ghi\nj".to_owned(),
            estimated_bytes: 54,
            piece_assembly_bytes: 55,
            peer_buffer_bytes: 56,
            tracker_peer_bytes: 57,
            peer_command_queue_bytes: 58,
            storage_cache_bytes: 59,
        }];
        let governor = rt_metrics::ResourceGovernor::new(Default::default());
        assert!(governor
            .try_acquire(MemoryClass::ApiSnapshot, u64::MAX)
            .is_none());
        stats.resources = Some(governor.snapshot());
        let rendered = render_metrics(&stats);
        assert!(rendered.contains("torrentng_torrents_total 2"));
        assert!(rendered.contains("torrentng_torrents_seeding 1"));
        assert!(rendered.contains("torrentng_torrents_activity_hot 2"));
        assert!(rendered.contains("torrentng_torrents_activity_warm 3"));
        assert!(rendered.contains("torrentng_torrents_activity_dormant 4"));
        assert!(rendered.contains("torrentng_torrent_tasks_active 5"));
        assert!(rendered.contains("torrentng_fastresume_dirty_pieces 6"));
        assert!(rendered.contains("torrentng_completed_piece_verify_from_memory_total 7"));
        assert!(rendered.contains("torrentng_completed_piece_verify_from_disk_total 8"));
        assert!(rendered.contains("torrentng_jobs_active 3"));
        assert!(rendered.contains("torrentng_trackers_error 4"));
        assert!(rendered.contains("torrentng_dht_routing_nodes 45"));
        assert!(rendered.contains("torrentng_dht_announced_peer_sets 46"));
        assert!(rendered.contains("torrentng_dht_announced_peers 47"));
        assert!(rendered.contains("torrentng_dht_tracked_torrents 48"));
        assert!(rendered.contains("torrentng_dht_outstanding_requests 49"));
        assert!(rendered.contains("torrentng_dht_queried_nodes 50"));
        assert!(rendered.contains("torrentng_storage_file_pool_memory_bytes 57"));
        assert!(rendered.contains("torrentng_storage_file_pool_hits_total 5"));
        assert!(rendered.contains("torrentng_storage_read_ops_total 6"));
        assert!(rendered.contains("torrentng_storage_hash_ops_total 7"));
        assert!(rendered.contains("torrentng_storage_device_queue_capacity 62"));
        assert!(rendered.contains("torrentng_storage_device_queue_available 63"));
        assert!(rendered.contains("torrentng_storage_queued_disk_bytes 54"));
        assert!(rendered.contains("torrentng_storage_queue_full_total 56"));
        assert!(rendered.contains("torrentng_storage_peer_read_cache_hits_total 8"));
        assert!(rendered.contains("torrentng_storage_peer_read_cache_evictions_total 58"));
        assert!(rendered.contains("torrentng_piece_assembly_evictions_total 9"));
        assert!(rendered.contains("torrentng_peer_request_window_reductions_total 40"));
        assert!(rendered.contains("torrentng_peer_rx_buffer_bytes 41"));
        assert!(rendered.contains("torrentng_peer_tx_buffer_bytes 42"));
        assert!(rendered.contains("torrentng_peer_command_queue_depth 51"));
        assert!(rendered.contains("torrentng_peer_command_queue_capacity 52"));
        assert!(rendered.contains("torrentng_peer_command_queue_full_total 53"));
        assert!(rendered.contains("torrentng_tracker_peer_cache_entries 43"));
        assert!(rendered.contains("torrentng_tracker_peer_cache_drops_total 44"));
        assert!(rendered.contains(
            "torrentng_hot_torrent_memory_estimated_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 54"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_piece_assembly_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 55"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_peer_buffer_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 56"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_tracker_peer_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 57"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_peer_command_queue_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 58"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_storage_cache_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 59"
        ));
        assert!(
            rendered.contains("torrentng_storage_read_ops_by_class_total{class=\"peer_read\"} 10")
        );
        assert!(rendered
            .contains("torrentng_storage_write_ops_by_class_total{class=\"peer_write\"} 11"));
        assert!(rendered
            .contains("torrentng_storage_bytes_read_by_class_total{class=\"peer_read\"} 12"));
        assert!(rendered
            .contains("torrentng_storage_bytes_written_by_class_total{class=\"peer_write\"} 13"));
        assert!(rendered.contains("torrentng_storage_backend_read_ops_total 14"));
        assert!(rendered.contains("torrentng_storage_backend_bytes_read_total 15"));
        assert!(rendered
            .contains("torrentng_storage_backend_read_ops_by_class_total{class=\"peer_read\"} 16"));
        assert!(rendered.contains(
            "torrentng_storage_backend_bytes_read_by_class_total{class=\"peer_read\"} 17"
        ));
        assert!(rendered.contains("torrentng_storage_read_latency_nanoseconds_total 18"));
        assert!(rendered.contains("torrentng_storage_write_latency_nanoseconds_total 19"));
        assert!(rendered.contains("torrentng_storage_sync_latency_nanoseconds_total 20"));
        assert!(rendered.contains("torrentng_storage_hash_latency_nanoseconds_total 21"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_enabled 24"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_queue_depth 25"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_queued 26"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_queue_full_total 55"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_batches_total 27"));
        assert!(
            rendered.contains("torrentng_storage_peer_read_elevator_coalesced_requests_total 28")
        );
        assert!(rendered.contains("torrentng_storage_page_cache_advise_sequential_total 29"));
        assert!(rendered.contains("torrentng_storage_page_cache_advise_willneed_total 30"));
        assert!(rendered.contains("torrentng_storage_page_cache_advise_dontneed_total 31"));
        assert!(rendered.contains("torrentng_storage_page_cache_advise_failures_total 32"));
        assert!(rendered.contains("torrentng_storage_sparse_data_extents_total 33"));
        assert!(rendered.contains("torrentng_storage_sparse_hole_bytes_total 34"));
        assert!(rendered.contains("torrentng_storage_sparse_seek_fallbacks_total 35"));
        assert!(rendered.contains(
            "torrentng_storage_read_latency_nanoseconds_by_class_total{class=\"peer_read\"} 22"
        ));
        assert!(rendered.contains(
            "torrentng_storage_write_latency_nanoseconds_by_class_total{class=\"peer_write\"} 23"
        ));
        assert!(rendered.contains(
            "torrentng_storage_read_latency_nanoseconds_by_device_total{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 36"
        ));
        assert!(rendered.contains(
            "torrentng_storage_write_latency_nanoseconds_by_device_total{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 37"
        ));
        assert!(rendered.contains(
            "torrentng_storage_sync_latency_nanoseconds_by_device_total{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 38"
        ));
        assert!(rendered.contains(
            "torrentng_storage_hash_latency_nanoseconds_by_device_total{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 39"
        ));
        assert!(rendered.contains(
            "torrentng_storage_read_latency_nanoseconds_by_device_bucket{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\",le=\"+Inf\"} 6"
        ));
        assert!(rendered.contains(
            "torrentng_storage_write_latency_nanoseconds_by_device_bucket{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\",le=\"+Inf\"} 11"
        ));
        assert!(rendered.contains(
            "torrentng_storage_sync_latency_nanoseconds_by_device_count{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 2"
        ));
        assert!(rendered.contains(
            "torrentng_storage_hash_latency_nanoseconds_by_device_sum{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 39"
        ));
        assert!(
            rendered.contains("torrentng_storage_read_latency_nanoseconds_bucket{le=\"+Inf\"} 6")
        );
        assert!(rendered.contains("torrentng_storage_read_latency_nanoseconds_sum 18"));
        assert!(rendered.contains("torrentng_storage_read_latency_nanoseconds_count 6"));
        assert!(
            rendered.contains("torrentng_storage_write_latency_nanoseconds_bucket{le=\"+Inf\"} 11")
        );
        assert!(
            rendered.contains("torrentng_storage_sync_latency_nanoseconds_bucket{le=\"+Inf\"} 2")
        );
        assert!(
            rendered.contains("torrentng_storage_hash_latency_nanoseconds_bucket{le=\"+Inf\"} 7")
        );
        assert!(rendered.contains("torrentng_memory_cap_bytes "));
        assert!(rendered.contains("torrentng_memory_class_cap_bytes{class=\"api_snapshot\"} "));
        assert!(rendered
            .contains("torrentng_memory_class_denied_allocations_total{class=\"api_snapshot\"} 1"));
        assert!(rendered.contains("torrentng_storage_backend_selected{backend=\""));
        assert!(rendered.contains("torrentng_storage_backend_fixed_buffers_supported "));
        assert!(rendered.contains("torrentng_storage_backend_registered_files_supported "));
        assert!(rendered.contains("torrentng_storage_backend_max_batch_len "));
        assert!(rendered.contains("torrentng_storage_backend_fixed_buffer_bytes "));
        assert!(rendered.contains("torrentng_storage_backend_fixed_buffer_strategy{strategy=\""));
        assert!(rendered.contains("torrentng_storage_backend_fixed_buffer_worker_copy "));
        assert!(rendered.contains("torrentng_storage_backend_frame_pool_slots_supported "));
        assert!(rendered.contains("torrentng_storage_handles_open "));
        assert!(rendered.contains("torrentng_storage_frame_bytes_cap "));
        assert!(rendered.contains("torrentng_utp_connects_total "));
        assert!(rendered.contains("torrentng_utp_accepts_total "));
        assert!(rendered.contains("torrentng_utp_bytes_sent_total "));
        assert!(rendered.contains("torrentng_utp_bytes_received_total "));
        assert!(rendered.contains("torrentng_utp_send_timeouts_total "));
        assert!(rendered.contains("torrentng_utp_recv_timeouts_total "));
        assert!(rendered.contains("torrentng_utp_retransmits_total "));
        assert!(rendered.contains("torrentng_utp_route_drops_total "));
        assert!(rendered.contains("torrentng_utp_rtt_samples_total "));
        assert!(rendered.contains("torrentng_utp_rtt_us "));
        assert!(rendered.contains("torrentng_utp_rtt_min_us "));
        assert!(rendered.contains("torrentng_utp_rtt_max_us "));
        assert!(rendered.contains("torrentng_utp_rtt_var_us "));
        assert!(rendered.contains("torrentng_utp_retransmit_timeout_us "));
        assert!(rendered.contains("torrentng_utp_congestion_window_bytes "));
        assert!(rendered.contains("torrentng_utp_congestion_base_delay_us "));
        assert!(rendered.contains("torrentng_utp_congestion_current_delay_us "));
        assert!(rendered.contains("torrentng_utp_bytes_in_flight "));
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
    async fn jobs_without_engine_returns_unavailable() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/jobs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn storage_without_engine_returns_unavailable() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/storage")
                    .body(Body::empty())
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
    async fn update_torrent_without_engine_updates_registry() {
        let state = AppState::new();
        let hash = "u".repeat(40);
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.clone(),
                "old.torrent".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        let registry = state.registry.clone();
        let app = build_router(state);
        let body = serde_json::json!({ "name": "new.torrent", "save_path": "/new-data" });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/v1/torrents/{hash}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let reg = registry.read().await;
        let entry = reg.get(&hash).unwrap();
        assert_eq!(entry.name, "new.torrent");
        assert_eq!(entry.save_path, "/new-data");
    }

    #[tokio::test]
    async fn torrent_limits_without_engine_returns_unavailable() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/torrents/{hash}/limits"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn transfer_limits_without_engine_returns_unavailable() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/transfer/limits")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn update_torrent_limits_request_distinguishes_null_from_absent() {
        let req: UpdateTorrentLimitsRequest = serde_json::from_value(serde_json::json!({
            "seed_ratio_limit": null,
            "sequential_download": true
        }))
        .unwrap();
        let mut limits = EngineTorrentLimits {
            seed_ratio_limit: Some(2.0),
            ..Default::default()
        };
        assert!(matches!(req.download_limit, None));

        merge_torrent_limits(&mut limits, req).unwrap();

        assert_eq!(limits.seed_ratio_limit, None);
        assert!(limits.sequential_download);
    }

    #[tokio::test]
    async fn native_alias_and_projection_routes_are_exposed() {
        let (app, hash) = setup_app_with_torrent().await;
        for (method, uri, expected) in [
            (
                "POST",
                format!("/api/v1/torrents/{hash}/start"),
                StatusCode::NO_CONTENT,
            ),
            (
                "POST",
                format!("/api/v1/torrents/{hash}/stop"),
                StatusCode::NO_CONTENT,
            ),
            (
                "GET",
                format!("/api/v1/torrents/{hash}/files"),
                StatusCode::OK,
            ),
            (
                "GET",
                format!("/api/v1/torrents/{hash}/trackers"),
                StatusCode::OK,
            ),
            (
                "GET",
                "/api/v1/transfer/limits".to_owned(),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            ("GET", "/api/v1/transfer/info".to_owned(), StatusCode::OK),
            (
                "GET",
                "/api/v1/session/features".to_owned(),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            ("POST", "/api/v1/auth/login".to_owned(), StatusCode::OK),
            ("POST", "/api/v1/auth/logout".to_owned(), StatusCode::OK),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), expected, "{method}");
        }
    }

    #[tokio::test]
    async fn native_login_issues_session_cookie_and_validates_tokens() {
        let app = build_router(AppState::with_tokens(None, vec!["secret token".to_owned()]));
        let bad = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=bad&password=wrong"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

        let ok = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=operator&password=secret+token"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let cookie = ok
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(cookie.starts_with("tng_session=secret%20token;"));
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
    async fn mutating_endpoint_accepts_session_cookie_token() {
        let (app, hash) = setup_authed_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/torrents/{hash}"))
                    .header(header::COOKIE, "other=1; tng_session=secret-token")
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

    #[tokio::test]
    async fn set_category_without_engine_updates_registry() {
        let state = AppState::new();
        let hash = "c".repeat(40);
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.clone(),
                "category.torrent".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        let registry = state.registry.clone();
        let app = build_router(state);
        let body = serde_json::json!({ "category": "Movies" });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/v1/torrents/{hash}/category"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            registry
                .read()
                .await
                .get(&hash)
                .unwrap()
                .category
                .as_deref(),
            Some("Movies")
        );
    }

    #[tokio::test]
    async fn patch_files_rejects_empty_body() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/torrents/{hash}/files"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"files":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn patch_tags_without_engine_updates_registry() {
        let state = AppState::new();
        let hash = "d".repeat(40);
        {
            let mut reg = state.registry.write().await;
            let mut entry = TorrentEntry::new(hash.clone(), "tags.torrent".into(), "/data".into());
            entry.tags = vec!["old".to_owned(), "keep".to_owned()];
            reg.add(entry).unwrap();
        }
        let registry = state.registry.clone();
        let app = build_router(state);
        let body = serde_json::json!({ "add": ["new", " keep ", ""], "remove": ["old"] });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/torrents/{hash}/tags"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            registry.read().await.get(&hash).unwrap().tags,
            vec!["keep".to_owned(), "new".to_owned()]
        );
    }

    #[tokio::test]
    async fn tag_post_delete_and_bulk_set_are_native() {
        let state = AppState::new();
        let hash = "e".repeat(40);
        {
            let mut reg = state.registry.write().await;
            let mut entry = TorrentEntry::new(hash.clone(), "tags.torrent".into(), "/data".into());
            entry.tags = vec!["old".to_owned()];
            reg.add(entry).unwrap();
        }
        let registry = state.registry.clone();
        let app = build_router(state);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/torrents/{hash}/tags"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tags":["new"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            registry.read().await.get(&hash).unwrap().tags,
            vec!["old".to_owned(), "new".to_owned()]
        );

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/bulk/set-tags")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"hashes":["{hash}"],"tags":["final"],"dry_run":false}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            registry.read().await.get(&hash).unwrap().tags,
            vec!["final".to_owned()]
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/torrents/{hash}/tags"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tags":["final"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(registry.read().await.get(&hash).unwrap().tags.is_empty());
    }

    #[tokio::test]
    async fn patch_trackers_without_engine_accepts_existing_torrent() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/torrents/{hash}/trackers"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"add":["udp://tracker/announce"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn torrent_delta_reports_initial_changes_and_removals() {
        let state = AppState::new();
        let hash = "b".repeat(40);
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.clone(),
                "delta.torrent".into(),
                "/data".into(),
            ))
            .unwrap();
        }

        let first = torrent_delta(&state, &BTreeMap::new()).await;
        assert_eq!(first.torrents.len(), 1);
        assert_eq!(first.torrents[0].info_hash, hash);
        assert!(first.removed.is_empty());

        {
            let mut reg = state.registry.write().await;
            reg.remove(&hash).unwrap();
        }
        let second = torrent_delta(&state, &first.current).await;
        assert!(second.torrents.is_empty());
        assert_eq!(second.removed, vec![hash]);
    }

    #[test]
    fn api_snapshot_estimates_scale_with_torrent_count() {
        assert_eq!(estimate_torrent_summary_snapshot_bytes(0), 0);
        assert_eq!(estimate_torrent_summary_snapshot_bytes(10), 10 * 1024);
        assert_eq!(estimate_torrent_delta_snapshot_bytes(10), 10 * 1536);
        let summary = TorrentSummary {
            info_hash: "a".repeat(40),
            name: "detail.bin".to_owned(),
            state: "downloading".to_owned(),
            total_length: 1024,
            downloaded: 0,
            uploaded: 0,
            ratio: 0.0,
            save_path: "/data".to_owned(),
            category: Some("linux".to_owned()),
            tags: vec!["iso".to_owned()],
            added_at: 1,
            completed_at: None,
            num_peers: 0,
            num_seeds: 0,
        };
        let small_meta = rt_engine::EngineTorrentMetadata {
            piece_length: 16 * 1024,
            piece_count: 1,
            piece_hashes: vec!["b".repeat(40)],
            piece_states: Vec::new(),
            is_private: false,
            trackers: vec!["http://tracker/announce".to_owned()],
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            files: vec![rt_engine::EngineTorrentFile {
                index: 0,
                path: "detail.bin".to_owned(),
                length: 1024,
                priority: 1,
                wanted: true,
            }],
        };
        let mut large_meta = small_meta.clone();
        large_meta
            .files
            .extend((0..200).map(|idx| rt_engine::EngineTorrentFile {
                index: idx + 1,
                path: format!("dir/{idx}/large-detail-file-{idx}.bin"),
                length: 1024,
                priority: 1,
                wanted: true,
            }));

        assert_eq!(
            estimate_torrent_detail_snapshot_bytes(&summary, &small_meta),
            estimate_torrent_detail_base_snapshot_bytes()
        );
        assert!(
            estimate_torrent_detail_snapshot_bytes(&summary, &large_meta)
                > estimate_torrent_detail_snapshot_bytes(&summary, &small_meta)
        );
    }

    #[test]
    fn metric_with_label_escapes_label_values_once() {
        let mut rendered = String::new();
        metric_with_label(
            &mut rendered,
            "torrentng_test_metric",
            "gauge",
            "test metric",
            "backend",
            "pool\"a\\disk\n1",
            1,
        );

        assert!(rendered.contains("torrentng_test_metric{backend=\"pool\\\"a\\\\disk\\n1\"} 1"));
        assert!(!rendered.contains("pool\\\"a\\\\disk\\n1pool"));
    }
}
