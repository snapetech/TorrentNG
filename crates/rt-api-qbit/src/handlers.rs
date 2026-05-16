use axum::{
    extract::{Multipart, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use rt_metainfo::{parse_magnet, parse_torrent};
use serde::Deserialize;
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    net::IpAddr,
    time::Duration,
};
use url::Url;

use crate::{
    model::{
        to_qbit_state, QbCategoryInfo, QbFileInfo, QbServerState, QbTorrentInfo,
        QbTorrentProperties, QbTrackerInfo,
    },
    state::AppState,
};

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

/// `POST /api/qb/v2/auth/login` — always succeeds (auth handled at sidecar layer).
pub async fn auth_login() -> impl IntoResponse {
    (StatusCode::OK, "Ok.")
}

pub async fn auth_logout() -> impl IntoResponse {
    StatusCode::OK
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

pub async fn app_build_info() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "qt": "6.7.0",
            "libtorrent": "rtorrentNG-native",
            "boost": "",
            "openssl": "",
            "bitness": 64,
        })),
    )
}

pub async fn app_preferences(State(state): State<AppState>) -> impl IntoResponse {
    let save_path = default_save_path(&state).await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "save_path": save_path,
            "temp_path_enabled": false,
            "temp_path": "",
            "scan_dirs": {},
            "export_dir": "",
            "export_dir_fin": "",
            "mail_notification_enabled": false,
            "autorun_enabled": false,
            "queueing_enabled": false,
            "max_active_downloads": -1,
            "max_active_torrents": -1,
            "max_active_uploads": -1,
            "dont_count_slow_torrents": false,
            "dl_limit": 0,
            "up_limit": 0,
            "max_connec": -1,
            "max_connec_per_torrent": -1,
            "max_uploads": -1,
            "max_uploads_per_torrent": -1,
            "listen_port": 0,
            "dht": true,
            "pex": true,
            "lsd": false,
            "web_ui_domain_list": "*",
            "web_ui_address": "0.0.0.0",
            "web_ui_port": 8080,
        })),
    )
}

pub async fn app_set_preferences() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn app_shutdown() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn app_send_test_email() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn app_network_interface_list() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(vec![serde_json::json!({
            "name": "Any interface",
            "value": "",
        })]),
    )
}

pub async fn app_network_interface_address_list() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(vec![serde_json::json!({
            "name": "All addresses",
            "value": "",
        })]),
    )
}

pub async fn app_default_save_path(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, default_save_path(&state).await)
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
    let entries = {
        let reg = state.registry.read().await;
        reg.iter()
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
                if let Some(ref tag) = q.tag {
                    if !tag.is_empty() && !e.tags.iter().any(|entry_tag| entry_tag == tag) {
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
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut infos = Vec::with_capacity(entries.len());
    for entry in &entries {
        infos.push(qbit_torrent_info(&state, entry).await);
    }

    sort_torrent_infos(&mut infos, q.sort.as_deref(), q.reverse.unwrap_or(false));

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

/// `POST /api/qb/v2/torrents/add`.
pub async fn torrents_add(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Fails.").into_response();
    };

    let mut save_path = String::new();
    let mut paused = false;
    let mut stopped = false;
    let mut torrent_blobs = Vec::new();
    let mut urls = String::new();
    let mut category = String::new();
    let mut tags = Vec::<String>::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("savepath") => {
                save_path = field.text().await.unwrap_or_default();
            }
            Some("paused") => {
                paused = field.text().await.unwrap_or_default() == "true";
            }
            Some("stopped") => {
                stopped = field.text().await.unwrap_or_default() == "true";
            }
            Some("urls") => {
                urls = field.text().await.unwrap_or_default();
            }
            Some("category") => {
                category = field.text().await.unwrap_or_default();
            }
            Some("tags") => {
                tags = split_tags(&field.text().await.unwrap_or_default());
            }
            Some("torrents") => {
                if let Ok(bytes) = field.bytes().await {
                    torrent_blobs.push(bytes.to_vec());
                }
            }
            _ => {}
        }
    }

    for url in urls.lines().map(str::trim).filter(|url| !url.is_empty()) {
        if url.starts_with("magnet:") {
            let magnet = match parse_magnet(url) {
                Ok(magnet) => magnet,
                Err(e) => {
                    tracing::error!("qb add magnet parse failed: {e}");
                    return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                }
            };
            let save_path = if save_path.trim().is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(save_path.clone()))
            };
            if let Err(e) = engine
                .add_magnet_with_labels(
                    magnet,
                    save_path,
                    paused || stopped,
                    Some(category.clone()),
                    tags.clone(),
                )
                .await
            {
                tracing::error!("qb add magnet failed: {e}");
                return (StatusCode::BAD_REQUEST, "Fails.").into_response();
            }
            continue;
        }
        match fetch_torrent_url(url).await {
            Ok(raw) => torrent_blobs.push(raw),
            Err(e) => {
                tracing::error!("qb add torrent url {url}: {e}");
                return (StatusCode::BAD_REQUEST, "Fails.").into_response();
            }
        }
    }

    if torrent_blobs.is_empty() {
        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
    }

    let save_path = if save_path.trim().is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(save_path))
    };
    let start_paused = paused || stopped;

    for raw in torrent_blobs {
        let meta = match parse_torrent(&raw) {
            Ok(meta) => meta,
            Err(e) => {
                tracing::error!("qb add torrent parse failed: {e}");
                return (StatusCode::BAD_REQUEST, "Fails.").into_response();
            }
        };
        if let Err(e) = engine
            .add_torrent_with_labels(
                meta,
                save_path.clone(),
                start_paused,
                Some(category.clone()),
                tags.clone(),
            )
            .await
        {
            tracing::error!("qb add torrent failed: {e}");
            return (StatusCode::BAD_REQUEST, "Fails.").into_response();
        }
    }

    (StatusCode::OK, "Ok.").into_response()
}

/// `POST /api/qb/v2/torrents/pause` — pause by hashes (pipe-separated or "all").
pub async fn torrents_pause(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let hashes = resolve_hashes(&state, extract_hashes(&body)).await;
    if let Some(engine) = &state.engine {
        for hash in hashes {
            let _ = engine.pause_torrent(hash).await;
        }
    } else {
        let mut reg = state.registry.write().await;
        for hash in hashes {
            if let Some(e) = reg.get_mut(&hash) {
                let _ = e.transition(rt_session::TorrentState::Paused);
            }
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/resume`.
pub async fn torrents_resume(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let hashes = resolve_hashes(&state, extract_hashes(&body)).await;
    if let Some(engine) = &state.engine {
        for hash in hashes {
            let _ = engine.resume_torrent(hash).await;
        }
    } else {
        let mut reg = state.registry.write().await;
        for hash in hashes {
            if let Some(e) = reg.get_mut(&hash) {
                let _ = e.transition(rt_session::TorrentState::Downloading);
            }
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/start`.
pub async fn torrents_start(State(state): State<AppState>, body: String) -> impl IntoResponse {
    torrents_resume(State(state), body).await
}

/// `POST /api/qb/v2/torrents/stop`.
pub async fn torrents_stop(State(state): State<AppState>, body: String) -> impl IntoResponse {
    torrents_pause(State(state), body).await
}

/// `POST /api/qb/v2/torrents/delete`.
pub async fn torrents_delete(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let hashes = resolve_hashes(&state, hashes).await;
    let delete_files = params
        .get("deleteFiles")
        .map(|value| matches!(value.as_str(), "true" | "1"))
        .unwrap_or(false);
    if let Some(engine) = &state.engine {
        for hash in hashes {
            let _ = engine.remove_torrent(hash, delete_files).await;
        }
    } else {
        let mut reg = state.registry.write().await;
        for hash in hashes {
            let _ = reg.remove(&hash);
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/reannounce`.
pub async fn torrents_reannounce(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let hashes = resolve_hashes(&state, extract_hashes(&body)).await;
    if let Some(engine) = &state.engine {
        for hash in hashes {
            let _ = engine.reannounce_torrent(hash).await;
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/recheck`.
pub async fn torrents_recheck(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let hashes = resolve_hashes(&state, extract_hashes(&body)).await;
    if let Some(engine) = &state.engine {
        for hash in hashes {
            let _ = engine.recheck_torrent(hash).await;
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/filePrio`.
pub async fn torrents_file_prio() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/increasePrio`.
pub async fn torrents_increase_prio() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/decreasePrio`.
pub async fn torrents_decrease_prio() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/topPrio`.
pub async fn torrents_top_prio() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/bottomPrio`.
pub async fn torrents_bottom_prio() -> impl IntoResponse {
    StatusCode::OK
}

#[derive(Debug, Deserialize)]
pub struct HashQuery {
    pub hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HashesQuery {
    pub hashes: Option<String>,
}

/// `GET /api/qb/v2/torrents/trackers`.
pub async fn torrents_trackers(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return (StatusCode::BAD_REQUEST, Json(Vec::<QbTrackerInfo>::new()));
    };
    let exists = {
        let reg = state.registry.read().await;
        reg.get(&hash).is_some()
    };
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(Vec::<QbTrackerInfo>::new()));
    };
    match engine.torrent_metadata(hash).await {
        Ok(meta) => {
            let trackers = meta
                .trackers
                .into_iter()
                .enumerate()
                .map(|(idx, url)| QbTrackerInfo {
                    url,
                    status: 0,
                    tier: idx as i32,
                    num_peers: -1,
                    num_seeds: -1,
                    num_leeches: -1,
                    num_downloaded: -1,
                    msg: String::new(),
                })
                .collect();
            (StatusCode::OK, Json(trackers))
        }
        Err(_) if exists => (StatusCode::OK, Json(Vec::<QbTrackerInfo>::new())),
        Err(_) => (StatusCode::NOT_FOUND, Json(Vec::<QbTrackerInfo>::new())),
    }
}

/// `POST /api/qb/v2/torrents/addTrackers`.
pub async fn torrents_add_trackers() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/editTracker`.
pub async fn torrents_edit_tracker() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/removeTrackers`.
pub async fn torrents_remove_trackers() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/addPeers`.
pub async fn torrents_add_peers() -> impl IntoResponse {
    StatusCode::OK
}

/// `GET /api/qb/v2/torrents/files`.
pub async fn torrents_files(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return (StatusCode::BAD_REQUEST, Json(Vec::<QbFileInfo>::new()));
    };
    let exists = {
        let reg = state.registry.read().await;
        reg.get(&hash).is_some()
    };
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(Vec::<QbFileInfo>::new()));
    };
    let complete = {
        let reg = state.registry.read().await;
        reg.get(&hash)
            .map(|entry| entry.completed_at.is_some() || entry.amount_left == 0)
            .unwrap_or(false)
    };
    match engine.torrent_metadata(hash).await {
        Ok(meta) => {
            let files = meta
                .files
                .into_iter()
                .map(|file| QbFileInfo {
                    index: file.index,
                    name: file.path,
                    size: file.length as i64,
                    priority: 1,
                    progress: if complete { 1.0 } else { 0.0 },
                })
                .collect();
            (StatusCode::OK, Json(files))
        }
        Err(_) if exists => (StatusCode::OK, Json(Vec::<QbFileInfo>::new())),
        Err(_) => (StatusCode::NOT_FOUND, Json(Vec::<QbFileInfo>::new())),
    }
}

/// `GET /api/qb/v2/torrents/webseeds`.
pub async fn torrents_webseeds() -> impl IntoResponse {
    (StatusCode::OK, Json(Vec::<serde_json::Value>::new()))
}

/// `GET /api/qb/v2/torrents/pieceStates`.
pub async fn torrents_piece_states(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return (StatusCode::OK, Json(Vec::<i32>::new()));
    };
    let entry = {
        let reg = state.registry.read().await;
        reg.get(&hash).cloned()
    };
    let Some(entry) = entry else {
        return (StatusCode::NOT_FOUND, Json(Vec::<i32>::new()));
    };
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(Vec::<i32>::new()));
    };
    match engine.torrent_metadata(hash).await {
        Ok(meta) => {
            let have = pieces_have(
                entry.total_length,
                entry.amount_left,
                entry.completed_at.is_some(),
                meta.piece_length as i64,
                meta.piece_count as i64,
            ) as usize;
            let states = (0..meta.piece_count)
                .map(|index| if index < have { 2 } else { 0 })
                .collect();
            (StatusCode::OK, Json(states))
        }
        Err(_) => (StatusCode::OK, Json(Vec::<i32>::new())),
    }
}

/// `GET /api/qb/v2/torrents/pieceHashes`.
pub async fn torrents_piece_hashes(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return (StatusCode::OK, Json(Vec::<String>::new()));
    };
    let exists = {
        let reg = state.registry.read().await;
        reg.get(&hash).is_some()
    };
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(Vec::<String>::new()));
    };
    match engine.torrent_metadata(hash).await {
        Ok(meta) => (StatusCode::OK, Json(meta.piece_hashes)),
        Err(_) if exists => (StatusCode::OK, Json(Vec::<String>::new())),
        Err(_) => (StatusCode::NOT_FOUND, Json(Vec::<String>::new())),
    }
}

/// `GET /api/qb/v2/torrents/export`.
pub async fn torrents_export() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-bittorrent")],
        Vec::<u8>::new(),
    )
}

/// `GET /api/qb/v2/torrents/properties`.
pub async fn torrents_properties(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return (
            StatusCode::BAD_REQUEST,
            Json(default_torrent_properties(String::new())),
        );
    };

    let entry = {
        let reg = state.registry.read().await;
        reg.get(&hash).cloned()
    };
    let Some(entry) = entry else {
        return (
            StatusCode::NOT_FOUND,
            Json(default_torrent_properties(String::new())),
        );
    };

    let (piece_size, pieces_num) = if let Some(engine) = &state.engine {
        match engine.torrent_metadata(hash).await {
            Ok(meta) => (meta.piece_length as i64, meta.piece_count as i64),
            Err(_) => (0, 0),
        }
    } else {
        (0, 0)
    };

    let pieces_have = pieces_have(
        entry.total_length,
        entry.amount_left,
        entry.completed_at.is_some(),
        piece_size,
        pieces_num,
    );
    let props = QbTorrentProperties {
        save_path: format!("{}/", entry.save_path.trim_end_matches('/')),
        creation_date: entry.added_at as i64,
        piece_size,
        comment: String::new(),
        total_wasted: 0,
        total_uploaded: entry.stats.uploaded as i64,
        total_uploaded_session: entry.stats.uploaded as i64,
        total_downloaded: entry.stats.downloaded as i64,
        total_downloaded_session: entry.stats.downloaded as i64,
        up_limit: -1,
        dl_limit: -1,
        time_elapsed: 0,
        seeding_time: 0,
        nb_connections: 0,
        nb_connections_limit: -1,
        share_ratio: entry.stats.ratio(),
        addition_date: entry.added_at as i64,
        completion_date: entry.completed_at.map(|t| t as i64).unwrap_or(-1),
        created_by: String::new(),
        dl_speed_avg: 0,
        dl_speed: 0,
        eta: -1,
        last_seen: -1,
        peers: 0,
        peers_total: 0,
        pieces_have,
        pieces_num,
        reannounce: -1,
        seeds: 0,
        seeds_total: 0,
        total_size: entry.total_length as i64,
        up_speed_avg: 0,
        up_speed: 0,
    };
    (StatusCode::OK, Json(props))
}

/// `GET /api/qb/v2/torrents/categories`.
pub async fn torrents_categories(State(state): State<AppState>) -> impl IntoResponse {
    let mut categories = serde_json::Map::new();
    {
        let stored = state.categories.read().await;
        for (category, save_path) in stored.iter() {
            let info = QbCategoryInfo {
                name: category.clone(),
                save_path: format!("{}/", save_path.trim_end_matches('/')),
            };
            categories.insert(category.clone(), serde_json::to_value(info).unwrap());
        }
    }
    let reg = state.registry.read().await;
    for entry in reg.iter() {
        let Some(category) = entry.category.as_deref() else {
            continue;
        };
        if category.is_empty() || categories.contains_key(category) {
            continue;
        }
        let info = QbCategoryInfo {
            name: category.to_owned(),
            save_path: format!("{}/", entry.save_path.trim_end_matches('/')),
        };
        categories.insert(category.to_owned(), serde_json::to_value(info).unwrap());
    }
    (StatusCode::OK, Json(serde_json::Value::Object(categories)))
}

/// `GET /api/qb/v2/torrents/tags`.
pub async fn torrents_tags(State(state): State<AppState>) -> impl IntoResponse {
    let mut tags = state.tags.read().await.clone();
    {
        let reg = state.registry.read().await;
        for entry in reg.iter() {
            tags.extend(entry.tags.iter().filter(|tag| !tag.is_empty()).cloned());
        }
    }
    (StatusCode::OK, Json(tags.into_iter().collect::<Vec<_>>()))
}

/// `POST /api/qb/v2/torrents/rename`.
pub async fn torrents_rename(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").cloned() else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(name) = params.get("name").cloned() else {
        return StatusCode::BAD_REQUEST;
    };
    update_torrent_fields(&state, &hash, Some(name), None).await
}

/// `POST /api/qb/v2/torrents/renameFile`.
pub async fn torrents_rename_file() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/renameFolder`.
pub async fn torrents_rename_folder() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setLocation`.
pub async fn torrents_set_location(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let hashes = resolve_hashes(&state, hashes).await;
    let Some(location) = params.get("location").cloned() else {
        return StatusCode::BAD_REQUEST;
    };
    for hash in hashes {
        let status = update_torrent_fields(
            &state,
            &hash,
            None,
            Some(std::path::PathBuf::from(location.clone())),
        )
        .await;
        if status != StatusCode::OK {
            return status;
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setSavePath`.
pub async fn torrents_set_save_path(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    torrents_set_location(State(state), body).await
}

/// `POST /api/qb/v2/torrents/createCategory`.
pub async fn torrents_create_category(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(category) = params
        .get("category")
        .and_then(|category| normalize_api_text(category))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let save_path = params
        .get("savePath")
        .and_then(|save_path| normalize_api_text(save_path))
        .unwrap_or_default();
    state.categories.write().await.insert(category, save_path);
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/editCategory`.
pub async fn torrents_edit_category(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(category) = params
        .get("category")
        .and_then(|category| normalize_api_text(category))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_category) = params
        .get("newCategory")
        .and_then(|category| normalize_api_text(category))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let hashes = {
        let reg = state.registry.read().await;
        reg.iter()
            .filter(|entry| entry.category.as_deref() == Some(category.as_str()))
            .map(|entry| entry.info_hash.clone())
            .collect::<Vec<_>>()
    };
    for hash in hashes {
        update_torrent_category(&state, &hash, Some(new_category.clone())).await;
    }
    let mut categories = state.categories.write().await;
    if let Some(save_path) = categories.remove(&category) {
        categories.insert(new_category, save_path);
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/removeCategories`.
pub async fn torrents_remove_categories(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let categories = params
        .get("categories")
        .map(|value| split_pipe_values(value))
        .unwrap_or_default();
    let hashes = {
        let reg = state.registry.read().await;
        reg.iter()
            .filter(|entry| {
                entry
                    .category
                    .as_ref()
                    .is_some_and(|category| categories.contains(category))
            })
            .map(|entry| entry.info_hash.clone())
            .collect::<Vec<_>>()
    };
    for hash in hashes {
        update_torrent_category(&state, &hash, None).await;
    }
    let mut stored = state.categories.write().await;
    for category in categories {
        stored.remove(&category);
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/createTags`.
pub async fn torrents_create_tags(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let tags = params
        .get("tags")
        .map(|tags| split_tags(tags))
        .unwrap_or_default();
    if tags.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    state.tags.write().await.extend(tags);
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/deleteTags`.
pub async fn torrents_delete_tags(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let remove_tags = params
        .get("tags")
        .map(|tags| split_tags(tags))
        .unwrap_or_default();
    if remove_tags.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    let hashes = {
        let reg = state.registry.read().await;
        reg.iter()
            .filter(|entry| entry.tags.iter().any(|tag| remove_tags.contains(tag)))
            .map(|entry| entry.info_hash.clone())
            .collect::<Vec<_>>()
    };
    for hash in hashes {
        update_torrent_tags(&state, &hash, Vec::new(), remove_tags.clone()).await;
    }
    let mut stored = state.tags.write().await;
    for tag in remove_tags {
        stored.remove(&tag);
    }
    StatusCode::OK
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
    let hashes = resolve_hashes(&state, hashes).await;
    let category = params.get("category").cloned().unwrap_or_default();
    if let Some(engine) = &state.engine {
        for hash in &hashes {
            let category = if category.trim().is_empty() {
                None
            } else {
                Some(category.clone())
            };
            let _ = engine
                .update_torrent_labels(hash.clone(), Some(category), Vec::new(), Vec::new())
                .await;
        }
    } else {
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
    let hashes = resolve_hashes(&state, hashes).await;
    let new_tags: Vec<String> = params
        .get("tags")
        .map(|t| split_tags(t))
        .unwrap_or_default();
    if let Some(engine) = &state.engine {
        for hash in &hashes {
            let _ = engine
                .update_torrent_labels(hash.clone(), None, new_tags.clone(), Vec::new())
                .await;
        }
    } else {
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
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setTags`.
pub async fn torrents_set_tags(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let hashes = resolve_hashes(&state, hashes).await;
    let new_tags = params
        .get("tags")
        .map(|tags| split_tags(tags))
        .unwrap_or_default();
    for hash in hashes {
        let old_tags = {
            let reg = state.registry.read().await;
            reg.get(&hash)
                .map(|entry| entry.tags.clone())
                .unwrap_or_default()
        };
        let status = update_torrent_tags(&state, &hash, new_tags.clone(), old_tags).await;
        if status != StatusCode::OK {
            return status;
        }
    }
    state.tags.write().await.extend(new_tags);
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/removeTags`.
pub async fn torrents_remove_tags(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let hashes = resolve_hashes(&state, hashes).await;
    let remove_tags: Vec<String> = params
        .get("tags")
        .map(|t| split_tags(t))
        .unwrap_or_default();
    if let Some(engine) = &state.engine {
        for hash in &hashes {
            let _ = engine
                .update_torrent_labels(hash.clone(), None, Vec::new(), remove_tags.clone())
                .await;
        }
    } else {
        let mut reg = state.registry.write().await;
        for hash in &hashes {
            if let Some(e) = reg.get_mut(hash) {
                e.tags.retain(|tag| !remove_tags.contains(tag));
            }
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setDownloadLimit`.
pub async fn torrents_set_download_limit() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setUploadLimit`.
pub async fn torrents_set_upload_limit() -> impl IntoResponse {
    StatusCode::OK
}

/// `GET /api/qb/v2/torrents/downloadLimit`.
pub async fn torrents_download_limit(
    State(state): State<AppState>,
    Query(q): Query<HashesQuery>,
) -> impl IntoResponse {
    torrent_limit_map(&state, q.hashes).await
}

/// `GET /api/qb/v2/torrents/uploadLimit`.
pub async fn torrents_upload_limit(
    State(state): State<AppState>,
    Query(q): Query<HashesQuery>,
) -> impl IntoResponse {
    torrent_limit_map(&state, q.hashes).await
}

/// `POST /api/qb/v2/torrents/setShareLimits`.
pub async fn torrents_set_share_limits() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setForceStart`.
pub async fn torrents_set_force_start() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setSuperSeeding`.
pub async fn torrents_set_super_seeding() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setAutoTMM`.
pub async fn torrents_set_auto_tmm() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setAutoManagement`.
pub async fn torrents_set_auto_management() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/toggleSequentialDownload`.
pub async fn torrents_toggle_sequential_download() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/toggleFirstLastPiecePrio`.
pub async fn torrents_toggle_first_last_piece_prio() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/transfer/setDownloadLimit`.
pub async fn transfer_set_download_limit() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /api/qb/v2/transfer/setUploadLimit`.
pub async fn transfer_set_upload_limit() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn transfer_speed_limits_mode() -> impl IntoResponse {
    (StatusCode::OK, "0")
}

pub async fn transfer_toggle_speed_limits_mode() -> impl IntoResponse {
    StatusCode::OK
}

/// `GET /api/qb/v2/transfer/downloadLimit`.
pub async fn transfer_download_limit() -> impl IntoResponse {
    (StatusCode::OK, "0")
}

/// `GET /api/qb/v2/transfer/uploadLimit`.
pub async fn transfer_upload_limit() -> impl IntoResponse {
    (StatusCode::OK, "0")
}

// ---------------------------------------------------------------------------
// Sync & Transfer
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SyncMaindataQuery {
    pub rid: Option<i64>,
}

pub async fn sync_maindata(
    State(state): State<AppState>,
    Query(q): Query<SyncMaindataQuery>,
) -> impl IntoResponse {
    let entries = {
        let reg = state.registry.read().await;
        reg.iter().cloned().collect::<Vec<_>>()
    };
    let rid = sync_rid_for_entries(&entries);
    let full_update = q.rid.unwrap_or(0) != rid;
    let mut torrents = serde_json::Map::new();
    if full_update {
        for entry in &entries {
            torrents.insert(
                entry.info_hash.clone(),
                serde_json::to_value(qbit_torrent_info(&state, entry).await).unwrap(),
            );
        }
    }
    let resp = serde_json::json!({
        "rid": rid,
        "full_update": full_update,
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

pub async fn sync_torrent_peers() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "rid": 1,
            "full_update": true,
            "peers": {},
            "peers_removed": [],
            "show_flags": true,
        })),
    )
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

pub async fn transfer_ban_peers() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn log_main() -> impl IntoResponse {
    (StatusCode::OK, Json(Vec::<serde_json::Value>::new()))
}

pub async fn log_peers() -> impl IntoResponse {
    (StatusCode::OK, Json(Vec::<String>::new()))
}

pub async fn search_status() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "Stopped",
            "plugins": [],
        })),
    )
}

pub async fn search_plugins() -> impl IntoResponse {
    (StatusCode::OK, Json(Vec::<String>::new()))
}

pub async fn search_categories() -> impl IntoResponse {
    (StatusCode::OK, Json(Vec::<String>::new()))
}

pub async fn search_plugin_noop() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn search_start() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "id": 0 })))
}

pub async fn search_stop() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn search_results() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "Stopped",
            "total": 0,
            "results": [],
        })),
    )
}

pub async fn search_delete() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn rss_items() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({})))
}

pub async fn rss_rules() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({})))
}

pub async fn rss_matching_articles() -> impl IntoResponse {
    (StatusCode::OK, Json(Vec::<serde_json::Value>::new()))
}

pub async fn rss_noop() -> impl IntoResponse {
    StatusCode::OK
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

fn torrent_progress(total_length: u64, amount_left: u64, complete: bool) -> f64 {
    if complete {
        return 1.0;
    }
    if total_length == 0 {
        return 0.0;
    }
    let done = total_length.saturating_sub(amount_left);
    (done as f64 / total_length as f64).clamp(0.0, 1.0)
}

fn sort_torrent_infos(infos: &mut [QbTorrentInfo], sort: Option<&str>, reverse: bool) {
    match sort.unwrap_or_default() {
        "name" => infos.sort_by(|a, b| a.name.cmp(&b.name)),
        "size" => infos.sort_by_key(|info| info.size),
        "progress" => infos.sort_by(|a, b| a.progress.total_cmp(&b.progress)),
        "dlspeed" => infos.sort_by_key(|info| info.dlspeed),
        "upspeed" => infos.sort_by_key(|info| info.upspeed),
        "ratio" => infos.sort_by(|a, b| a.ratio.total_cmp(&b.ratio)),
        "added_on" => infos.sort_by_key(|info| info.added_on),
        "completion_on" => infos.sort_by_key(|info| info.completion_on),
        "category" => infos.sort_by(|a, b| a.category.cmp(&b.category)),
        "state" => infos.sort_by(|a, b| a.state.cmp(&b.state)),
        _ => infos.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    if reverse {
        infos.reverse();
    }
}

fn pieces_have(
    total_length: u64,
    amount_left: u64,
    complete: bool,
    piece_size: i64,
    pieces_num: i64,
) -> i64 {
    if pieces_num <= 0 || piece_size <= 0 {
        return 0;
    }
    if complete || amount_left == 0 {
        return pieces_num;
    }
    let done = total_length.saturating_sub(amount_left);
    let have = done.div_ceil(piece_size as u64) as i64;
    have.clamp(0, pieces_num)
}

fn default_torrent_properties(save_path: String) -> QbTorrentProperties {
    QbTorrentProperties {
        save_path,
        creation_date: -1,
        piece_size: 0,
        comment: String::new(),
        total_wasted: 0,
        total_uploaded: 0,
        total_uploaded_session: 0,
        total_downloaded: 0,
        total_downloaded_session: 0,
        up_limit: -1,
        dl_limit: -1,
        time_elapsed: 0,
        seeding_time: 0,
        nb_connections: 0,
        nb_connections_limit: -1,
        share_ratio: 0.0,
        addition_date: -1,
        completion_date: -1,
        created_by: String::new(),
        dl_speed_avg: 0,
        dl_speed: 0,
        eta: -1,
        last_seen: -1,
        peers: 0,
        peers_total: 0,
        pieces_have: 0,
        pieces_num: 0,
        reannounce: -1,
        seeds: 0,
        seeds_total: 0,
        total_size: 0,
        up_speed_avg: 0,
        up_speed: 0,
    }
}

async fn torrent_limit_map(
    state: &AppState,
    hashes: Option<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let requested = hashes
        .as_deref()
        .map(extract_hashes_from_str)
        .unwrap_or_default();
    let hashes = resolve_hashes(state, requested).await;
    let reg = state.registry.read().await;
    let limits = reg
        .iter()
        .filter(|entry| hashes.is_empty() || hashes.contains(&entry.info_hash))
        .map(|entry| (entry.info_hash.clone(), serde_json::json!(0)))
        .collect::<serde_json::Map<_, _>>();
    (StatusCode::OK, Json(serde_json::Value::Object(limits)))
}

async fn qbit_torrent_info(state: &AppState, e: &rt_session::TorrentEntry) -> QbTorrentInfo {
    let progress = torrent_progress(e.total_length, e.amount_left, e.completed_at.is_some());
    let (tracker, trackers_count) = if let Some(engine) = &state.engine {
        match engine.torrent_metadata(e.info_hash.clone()).await {
            Ok(meta) => (
                meta.trackers.first().cloned().unwrap_or_default(),
                meta.trackers.len() as u32,
            ),
            Err(_) => (String::new(), 0),
        }
    } else {
        (String::new(), 0)
    };
    QbTorrentInfo {
        hash: e.info_hash.clone(),
        name: e.name.clone(),
        state: to_qbit_state(e.state.as_str()).to_owned(),
        size: e.total_length as i64,
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
        progress,
        priority: 0,
        amount_left: e.amount_left as i64,
        auto_tmm: false,
        tracker,
        trackers_count,
    }
}

fn sync_rid_for_entries(entries: &[rt_session::TorrentEntry]) -> i64 {
    let mut hasher = DefaultHasher::new();
    for entry in entries {
        entry.info_hash.hash(&mut hasher);
        entry.name.hash(&mut hasher);
        entry.state.hash(&mut hasher);
        entry.total_length.hash(&mut hasher);
        entry.amount_left.hash(&mut hasher);
        entry.save_path.hash(&mut hasher);
        entry.category.hash(&mut hasher);
        entry.tags.hash(&mut hasher);
        entry.stats.downloaded.hash(&mut hasher);
        entry.stats.uploaded.hash(&mut hasher);
        entry.added_at.hash(&mut hasher);
        entry.completed_at.hash(&mut hasher);
    }
    let rid = (hasher.finish() & 0x7fff_ffff_ffff_ffff) as i64;
    rid.max(1)
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
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn split_tags(tags: &str) -> Vec<String> {
    tags.split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn split_pipe_values(values: &str) -> Vec<String> {
    values.split('|').filter_map(normalize_api_text).collect()
}

fn normalize_api_text(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

async fn update_torrent_category(
    state: &AppState,
    hash: &str,
    category: Option<String>,
) -> StatusCode {
    if let Some(engine) = &state.engine {
        let category = Some(category);
        return match engine
            .update_torrent_labels(hash.to_owned(), category, Vec::new(), Vec::new())
            .await
        {
            Ok(()) => StatusCode::OK,
            Err(_) => StatusCode::NOT_FOUND,
        };
    }
    let mut reg = state.registry.write().await;
    let Some(entry) = reg.get_mut(hash) else {
        return StatusCode::NOT_FOUND;
    };
    entry.category = category;
    StatusCode::OK
}

async fn update_torrent_tags(
    state: &AppState,
    hash: &str,
    add_tags: Vec<String>,
    remove_tags: Vec<String>,
) -> StatusCode {
    if let Some(engine) = &state.engine {
        return match engine
            .update_torrent_labels(hash.to_owned(), None, add_tags, remove_tags)
            .await
        {
            Ok(()) => StatusCode::OK,
            Err(_) => StatusCode::NOT_FOUND,
        };
    }
    let mut reg = state.registry.write().await;
    let Some(entry) = reg.get_mut(hash) else {
        return StatusCode::NOT_FOUND;
    };
    for tag in add_tags {
        if !entry.tags.contains(&tag) {
            entry.tags.push(tag);
        }
    }
    if !remove_tags.is_empty() {
        entry.tags.retain(|tag| !remove_tags.contains(tag));
    }
    StatusCode::OK
}

async fn update_torrent_fields(
    state: &AppState,
    hash: &str,
    name: Option<String>,
    save_path: Option<std::path::PathBuf>,
) -> StatusCode {
    if let Some(engine) = &state.engine {
        return match engine
            .update_torrent_fields(hash.to_owned(), name, save_path)
            .await
        {
            Ok(()) => StatusCode::OK,
            Err(_) => StatusCode::NOT_FOUND,
        };
    }
    let mut reg = state.registry.write().await;
    let Some(entry) = reg.get_mut(hash) else {
        return StatusCode::NOT_FOUND;
    };
    if let Some(name) = name.and_then(|name| normalize_api_text(&name)) {
        entry.name = name;
    }
    if let Some(save_path) = save_path {
        entry.save_path = save_path.to_string_lossy().to_string();
    }
    StatusCode::OK
}

async fn fetch_torrent_url(raw_url: &str) -> Result<Vec<u8>, String> {
    const MAX_TORRENT_BYTES: u64 = 16 * 1024 * 1024;

    let url = Url::parse(raw_url).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https torrent URLs are supported".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "torrent URL is missing a host".to_owned())?;
    reject_private_host(host, url.port_or_known_default().unwrap_or(80)).await?;

    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    if response.content_length().unwrap_or(0) > MAX_TORRENT_BYTES {
        return Err("torrent response is too large".to_owned());
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_TORRENT_BYTES {
        return Err("torrent response is too large".to_owned());
    }
    Ok(bytes.to_vec())
}

async fn reject_private_host(host: &str, port: u16) -> Result<(), String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return reject_private_ip(ip);
    }

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("DNS lookup failed: {e}"))?;
    for addr in addrs {
        reject_private_ip(addr.ip())?;
    }
    Ok(())
}

fn reject_private_ip(ip: IpAddr) -> Result<(), String> {
    let blocked = match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.segments()[0] & 0xfe00 == 0xfc00
                || ip.segments()[0] & 0xffc0 == 0xfe80
        }
    };
    if blocked {
        Err(format!("private or local address {ip} is not allowed"))
    } else {
        Ok(())
    }
}

async fn resolve_hashes(state: &AppState, hashes: Vec<String>) -> Vec<String> {
    if hashes.iter().any(|hash| hash == "all") {
        let reg = state.registry.read().await;
        reg.iter().map(|entry| entry.info_hash.clone()).collect()
    } else {
        hashes
    }
}

async fn default_save_path(state: &AppState) -> String {
    let reg = state.registry.read().await;
    let save_path = reg
        .iter()
        .next()
        .map(|entry| format!("{}/", entry.save_path.trim_end_matches('/')))
        .unwrap_or_else(|| "/downloads/".to_owned());
    save_path
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
    async fn app_preferences_and_default_save_path_ok() {
        let hash = "f".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/app/preferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["save_path"].as_str().unwrap(), "/data/");

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/app/defaultSavePath")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "/data/");
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
    async fn torrents_add_without_engine_returns_unavailable() {
        let app = build_qbit_router(AppState::new());
        let body = "--x\r\ncontent-disposition: form-data; name=\"torrents\"; filename=\"a.torrent\"\r\n\r\nnope\r\n--x--\r\n";
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/add")
                    .header("content-type", "multipart/form-data; boundary=x")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
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
    async fn torrents_info_filters_by_tag_and_sorts() {
        let state = AppState::new();
        {
            let mut reg = state.registry.write().await;
            let mut first = TorrentEntry::new("a".repeat(40), "zeta".into(), "/data".into());
            first.total_length = 10;
            first.tags = vec!["keep".into()];
            reg.add(first).unwrap();
            let mut second = TorrentEntry::new("b".repeat(40), "alpha".into(), "/data".into());
            second.total_length = 30;
            second.tags = vec!["keep".into()];
            reg.add(second).unwrap();
            let mut skipped = TorrentEntry::new("c".repeat(40), "middle".into(), "/data".into());
            skipped.total_length = 20;
            skipped.tags = vec!["skip".into()];
            reg.add(skipped).unwrap();
        }
        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info?tag=keep&sort=size&reverse=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["name"].as_str().unwrap(), "alpha");
        assert_eq!(v[1]["name"].as_str().unwrap(), "zeta");
    }

    #[tokio::test]
    async fn torrents_properties_returns_registry_projection_without_engine() {
        let hash = "d".repeat(40);
        let state = make_state_with(&hash).await;
        {
            let mut reg = state.registry.write().await;
            let entry = reg.get_mut(&hash).unwrap();
            entry.total_length = 1_000;
            entry.amount_left = 250;
            entry.stats.add_download(750);
            entry.stats.add_upload(1_500);
        }
        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/torrents/properties?hash={hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["save_path"].as_str().unwrap(), "/data/");
        assert_eq!(v["total_size"].as_i64().unwrap(), 1_000);
        assert_eq!(v["total_downloaded"].as_i64().unwrap(), 750);
        assert_eq!(v["total_uploaded"].as_i64().unwrap(), 1_500);
        assert_eq!(v["share_ratio"].as_f64().unwrap(), 2.0);
        assert_eq!(v["piece_size"].as_i64().unwrap(), 0);
        assert_eq!(v["pieces_num"].as_i64().unwrap(), 0);
    }

    #[tokio::test]
    async fn torrents_properties_missing_hash_is_bad_request() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/properties")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn torrents_categories_and_tags_project_registry_labels() {
        let hash = "e".repeat(40);
        let state = make_state_with(&hash).await;
        {
            let mut reg = state.registry.write().await;
            let entry = reg.get_mut(&hash).unwrap();
            entry.category = Some("movies".into());
            entry.tags = vec!["hd".into(), "archive".into(), "hd".into()];
        }
        let app = build_qbit_router(state);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/categories")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["movies"]["name"].as_str().unwrap(), "movies");
        assert_eq!(v["movies"]["savePath"].as_str().unwrap(), "/data/");

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!(["archive", "hd"]));
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
        assert!(v["rid"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn sync_maindata_uses_stable_rid_for_unchanged_registry() {
        let hash = "9".repeat(40);
        let app = build_qbit_router(make_state_with(&hash).await);
        let resp = app
            .clone()
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
        let first: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let rid = first["rid"].as_i64().unwrap();
        assert_eq!(first["full_update"], true);
        assert_eq!(first["torrents"].as_object().unwrap().len(), 1);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/sync/maindata?rid={rid}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let second: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(second["rid"].as_i64().unwrap(), rid);
        assert_eq!(second["full_update"], false);
        assert!(second["torrents"].as_object().unwrap().is_empty());
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

    #[tokio::test]
    async fn set_category_resolves_all_hashes() {
        let hash = "a".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&category=archive"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reg = state.registry.read().await;
        assert_eq!(reg.get(&hash).unwrap().category.as_deref(), Some("archive"));
    }

    #[tokio::test]
    async fn set_category_decodes_url_encoded_form_values() {
        let hash = "1".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&category=tv%20shows"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reg = state.registry.read().await;
        assert_eq!(
            reg.get(&hash).unwrap().category.as_deref(),
            Some("tv shows")
        );
    }

    #[tokio::test]
    async fn rename_and_set_location_update_registry_without_engine() {
        let hash = "2".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/rename")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("hash={hash}&name=better%20name")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setLocation")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&location=%2Fnew%20data"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let reg = state.registry.read().await;
        let entry = reg.get(&hash).unwrap();
        assert_eq!(entry.name, "better name");
        assert_eq!(entry.save_path, "/new data");
    }

    #[tokio::test]
    async fn category_and_global_tag_endpoints_update_registry() {
        let hash = "3".repeat(40);
        let state = make_state_with(&hash).await;
        {
            let mut reg = state.registry.write().await;
            let entry = reg.get_mut(&hash).unwrap();
            entry.category = Some("old".into());
            entry.tags = vec!["remove".into(), "keep".into()];
        }
        let app = build_qbit_router(state.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/editCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("category=old&newCategory=new"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/deleteTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("tags=remove"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/removeCategories")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("categories=new"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let reg = state.registry.read().await;
        let entry = reg.get(&hash).unwrap();
        assert_eq!(entry.category, None);
        assert_eq!(entry.tags, vec!["keep".to_owned()]);
    }

    #[tokio::test]
    async fn qbit_alias_and_broad_compat_routes_are_registered() {
        let app = build_qbit_router(AppState::new());
        for path in [
            "/api/v2/app/version",
            "/api/qb/v2/auth/logout",
            "/api/qb/v2/app/sendTestEmail",
            "/api/qb/v2/torrents/start",
            "/api/qb/v2/torrents/stop",
            "/api/qb/v2/torrents/filePrio",
            "/api/qb/v2/torrents/addTrackers",
            "/api/qb/v2/torrents/addPeers",
            "/api/qb/v2/torrents/removeTrackers",
            "/api/qb/v2/torrents/renameFile",
            "/api/qb/v2/torrents/renameFolder",
            "/api/qb/v2/torrents/setSavePath",
            "/api/qb/v2/torrents/setAutoManagement",
            "/api/qb/v2/torrents/setAutoTMM",
            "/api/qb/v2/torrents/toggleSequentialDownload",
            "/api/qb/v2/transfer/toggleSpeedLimitsMode",
            "/api/qb/v2/transfer/banPeers",
            "/api/qb/v2/search/installPlugin",
            "/api/qb/v2/search/uninstallPlugin",
            "/api/qb/v2/search/enablePlugin",
            "/api/qb/v2/search/updatePlugins",
            "/api/qb/v2/search/start",
            "/api/qb/v2/search/delete",
            "/api/qb/v2/rss/addFeed",
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from("hashes=all"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_ne!(resp.status(), StatusCode::NOT_FOUND, "{path}");
        }

        for path in [
            "/api/v2/app/webapiVersion",
            "/api/qb/v2/app/networkInterfaceList",
            "/api/qb/v2/app/networkInterfaceAddressList",
            "/api/qb/v2/torrents/webseeds",
            "/api/qb/v2/torrents/pieceStates",
            "/api/qb/v2/torrents/pieceHashes",
            "/api/qb/v2/torrents/export",
            "/api/qb/v2/torrents/downloadLimit",
            "/api/qb/v2/torrents/uploadLimit",
            "/api/qb/v2/sync/torrentPeers",
            "/api/qb/v2/log/main",
            "/api/qb/v2/log/peers",
            "/api/qb/v2/search/status",
            "/api/qb/v2/search/categories",
            "/api/qb/v2/transfer/speedLimitsMode",
            "/api/qb/v2/rss/items",
        ] {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
        }
    }

    #[tokio::test]
    async fn qbit_detail_endpoints_are_compatible_without_engine_metadata() {
        let hash = "d".repeat(40);
        let app = build_qbit_router(make_state_with(&hash).await);
        for path in [
            format!("/api/qb/v2/torrents/files?hash={hash}"),
            format!("/api/qb/v2/torrents/trackers?hash={hash}"),
        ] {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body, serde_json::json!([]));
        }
    }

    #[tokio::test]
    async fn create_tags_persists_empty_global_tags() {
        let state = AppState::new();
        let app = build_qbit_router(state);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/createTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("tags=hd,remux"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let tags: Vec<String> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tags, vec!["hd".to_owned(), "remux".to_owned()]);
    }

    #[tokio::test]
    async fn transfer_limit_endpoints_are_qbit_compatible_noops() {
        let hash = "e".repeat(40);
        let app = build_qbit_router(make_state_with(&hash).await);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/transfer/downloadLimit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "0");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/torrents/downloadLimit?hashes={hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let limits: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(limits[hash], 0);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setUploadLimit")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&limit=0"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn add_and_remove_tags_resolve_all_hashes() {
        let hash = "b".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/addTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&tags=hd,archive"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/removeTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&tags=hd"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let reg = state.registry.read().await;
        assert_eq!(reg.get(&hash).unwrap().tags, vec!["archive".to_owned()]);
    }

    #[tokio::test]
    async fn reannounce_accepts_all_hashes_without_engine() {
        let hash = "c".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/reannounce")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn ssrf_guard_rejects_private_and_local_ips() {
        assert!(reject_private_ip("127.0.0.1".parse().unwrap()).is_err());
        assert!(reject_private_ip("10.0.0.1".parse().unwrap()).is_err());
        assert!(reject_private_ip("172.16.0.1".parse().unwrap()).is_err());
        assert!(reject_private_ip("192.168.1.1".parse().unwrap()).is_err());
        assert!(reject_private_ip("169.254.1.1".parse().unwrap()).is_err());
        assert!(reject_private_ip("::1".parse().unwrap()).is_err());
        assert!(reject_private_ip("fc00::1".parse().unwrap()).is_err());
        assert!(reject_private_ip("fe80::1".parse().unwrap()).is_err());
        assert!(reject_private_ip("8.8.8.8".parse().unwrap()).is_ok());
    }

    #[test]
    fn pieces_have_is_bounded() {
        assert_eq!(pieces_have(1_000, 250, false, 100, 10), 8);
        assert_eq!(pieces_have(1_000, 0, false, 100, 10), 10);
        assert_eq!(pieces_have(1_000, 250, false, 0, 10), 0);
        assert_eq!(pieces_have(1_000, 250, false, 100, 0), 0);
    }

    #[test]
    fn parse_form_body_decodes_qbit_forms() {
        let params = parse_form_body("hashes=a%7Cb&tags=high+quality%2Carchive");
        assert_eq!(params.get("hashes").unwrap(), "a|b");
        assert_eq!(params.get("tags").unwrap(), "high quality,archive");
    }
}
