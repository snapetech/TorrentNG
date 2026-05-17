use axum::{
    extract::{Multipart, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use rt_metainfo::{parse_magnet, parse_torrent};
use serde::{
    ser::{SerializeMap, Serializer},
    Deserialize, Serialize,
};
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    net::{IpAddr, SocketAddr},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

use rt_engine::{
    EngineGlobalLimits, EnginePeerSnapshot, EnginePieceState, EngineTorrentLimits, QueueMove,
};
use rt_metrics::{MemoryClass, MemoryLease};

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
            "libtorrent": "TorrentNG-native",
            "boost": "",
            "openssl": "",
            "bitness": 64,
        })),
    )
}

pub async fn app_preferences(State(state): State<AppState>) -> impl IntoResponse {
    let save_path = default_save_path(&state).await;
    let mut preferences = qbit_preferences(save_path);
    if let Some(map) = preferences.as_object_mut() {
        for (key, value) in state.preference_overrides.read().await.iter() {
            map.insert(key.clone(), value.clone());
        }
    }
    (StatusCode::OK, Json(preferences))
}

pub async fn app_set_preferences(State(state): State<AppState>, body: String) -> impl IntoResponse {
    match qbit_preference_payload(&body) {
        Some(serde_json::Value::Object(updates)) => {
            state.preference_overrides.write().await.extend(updates);
            StatusCode::OK
        }
        Some(_) => StatusCode::BAD_REQUEST,
        None => StatusCode::BAD_REQUEST,
    }
}

pub async fn app_shutdown() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn app_send_test_email() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn app_get_cookies() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!([])))
}

pub async fn app_set_cookies() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn app_rotate_api_key() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "apiKey": "",
        })),
    )
}

pub async fn app_delete_api_key() -> impl IntoResponse {
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

fn qbit_preferences(save_path: String) -> serde_json::Value {
    serde_json::json!({
        "locale": "en",
        "create_subfolder_enabled": false,
        "start_paused_enabled": false,
        "auto_delete_mode": 0,
        "preallocate_all": false,
        "incomplete_files_ext": false,
        "auto_tmm_enabled": false,
        "torrent_changed_tmm_enabled": false,
        "save_path_changed_tmm_enabled": false,
        "category_changed_tmm_enabled": false,
        "save_path": save_path,
        "temp_path_enabled": false,
        "temp_path": "",
        "scan_dirs": {},
        "download_in_scan_dirs": {},
        "export_dir_enabled": false,
        "export_dir": "",
        "export_dir_fin": "",
        "mail_notification_enabled": false,
        "mail_notification_sender": "",
        "mail_notification_email": "",
        "mail_notification_smtp": "",
        "mail_notification_ssl_enabled": false,
        "mail_notification_auth_enabled": false,
        "mail_notification_username": "",
        "mail_notification_password": "",
        "autorun_enabled": false,
        "autorun_program": "",
        "queueing_enabled": false,
        "max_active_downloads": -1,
        "max_active_torrents": -1,
        "max_active_uploads": -1,
        "dont_count_slow_torrents": false,
        "slow_torrent_dl_rate_threshold": 2,
        "slow_torrent_ul_rate_threshold": 2,
        "slow_torrent_inactive_timer": 60,
        "max_ratio_enabled": false,
        "max_ratio": -1.0,
        "max_ratio_act": 0,
        "max_seeding_time_enabled": false,
        "max_seeding_time": -1,
        "listen_port": 0,
        "upnp": false,
        "random_port": false,
        "dl_limit": 0,
        "up_limit": 0,
        "max_connec": -1,
        "max_connec_per_torrent": -1,
        "max_uploads": -1,
        "max_uploads_per_torrent": -1,
        "stop_tracker_timeout": 1,
        "enable_piece_extent_affinity": false,
        "bittorrent_protocol": 0,
        "limit_utp_rate": false,
        "limit_tcp_overhead": false,
        "limit_lan_peers": false,
        "alt_dl_limit": 0,
        "alt_up_limit": 0,
        "scheduler_enabled": false,
        "schedule_from_hour": 8,
        "schedule_from_min": 0,
        "schedule_to_hour": 20,
        "schedule_to_min": 0,
        "scheduler_days": 0,
        "dht": true,
        "dhtSameAsBT": true,
        "dht_port": 0,
        "pex": true,
        "lsd": false,
        "encryption": 0,
        "anonymous_mode": false,
        "proxy_type": -1,
        "proxy_ip": "",
        "proxy_port": 0,
        "proxy_peer_connections": false,
        "proxy_auth_enabled": false,
        "proxy_username": "",
        "proxy_password": "",
        "proxy_torrents_only": false,
        "ip_filter_enabled": false,
        "ip_filter_path": "",
        "ip_filter_trackers": false,
        "web_ui_domain_list": "*",
        "web_ui_address": "0.0.0.0",
        "web_ui_port": 8080,
        "web_ui_upnp": false,
        "web_ui_username": "admin",
        "web_ui_password": "",
        "web_ui_csrf_protection_enabled": true,
        "web_ui_clickjacking_protection_enabled": true,
        "web_ui_secure_cookie_enabled": false,
        "web_ui_max_auth_fail_count": 5,
        "web_ui_ban_duration": 3600,
        "web_ui_session_timeout": 3600,
        "web_ui_host_header_validation_enabled": false,
        "bypass_local_auth": false,
        "bypass_auth_subnet_whitelist_enabled": false,
        "bypass_auth_subnet_whitelist": "",
        "alternative_webui_enabled": false,
        "alternative_webui_path": "",
        "use_https": false,
        "ssl_key": "",
        "ssl_cert": "",
        "web_ui_https_key_path": "",
        "web_ui_https_cert_path": "",
        "dyndns_enabled": false,
        "dyndns_service": 0,
        "dyndns_username": "",
        "dyndns_password": "",
        "dyndns_domain": "",
        "rss_refresh_interval": 30,
        "rss_max_articles_per_feed": 50,
        "rss_processing_enabled": false,
        "rss_auto_downloading_enabled": false,
        "rss_download_repack_proper_episodes": true,
        "rss_smart_episode_filters": "",
        "add_trackers_enabled": false,
        "add_trackers": "",
        "web_ui_use_custom_http_headers_enabled": false,
        "web_ui_custom_http_headers": "",
        "announce_to_all_tiers": true,
        "announce_to_all_trackers": false,
        "async_io_threads": 10,
        "banned_ips": "",
        "checking_memory_use": 32,
        "current_interface_address": "",
        "current_network_interface": "",
        "disk_cache": -1,
        "disk_cache_ttl": 60,
        "embedded_tracker_port": 0,
        "enable_coalesce_read_write": false,
        "enable_embedded_tracker": false,
        "enable_multi_connections_from_same_ip": false,
        "enable_os_cache": true,
        "enable_upload_suggestions": false,
        "file_pool_size": 5000,
        "outgoing_ports_max": 0,
        "outgoing_ports_min": 0,
        "recheck_completed_torrents": false,
        "resolve_peer_countries": false,
        "save_resume_data_interval": 60,
        "send_buffer_low_watermark": 10,
        "send_buffer_watermark": 500,
        "send_buffer_watermark_factor": 50,
        "socket_backlog_size": 30,
        "upload_choking_algorithm": 1,
        "upload_slots_behavior": 0,
        "upnp_lease_duration": 0,
        "utp_tcp_mixed_mode": 0,
    })
}

fn qbit_preference_payload(body: &str) -> Option<serde_json::Value> {
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).ok();
    }
    parse_form_body(trimmed)
        .remove("json")
        .and_then(|json| serde_json::from_str(&json).ok())
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
    let torrent_count = state.registry.read().await.len();
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(
            &state,
            estimate_qbit_torrent_info_snapshot_bytes(torrent_count),
        )
        .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) => return qbit_api_snapshot_budget_exhausted(),
            Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
        }
    } else {
        None
    };
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
    let mut entries = entries;
    sort_torrent_entries(&mut entries, q.sort.as_deref(), q.reverse.unwrap_or(false));

    let offset = q.offset.unwrap_or(0);
    let entries = if offset < entries.len() {
        &entries[offset..]
    } else {
        &[]
    };
    let entries: Vec<_> = if let Some(limit) = q.limit {
        entries.iter().take(limit).cloned().collect()
    } else {
        entries.to_vec()
    };
    let mut infos = Vec::with_capacity(entries.len());
    for entry in &entries {
        infos.push(qbit_torrent_info(&state, entry).await);
    }

    (StatusCode::OK, Json(infos)).into_response()
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
    let mut added_url_torrent = false;

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
                    tracing::error!(
                        component = "api",
                        operation = "add_magnet",
                        source = "magnet:redacted",
                        error = %e,
                        "qBit magnet parse failed"
                    );
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
                tracing::error!(
                    component = "api",
                    operation = "add_magnet",
                    error = %e,
                    "qBit magnet add failed"
                );
                return (StatusCode::BAD_REQUEST, "Fails.").into_response();
            }
            added_url_torrent = true;
            continue;
        }
        match fetch_torrent_url(url).await {
            Ok(raw) => torrent_blobs.push(raw),
            Err(e) => {
                tracing::error!(
                    component = "api",
                    operation = "add_torrent_url",
                    source = %redact_log_url(url),
                    error = %e,
                    "qBit torrent URL fetch failed"
                );
                return (StatusCode::BAD_REQUEST, "Fails.").into_response();
            }
        }
    }

    if torrent_blobs.is_empty() {
        if added_url_torrent {
            return (StatusCode::OK, "Ok.").into_response();
        }
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
                tracing::error!(
                    component = "api",
                    operation = "add_torrent",
                    error = %e,
                    "qBit torrent parse failed"
                );
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
            tracing::error!(
                component = "api",
                operation = "add_torrent",
                error = %e,
                "qBit torrent add failed"
            );
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
pub async fn torrents_file_prio(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").cloned() else {
        return StatusCode::BAD_REQUEST;
    };
    let file_ids = params
        .get("id")
        .or_else(|| params.get("ids"))
        .map(|ids| {
            ids.split('|')
                .filter_map(|id| id.trim().parse::<u32>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(priority) = params
        .get("priority")
        .and_then(|value| value.parse::<i64>().ok())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(engine) = &state.engine else {
        return StatusCode::OK;
    };
    match engine
        .update_file_priorities(hash, file_ids, priority)
        .await
    {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

/// `POST /api/qb/v2/torrents/increasePrio`.
pub async fn torrents_increase_prio(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_queue_order(&state, &body, QueueMove::Up).await
}

/// `POST /api/qb/v2/torrents/decreasePrio`.
pub async fn torrents_decrease_prio(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_queue_order(&state, &body, QueueMove::Down).await
}

/// `POST /api/qb/v2/torrents/topPrio`.
pub async fn torrents_top_prio(State(state): State<AppState>, body: String) -> impl IntoResponse {
    update_queue_order(&state, &body, QueueMove::Top).await
}

/// `POST /api/qb/v2/torrents/bottomPrio`.
pub async fn torrents_bottom_prio(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_queue_order(&state, &body, QueueMove::Bottom).await
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
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_tracker_snapshot_bytes(meta.trackers.len()),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(Vec::<QbTrackerInfo>::new()),
                    )
                }
            };
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
pub async fn torrents_add_trackers(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").cloned() else {
        return StatusCode::BAD_REQUEST;
    };
    let urls = params
        .get("urls")
        .map(|urls| split_tracker_values(urls))
        .unwrap_or_default();
    if urls.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    let mut trackers = current_tracker_urls(&state, &hash).await;
    for url in urls {
        if !trackers.contains(&url) {
            trackers.push(url);
        }
    }
    update_torrent_trackers(&state, &hash, trackers).await
}

/// `POST /api/qb/v2/torrents/editTracker`.
pub async fn torrents_edit_tracker(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").cloned() else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(original_url) = params
        .get("origUrl")
        .and_then(|url| normalize_api_text(url))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_url) = params.get("newUrl").and_then(|url| normalize_api_text(url)) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut trackers = current_tracker_urls(&state, &hash).await;
    let Some(slot) = trackers.iter_mut().find(|url| **url == original_url) else {
        return StatusCode::NOT_FOUND;
    };
    *slot = new_url;
    trackers = normalize_tracker_values(trackers);
    update_torrent_trackers(&state, &hash, trackers).await
}

/// `POST /api/qb/v2/torrents/removeTrackers`.
pub async fn torrents_remove_trackers(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").cloned() else {
        return StatusCode::BAD_REQUEST;
    };
    let remove = params
        .get("urls")
        .map(|urls| split_tracker_values(urls))
        .unwrap_or_default();
    if remove.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    let trackers = current_tracker_urls(&state, &hash)
        .await
        .into_iter()
        .filter(|url| !remove.contains(url))
        .collect::<Vec<_>>();
    update_torrent_trackers(&state, &hash, trackers).await
}

/// `POST /api/qb/v2/torrents/addPeers`.
pub async fn torrents_add_peers(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .or_else(|| params.get("hash"))
        .map(|hashes| {
            hashes
                .split('|')
                .filter_map(normalize_api_text)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let peers = params
        .get("peers")
        .map(|peers| parse_peer_addrs(peers))
        .unwrap_or_default();
    if hashes.is_empty() || peers.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    let Some(engine) = &state.engine else {
        return StatusCode::OK;
    };
    for hash in hashes {
        if engine.add_peers(hash, peers.clone()).await.is_err() {
            return StatusCode::NOT_FOUND;
        }
    }
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
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_metadata_snapshot_bytes(
                    meta.files.len(),
                    meta.piece_count as usize,
                    meta.webseeds.len(),
                ),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(Vec::<QbFileInfo>::new()),
                    )
                }
            };
            let files = meta
                .files
                .into_iter()
                .map(|file| QbFileInfo {
                    index: file.index,
                    name: file.path,
                    size: file.length as i64,
                    priority: file.priority.clamp(0, 2) as u8,
                    progress: if complete || !file.wanted { 1.0 } else { 0.0 },
                })
                .collect();
            (StatusCode::OK, Json(files))
        }
        Err(_) if exists => (StatusCode::OK, Json(Vec::<QbFileInfo>::new())),
        Err(_) => (StatusCode::NOT_FOUND, Json(Vec::<QbFileInfo>::new())),
    }
}

/// `GET /api/qb/v2/torrents/webseeds`.
pub async fn torrents_webseeds(
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
        Ok(meta) => {
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_metadata_snapshot_bytes(0, 0, meta.webseeds.len()),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new()))
                }
            };
            (StatusCode::OK, Json(meta.webseeds))
        }
        Err(_) if exists => (StatusCode::OK, Json(Vec::<String>::new())),
        Err(_) => (StatusCode::NOT_FOUND, Json(Vec::<String>::new())),
    }
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
    let Some(_) = entry else {
        return (StatusCode::NOT_FOUND, Json(Vec::<i32>::new()));
    };
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(Vec::<i32>::new()));
    };
    match engine.torrent_metadata(hash).await {
        Ok(meta) => {
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_metadata_snapshot_bytes(0, meta.piece_states.len(), 0),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<i32>::new()))
                }
            };
            let states = meta
                .piece_states
                .into_iter()
                .map(|state| match state {
                    EnginePieceState::Missing => 0,
                    EnginePieceState::Partial => 1,
                    EnginePieceState::Complete => 2,
                })
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
        Ok(meta) => {
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_metadata_snapshot_bytes(0, meta.piece_hashes.len(), 0),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new()))
                }
            };
            (StatusCode::OK, Json(meta.piece_hashes))
        }
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
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(&state, estimate_qbit_properties_snapshot_bytes()).await {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(default_torrent_properties(String::new())),
                )
            }
        }
    } else {
        None
    };

    let (piece_size, pieces_num) = if let Some(engine) = &state.engine {
        match engine.torrent_metadata(hash.clone()).await {
            Ok(meta) => (meta.piece_length as i64, meta.piece_count as i64),
            Err(_) => (0, 0),
        }
    } else {
        (0, 0)
    };

    let limits = get_torrent_limits(&state, &hash).await;
    let now = unix_now();
    let time_elapsed = now.saturating_sub(entry.added_at as i64);
    let seeding_time = entry
        .completed_at
        .map(|completed| now.saturating_sub(completed as i64))
        .unwrap_or(0);
    let dl_speed = 0;
    let up_speed = 0;
    let eta = if entry.amount_left == 0 {
        0
    } else if dl_speed > 0 {
        (entry.amount_left as i64 / dl_speed).max(0)
    } else {
        -1
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
        up_limit: limits.upload_limit.unwrap_or(-1),
        dl_limit: limits.download_limit.unwrap_or(-1),
        time_elapsed,
        seeding_time,
        nb_connections: 0,
        nb_connections_limit: limits.max_connections.unwrap_or(-1),
        share_ratio: entry.stats.ratio(),
        addition_date: entry.added_at as i64,
        completion_date: entry.completed_at.map(|t| t as i64).unwrap_or(-1),
        created_by: String::new(),
        dl_speed_avg: 0,
        dl_speed,
        eta,
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
        up_speed,
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
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(
            &state,
            estimate_qbit_label_snapshot_bytes(categories.len()),
        )
        .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::Value::Object(serde_json::Map::new())),
                )
            }
        }
    } else {
        None
    };
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
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(&state, estimate_qbit_label_snapshot_bytes(tags.len()))
            .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::new())),
        }
    } else {
        None
    };
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
pub async fn torrents_rename_file(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").and_then(|hash| normalize_api_text(hash)) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(file_id) = params
        .get("id")
        .or_else(|| params.get("file_id"))
        .and_then(|id| id.parse::<u32>().ok())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(name) = params
        .get("name")
        .or_else(|| params.get("newName"))
        .and_then(|name| normalize_api_text(name))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(engine) = &state.engine else {
        return StatusCode::OK;
    };
    match engine.rename_file_path(hash, file_id, name).await {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

/// `POST /api/qb/v2/torrents/renameFolder`.
pub async fn torrents_rename_folder(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").and_then(|hash| normalize_api_text(hash)) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(old_path) = params
        .get("oldPath")
        .or_else(|| params.get("old_path"))
        .and_then(|path| normalize_api_text(path))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_path) = params
        .get("newPath")
        .or_else(|| params.get("new_path"))
        .and_then(|path| normalize_api_text(path))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(engine) = &state.engine else {
        return StatusCode::OK;
    };
    match engine.rename_folder_path(hash, old_path, new_path).await {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::NOT_FOUND,
    }
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
    let category_save_path = if category.trim().is_empty() {
        None
    } else {
        state.categories.read().await.get(&category).cloned()
    };
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
            if let Some(save_path) = &category_save_path {
                let _ = engine
                    .update_torrent_fields(
                        hash.clone(),
                        None,
                        Some(std::path::PathBuf::from(save_path)),
                    )
                    .await;
            }
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
                if let Some(save_path) = &category_save_path {
                    e.save_path = save_path.clone();
                }
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
    let still_used = {
        let reg = state.registry.read().await;
        remove_tags
            .iter()
            .filter(|tag| {
                reg.iter()
                    .any(|entry| entry.tags.iter().any(|used| used == *tag))
            })
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    };
    let mut global = state.tags.write().await;
    for tag in remove_tags {
        if !still_used.contains(&tag) {
            global.remove(&tag);
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setDownloadLimit`.
pub async fn torrents_set_download_limit(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_limit_field(State(state), body, LimitField::Download).await
}

/// `POST /api/qb/v2/torrents/setUploadLimit`.
pub async fn torrents_set_upload_limit(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_limit_field(State(state), body, LimitField::Upload).await
}

/// `GET /api/qb/v2/torrents/downloadLimit`.
pub async fn torrents_download_limit(
    State(state): State<AppState>,
    Query(q): Query<HashesQuery>,
) -> impl IntoResponse {
    torrent_limit_map(&state, q.hashes, LimitField::Download).await
}

/// `GET /api/qb/v2/torrents/uploadLimit`.
pub async fn torrents_upload_limit(
    State(state): State<AppState>,
    Query(q): Query<HashesQuery>,
) -> impl IntoResponse {
    torrent_limit_map(&state, q.hashes, LimitField::Upload).await
}

/// `POST /api/qb/v2/torrents/setShareLimits`.
pub async fn torrents_set_share_limits(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let hashes = resolve_hashes(&state, hashes).await;
    let ratio = params
        .get("ratioLimit")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value >= 0.0);
    let seeding_time = params
        .get("seedingTimeLimit")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0);
    for hash in hashes {
        let mut limits = get_torrent_limits(&state, &hash).await;
        limits.seed_ratio_limit = ratio;
        limits.seed_idle_limit = seeding_time;
        if update_torrent_limits(&state, &hash, limits).await != StatusCode::OK {
            return StatusCode::NOT_FOUND;
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setForceStart`.
pub async fn torrents_set_force_start(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::ForceStart).await
}

/// `POST /api/qb/v2/torrents/setSuperSeeding`.
pub async fn torrents_set_super_seeding(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::SuperSeeding).await
}

/// `POST /api/qb/v2/torrents/setAutoTMM`.
pub async fn torrents_set_auto_tmm(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::AutoTmm).await
}

/// `POST /api/qb/v2/torrents/setAutoManagement`.
pub async fn torrents_set_auto_management(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::AutoManagement).await
}

/// `POST /api/qb/v2/torrents/toggleSequentialDownload`.
pub async fn torrents_toggle_sequential_download(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::Sequential).await
}

/// `POST /api/qb/v2/torrents/toggleFirstLastPiecePrio`.
pub async fn torrents_toggle_first_last_piece_prio(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::FirstLast).await
}

/// `POST /api/qb/v2/transfer/setDownloadLimit`.
pub async fn transfer_set_download_limit(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_global_limit(&state, &body, LimitField::Download).await
}

/// `POST /api/qb/v2/transfer/setUploadLimit`.
pub async fn transfer_set_upload_limit(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_global_limit(&state, &body, LimitField::Upload).await
}

pub async fn transfer_speed_limits_mode(State(state): State<AppState>) -> impl IntoResponse {
    let mode = global_limits(&state).await.speed_limits_mode;
    (StatusCode::OK, if mode { "1" } else { "0" })
}

pub async fn transfer_toggle_speed_limits_mode(State(state): State<AppState>) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return StatusCode::OK;
    };
    let mut limits = global_limits(&state).await;
    limits.speed_limits_mode = !limits.speed_limits_mode;
    match engine.update_global_limits(limits).await {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// `GET /api/qb/v2/transfer/downloadLimit`.
pub async fn transfer_download_limit(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        global_limits(&state).await.download_limit.to_string(),
    )
}

/// `GET /api/qb/v2/transfer/uploadLimit`.
pub async fn transfer_upload_limit(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        global_limits(&state).await.upload_limit.to_string(),
    )
}

// ---------------------------------------------------------------------------
// Sync & Transfer
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SyncMaindataQuery {
    pub rid: Option<i64>,
}

#[derive(Debug, Serialize)]
struct SyncMaindataResponse {
    rid: i64,
    full_update: bool,
    torrents: SyncTorrentMap,
    torrents_removed: &'static [&'static str],
    server_state: QbServerState,
}

#[derive(Debug)]
struct SyncTorrentMap {
    infos: Vec<QbTorrentInfo>,
    full_update: bool,
}

impl Serialize for SyncTorrentMap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let len = if self.full_update {
            Some(self.infos.len())
        } else {
            Some(0)
        };
        let mut map = serializer.serialize_map(len)?;
        if self.full_update {
            for info in &self.infos {
                map.serialize_entry(&info.hash, info)?;
            }
        }
        map.end()
    }
}

pub async fn sync_maindata(
    State(state): State<AppState>,
    Query(q): Query<SyncMaindataQuery>,
) -> impl IntoResponse {
    let torrent_count = state.registry.read().await.len();
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(
            &state,
            estimate_qbit_maindata_snapshot_bytes(torrent_count),
        )
        .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) => return qbit_api_snapshot_budget_exhausted(),
            Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
        }
    } else {
        None
    };
    let entries = {
        let reg = state.registry.read().await;
        reg.iter().cloned().collect::<Vec<_>>()
    };
    let mut infos = Vec::with_capacity(entries.len());
    for entry in &entries {
        infos.push(qbit_torrent_info(&state, entry).await);
    }
    let rid = sync_rid_for_infos(&infos);
    let full_update = q.rid.unwrap_or(0) != rid;
    let (alltime_dl, alltime_ul) = infos.iter().fold((0_i64, 0_i64), |(dl, ul), info| {
        (
            dl.saturating_add(info.downloaded),
            ul.saturating_add(info.uploaded),
        )
    });
    let global_ratio = if alltime_dl > 0 {
        alltime_ul as f64 / alltime_dl as f64
    } else {
        0.0
    };
    let limits = global_limits(&state).await;
    let resp = SyncMaindataResponse {
        rid,
        full_update,
        torrents: SyncTorrentMap { infos, full_update },
        torrents_removed: &[],
        server_state: QbServerState {
            dl_info_speed: 0,
            dl_info_data: 0,
            up_info_speed: 0,
            up_info_data: 0,
            alltime_dl,
            alltime_ul,
            average_time_queue: 0,
            connection_status: "connected".into(),
            free_space_on_disk: 0,
            global_ratio,
            queued_io_jobs: 0,
            queueing: false,
            read_cache_hits: "0".into(),
            read_cache_overload: "0".into(),
            refresh_interval: 1500,
            total_buffers_size: 0,
            total_peer_connections: 0,
            total_queued_size: 0,
            total_wasted_session: 0,
            dl_rate_limit: limits.download_limit,
            up_rate_limit: limits.upload_limit,
            use_alt_speed_limits: limits.speed_limits_mode,
            write_cache_overload: "0".into(),
        },
    };
    (StatusCode::OK, Json(resp)).into_response()
}

pub async fn sync_torrent_peers(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(hash) = q.get("hash").cloned() else {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "rid": 1,
                "full_update": true,
                "peers": {},
                "peers_removed": [],
                "show_flags": true,
            })),
        );
    };
    let peers = if let Some(engine) = &state.engine {
        engine.torrent_peers(hash).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(&state, estimate_qbit_peer_snapshot_bytes(peers.len()))
            .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "rid": 1,
                        "full_update": true,
                        "peers": {},
                        "peers_removed": [],
                        "show_flags": true,
                    })),
                )
            }
        }
    } else {
        None
    };
    let rid = qbit_peer_rid(&peers);
    let full_update = q
        .get("rid")
        .and_then(|rid| rid.parse::<i64>().ok())
        .is_none_or(|requested| requested != rid);
    let peer_map = if full_update {
        qbit_peer_map(&peers)
    } else {
        serde_json::Map::new()
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "rid": rid,
            "full_update": full_update,
            "peers": peer_map,
            "peers_removed": [],
            "show_flags": true,
        })),
    )
}

fn qbit_peer_rid(peers: &[EnginePeerSnapshot]) -> i64 {
    let mut hasher = DefaultHasher::new();
    let mut peers = peers.iter().collect::<Vec<_>>();
    peers.sort_by_key(|peer| peer.addr);
    for peer in peers {
        peer.addr.hash(&mut hasher);
        peer.client.hash(&mut hasher);
        peer.download_rate.hash(&mut hasher);
        peer.upload_rate.hash(&mut hasher);
        peer.downloaded.hash(&mut hasher);
        peer.uploaded.hash(&mut hasher);
        ((peer.progress * 1_000_000.0).round() as i64).hash(&mut hasher);
    }
    (hasher.finish() & 0x7fff_ffff) as i64 + 1
}

fn qbit_peer_map(peers: &[EnginePeerSnapshot]) -> serde_json::Map<String, serde_json::Value> {
    peers
        .iter()
        .map(|peer| {
            let key = peer.addr.to_string();
            let value = serde_json::json!({
                "client": peer.client,
                "connection": "BT",
                "country": "",
                "country_code": "",
                "dl_speed": peer.download_rate,
                "downloaded": peer.downloaded,
                "files": "",
                "flags": "",
                "flags_desc": "",
                "ip": peer.addr.ip().to_string(),
                "peer_id_client": peer.client,
                "port": peer.addr.port(),
                "progress": peer.progress,
                "relevance": peer.progress,
                "up_speed": peer.upload_rate,
                "uploaded": peer.uploaded,
            });
            (key, value)
        })
        .collect()
}

pub async fn transfer_info(State(state): State<AppState>) -> impl IntoResponse {
    let limits = global_limits(&state).await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "dl_info_speed": 0,
            "dl_info_data": 0,
            "up_info_speed": 0,
            "up_info_data": 0,
            "connection_status": "connected",
            "free_space_on_disk": 0,
            "dl_rate_limit": limits.download_limit,
            "up_rate_limit": limits.upload_limit,
            "use_alt_speed_limits": limits.speed_limits_mode,
        })),
    )
}

pub async fn transfer_ban_peers() -> impl IntoResponse {
    StatusCode::OK
}

#[derive(Debug, Deserialize)]
pub struct LogMainQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    normal: Option<bool>,
    #[serde(default)]
    info: Option<bool>,
    #[serde(default)]
    warning: Option<bool>,
    #[serde(default)]
    critical: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct QbLogEntry {
    id: i64,
    message: String,
    timestamp: i64,
    #[serde(rename = "type")]
    kind: i64,
}

pub async fn log_main(
    State(state): State<AppState>,
    Query(query): Query<LogMainQuery>,
) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(Vec::<QbLogEntry>::new()));
    };
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let levels = query.included_levels();
    match engine
        .session_events_filtered(None, None, levels, limit)
        .await
    {
        Ok(events) => (
            StatusCode::OK,
            Json(
                events
                    .into_iter()
                    .map(qbit_log_entry)
                    .filter(|entry| query.includes_type(entry.kind))
                    .collect(),
            ),
        ),
        Err(e) => {
            tracing::warn!(component = "api", operation = "log_main", error = %e, "failed to read session events");
            (StatusCode::OK, Json(Vec::<QbLogEntry>::new()))
        }
    }
}

impl LogMainQuery {
    fn includes_type(&self, kind: i64) -> bool {
        let any_filter = self.normal.is_some()
            || self.info.is_some()
            || self.warning.is_some()
            || self.critical.is_some();
        if !any_filter {
            return true;
        }
        match kind {
            1 => self.normal.unwrap_or(false) || self.info.unwrap_or(false),
            2 => self.warning.unwrap_or(false),
            4 => self.critical.unwrap_or(false),
            _ => true,
        }
    }

    fn included_levels(&self) -> Vec<String> {
        let any_filter = self.normal.is_some()
            || self.info.is_some()
            || self.warning.is_some()
            || self.critical.is_some();
        if !any_filter {
            return Vec::new();
        }

        let mut levels = Vec::new();
        if self.normal.unwrap_or(false) || self.info.unwrap_or(false) {
            levels.push("info".to_owned());
        }
        if self.warning.unwrap_or(false) {
            levels.push("warn".to_owned());
            levels.push("warning".to_owned());
        }
        if self.critical.unwrap_or(false) {
            levels.push("error".to_owned());
            levels.push("critical".to_owned());
        }
        levels
    }
}

fn qbit_log_entry(row: rt_db::SessionEventRow) -> QbLogEntry {
    let message = row.message.unwrap_or_else(|| row.kind.clone());
    QbLogEntry {
        id: row.event_id.unwrap_or_default(),
        message,
        timestamp: row.occurred_at,
        kind: qbit_log_type(&row.kind, &row.payload),
    }
}

fn qbit_log_type(kind: &str, payload: &str) -> i64 {
    let lower_kind = kind.to_ascii_lowercase();
    if lower_kind.contains("error") || lower_kind.contains("failed") {
        return 4;
    }
    if lower_kind.contains("warn") {
        return 2;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return 1;
    };
    match value
        .get("level")
        .and_then(|v| v.as_str())
        .map(str::to_ascii_lowercase)
    {
        Some(level) if level == "error" || level == "critical" => 4,
        Some(level) if level == "warn" || level == "warning" => 2,
        _ => 1,
    }
}

pub async fn log_peers(State(state): State<AppState>) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(Vec::<serde_json::Value>::new()));
    };
    let hashes = {
        let reg = state.registry.read().await;
        reg.iter()
            .map(|entry| entry.info_hash.clone())
            .collect::<Vec<_>>()
    };
    let mut peer_snapshots = Vec::new();
    for hash in hashes {
        let peers = engine.torrent_peers(hash.clone()).await.unwrap_or_default();
        peer_snapshots.push((hash, peers));
    }
    let peer_count = peer_snapshots
        .iter()
        .map(|(_, peers)| peers.len())
        .sum::<usize>();
    let _lease = match reserve_qbit_api_snapshot(
        &state,
        estimate_qbit_peer_snapshot_bytes(peer_count),
    )
    .await
    {
        Ok(Some(lease)) => Some(lease),
        Ok(None) | Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Vec::<serde_json::Value>::new()),
            )
        }
    };
    let mut entries = Vec::with_capacity(peer_count);
    for (hash, peers) in peer_snapshots {
        entries.extend(
            peers
                .into_iter()
                .map(|peer| qbit_peer_log_entry(&hash, &peer)),
        );
    }
    (StatusCode::OK, Json(entries))
}

fn qbit_peer_log_entry(info_hash: &str, peer: &EnginePeerSnapshot) -> serde_json::Value {
    serde_json::json!({
        "torrent": info_hash,
        "ip": peer.addr.ip().to_string(),
        "port": peer.addr.port(),
        "client": peer.client,
        "connection": "BT",
        "progress": peer.progress,
        "dl_speed": peer.download_rate,
        "up_speed": peer.upload_rate,
        "downloaded": peer.downloaded,
        "uploaded": peer.uploaded,
    })
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

fn sort_torrent_entries(
    entries: &mut [rt_session::TorrentEntry],
    sort: Option<&str>,
    reverse: bool,
) {
    match sort.unwrap_or_default() {
        "name" => entries.sort_by(|a, b| a.name.cmp(&b.name)),
        "size" => entries.sort_by_key(|entry| entry.total_length),
        "progress" => entries.sort_by(|a, b| {
            torrent_progress(a.total_length, a.amount_left, a.completed_at.is_some()).total_cmp(
                &torrent_progress(b.total_length, b.amount_left, b.completed_at.is_some()),
            )
        }),
        "ratio" => entries.sort_by(|a, b| a.stats.ratio().total_cmp(&b.stats.ratio())),
        "added_on" => entries.sort_by_key(|entry| entry.added_at),
        "completion_on" => entries.sort_by_key(|entry| entry.completed_at.unwrap_or(0)),
        "category" => entries.sort_by(|a, b| a.category.cmp(&b.category)),
        "state" => entries
            .sort_by(|a, b| to_qbit_state(a.state.as_str()).cmp(to_qbit_state(b.state.as_str()))),
        "dlspeed" | "upspeed" => entries.sort_by(|a, b| a.info_hash.cmp(&b.info_hash)),
        _ => entries.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    if reverse {
        entries.reverse();
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
    field: LimitField,
) -> (StatusCode, Json<serde_json::Value>) {
    let requested = hashes
        .as_deref()
        .map(extract_hashes_from_str)
        .unwrap_or_default();
    let hashes = resolve_hashes(state, requested).await;
    let reg = state.registry.read().await;
    let entries = reg
        .iter()
        .filter(|entry| hashes.is_empty() || hashes.contains(&entry.info_hash))
        .map(|entry| entry.info_hash.clone())
        .collect::<Vec<_>>();
    drop(reg);
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(
            state,
            estimate_qbit_limit_map_snapshot_bytes(entries.len()),
        )
        .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::Value::Object(serde_json::Map::new())),
                )
            }
        }
    } else {
        None
    };
    let mut limits = serde_json::Map::new();
    for hash in entries {
        let value = match field {
            LimitField::Download => get_torrent_limits(state, &hash)
                .await
                .download_limit
                .unwrap_or(0),
            LimitField::Upload => get_torrent_limits(state, &hash)
                .await
                .upload_limit
                .unwrap_or(0),
        };
        limits.insert(hash, serde_json::json!(value));
    }
    (StatusCode::OK, Json(serde_json::Value::Object(limits)))
}

#[derive(Debug, Clone, Copy)]
enum LimitField {
    Download,
    Upload,
}

#[derive(Debug, Clone, Copy)]
enum BoolLimitField {
    Sequential,
    FirstLast,
    ForceStart,
    SuperSeeding,
    AutoTmm,
    AutoManagement,
}

async fn update_limit_field(
    State(state): State<AppState>,
    body: String,
    field: LimitField,
) -> StatusCode {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let hashes = resolve_hashes(&state, hashes).await;
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0);
    for hash in hashes {
        let mut limits = get_torrent_limits(&state, &hash).await;
        match field {
            LimitField::Download => limits.download_limit = limit,
            LimitField::Upload => limits.upload_limit = limit,
        }
        if update_torrent_limits(&state, &hash, limits).await != StatusCode::OK {
            return StatusCode::NOT_FOUND;
        }
    }
    StatusCode::OK
}

async fn update_global_limit(state: &AppState, body: &str, field: LimitField) -> StatusCode {
    let Some(engine) = &state.engine else {
        return StatusCode::OK;
    };
    let params = parse_form_body(body);
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let mut limits = global_limits(state).await;
    match field {
        LimitField::Download => limits.download_limit = limit,
        LimitField::Upload => limits.upload_limit = limit,
    }
    match engine.update_global_limits(limits).await {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn update_bool_limit_field(
    State(state): State<AppState>,
    body: String,
    field: BoolLimitField,
) -> StatusCode {
    let params = parse_form_body(&body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let hashes = resolve_hashes(&state, hashes).await;
    let requested = params
        .get("value")
        .or_else(|| params.get("enable"))
        .and_then(|value| parse_qbit_bool(value));
    for hash in hashes {
        let mut limits = get_torrent_limits(&state, &hash).await;
        let current = match field {
            BoolLimitField::Sequential => limits.sequential_download,
            BoolLimitField::FirstLast => limits.first_last_piece_prio,
            BoolLimitField::ForceStart => limits.force_start,
            BoolLimitField::SuperSeeding => limits.super_seeding,
            BoolLimitField::AutoTmm => limits.auto_tmm,
            BoolLimitField::AutoManagement => limits.auto_management,
        };
        let value = requested.unwrap_or(!current);
        match field {
            BoolLimitField::Sequential => limits.sequential_download = value,
            BoolLimitField::FirstLast => limits.first_last_piece_prio = value,
            BoolLimitField::ForceStart => limits.force_start = value,
            BoolLimitField::SuperSeeding => limits.super_seeding = value,
            BoolLimitField::AutoTmm => limits.auto_tmm = value,
            BoolLimitField::AutoManagement => limits.auto_management = value,
        }
        if update_torrent_limits(&state, &hash, limits).await != StatusCode::OK {
            return StatusCode::NOT_FOUND;
        }
    }
    StatusCode::OK
}

async fn global_limits(state: &AppState) -> EngineGlobalLimits {
    let Some(engine) = &state.engine else {
        return EngineGlobalLimits::default();
    };
    engine.global_limits().await.unwrap_or_default()
}

async fn reserve_qbit_api_snapshot(
    state: &AppState,
    bytes: u64,
) -> Result<Option<MemoryLease>, String> {
    let Some(engine) = &state.engine else {
        return Ok(None);
    };
    engine.reserve_memory(MemoryClass::ApiSnapshot, bytes).await
}

fn qbit_api_snapshot_budget_exhausted() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "api snapshot memory budget exhausted",
    )
        .into_response()
}

fn estimate_qbit_torrent_info_snapshot_bytes(torrent_count: usize) -> u64 {
    (torrent_count as u64).saturating_mul(2048)
}

fn estimate_qbit_maindata_snapshot_bytes(torrent_count: usize) -> u64 {
    // /sync/maindata wraps torrent info in a keyed map plus server state.
    16 * 1024 + (torrent_count as u64).saturating_mul(2304)
}

fn estimate_qbit_metadata_snapshot_bytes(
    file_count: usize,
    piece_count: usize,
    webseed_count: usize,
) -> u64 {
    16 * 1024
        + (file_count as u64).saturating_mul(512)
        + (piece_count as u64).saturating_mul(96)
        + (webseed_count as u64).saturating_mul(256)
}

fn estimate_qbit_tracker_snapshot_bytes(tracker_count: usize) -> u64 {
    8 * 1024 + (tracker_count as u64).saturating_mul(512)
}

fn estimate_qbit_label_snapshot_bytes(item_count: usize) -> u64 {
    8 * 1024 + (item_count as u64).saturating_mul(256)
}

fn estimate_qbit_limit_map_snapshot_bytes(torrent_count: usize) -> u64 {
    8 * 1024 + (torrent_count as u64).saturating_mul(192)
}

fn estimate_qbit_properties_snapshot_bytes() -> u64 {
    32 * 1024
}

fn estimate_qbit_peer_snapshot_bytes(peer_count: usize) -> u64 {
    8 * 1024 + (peer_count as u64).saturating_mul(1024)
}

async fn queue_priority(state: &AppState, hash: &str) -> i32 {
    let Some(engine) = &state.engine else {
        return 0;
    };
    engine.queue_priority(hash.to_owned()).await.unwrap_or(0)
}

async fn update_queue_order(state: &AppState, body: &str, queue_move: QueueMove) -> StatusCode {
    let params = parse_form_body(body);
    let hashes = params
        .get("hashes")
        .map(|h| extract_hashes_from_str(h))
        .unwrap_or_default();
    let hashes = resolve_hashes(state, hashes).await;
    let Some(engine) = &state.engine else {
        return StatusCode::OK;
    };
    match engine.update_queue_order(hashes, queue_move).await {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

async fn get_torrent_limits(state: &AppState, hash: &str) -> EngineTorrentLimits {
    let Some(engine) = &state.engine else {
        return EngineTorrentLimits::default();
    };
    engine
        .torrent_limits(hash.to_owned())
        .await
        .unwrap_or_default()
}

async fn update_torrent_limits(
    state: &AppState,
    hash: &str,
    limits: EngineTorrentLimits,
) -> StatusCode {
    let Some(engine) = &state.engine else {
        return StatusCode::OK;
    };
    match engine.update_torrent_limits(hash.to_owned(), limits).await {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

fn parse_qbit_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

async fn qbit_torrent_info(state: &AppState, e: &rt_session::TorrentEntry) -> QbTorrentInfo {
    let progress = torrent_progress(e.total_length, e.amount_left, e.completed_at.is_some());
    let (tracker, trackers_count) = if state.engine.is_some() {
        qbit_tracker_projection(state, &e.info_hash).await
    } else {
        (String::new(), 0)
    };
    let priority = queue_priority(state, &e.info_hash).await;
    let limits = get_torrent_limits(state, &e.info_hash).await;
    QbTorrentInfo {
        hash: e.info_hash.clone(),
        name: e.name.clone(),
        state: to_qbit_state(e.state.as_str()).to_owned(),
        size: e.total_length as i64,
        total_size: e.total_length as i64,
        downloaded: e.stats.downloaded as i64,
        downloaded_session: e.stats.downloaded as i64,
        uploaded: e.stats.uploaded as i64,
        uploaded_session: e.stats.uploaded as i64,
        ratio: e.stats.ratio(),
        save_path: format!("{}/", e.save_path.trim_end_matches('/')),
        content_path: format!(
            "{}/{}",
            e.save_path.trim_end_matches('/'),
            e.name.trim_start_matches('/')
        ),
        root_path: format!(
            "{}/{}",
            e.save_path.trim_end_matches('/'),
            e.name.trim_start_matches('/')
        ),
        category: e.category.clone().unwrap_or_default(),
        tags: e.tags.join(","),
        added_on: e.added_at as i64,
        completion_on: e.completed_at.map(|t| t as i64).unwrap_or(-1),
        last_activity: e.added_at as i64,
        seen_complete: e.completed_at.map(|t| t as i64).unwrap_or(-1),
        time_active: 0,
        seeding_time: 0,
        num_leechs: 0,
        num_seeds: 0,
        dlspeed: 0,
        upspeed: 0,
        dl_limit: limits.download_limit.unwrap_or(-1),
        up_limit: limits.upload_limit.unwrap_or(-1),
        eta: -1,
        progress,
        priority,
        amount_left: e.amount_left as i64,
        auto_tmm: limits.auto_tmm || limits.auto_management,
        seq_dl: limits.sequential_download,
        f_l_piece_prio: limits.first_last_piece_prio,
        force_start: limits.force_start,
        super_seeding: limits.super_seeding,
        ratio_limit: limits.seed_ratio_limit.unwrap_or(-1.0),
        seeding_time_limit: limits.seed_idle_limit.unwrap_or(-1),
        tracker,
        trackers_count,
        magnet_uri: qbit_magnet_uri(&e.info_hash),
        infohash_v1: if e.info_hash.len() == 40 {
            e.info_hash.clone()
        } else {
            String::new()
        },
        infohash_v2: if e.info_hash.len() == 64 {
            e.info_hash.clone()
        } else {
            String::new()
        },
    }
}

fn qbit_magnet_uri(info_hash: &str) -> String {
    if info_hash.len() == 64 && info_hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
        format!("magnet:?xt=urn:btmh:1220{}", info_hash.to_ascii_lowercase())
    } else {
        format!("magnet:?xt=urn:btih:{info_hash}")
    }
}

fn redact_log_url(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("magnet:?") {
        return "magnet:redacted".to_owned();
    }
    match Url::parse(value) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            let sensitive = [
                "token", "apikey", "api_key", "passkey", "auth", "password", "cookie", "session",
            ];
            let pairs = url
                .query_pairs()
                .map(|(key, value)| {
                    if sensitive
                        .iter()
                        .any(|needle| key.to_ascii_lowercase().contains(needle))
                    {
                        (key.into_owned(), "redacted".to_owned())
                    } else {
                        (key.into_owned(), value.into_owned())
                    }
                })
                .collect::<Vec<_>>();
            url.set_query(None);
            if !pairs.is_empty() {
                let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                for (key, value) in pairs {
                    serializer.append_pair(&key, &value);
                }
                url.set_query(Some(&serializer.finish()));
            }
            url.to_string()
        }
        Err(_) => "url:invalid".to_owned(),
    }
}

async fn qbit_tracker_projection(state: &AppState, info_hash: &str) -> (String, u32) {
    if let Some(cached) = state
        .tracker_projection_cache
        .read()
        .await
        .get(info_hash)
        .cloned()
    {
        return cached;
    }
    let Some(engine) = &state.engine else {
        return (String::new(), 0);
    };
    let projection = match engine.torrent_metadata(info_hash.to_owned()).await {
        Ok(meta) => (
            meta.trackers.first().cloned().unwrap_or_default(),
            meta.trackers.len() as u32,
        ),
        Err(_) => (String::new(), 0),
    };
    if projection.1 > 0 {
        state
            .tracker_projection_cache
            .write()
            .await
            .insert(info_hash.to_owned(), projection.clone());
    }
    projection
}

fn sync_rid_for_infos(infos: &[QbTorrentInfo]) -> i64 {
    let mut hasher = DefaultHasher::new();
    let mut infos = infos.iter().collect::<Vec<_>>();
    infos.sort_by(|a, b| a.hash.cmp(&b.hash));
    for info in infos {
        info.hash.hash(&mut hasher);
        info.name.hash(&mut hasher);
        info.state.hash(&mut hasher);
        info.size.hash(&mut hasher);
        info.downloaded.hash(&mut hasher);
        info.uploaded.hash(&mut hasher);
        info.amount_left.hash(&mut hasher);
        info.save_path.hash(&mut hasher);
        info.category.hash(&mut hasher);
        info.tags.hash(&mut hasher);
        info.added_on.hash(&mut hasher);
        info.completion_on.hash(&mut hasher);
        info.tracker.hash(&mut hasher);
        info.trackers_count.hash(&mut hasher);
        info.progress.to_bits().hash(&mut hasher);
        info.ratio.to_bits().hash(&mut hasher);
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

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn split_tags(tags: &str) -> Vec<String> {
    tags.split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn split_tracker_values(values: &str) -> Vec<String> {
    normalize_tracker_values(
        values
            .split(['|', '\n', '\r'])
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    )
}

fn normalize_tracker_values(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim().to_owned();
        if !value.is_empty() && !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

fn split_pipe_values(values: &str) -> Vec<String> {
    values.split('|').filter_map(normalize_api_text).collect()
}

fn parse_peer_addrs(values: &str) -> Vec<SocketAddr> {
    values
        .split('|')
        .filter_map(|peer| peer.trim().parse::<SocketAddr>().ok())
        .collect()
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

async fn current_tracker_urls(state: &AppState, hash: &str) -> Vec<String> {
    let Some(engine) = &state.engine else {
        return Vec::new();
    };
    engine
        .torrent_metadata(hash.to_owned())
        .await
        .map(|meta| meta.trackers)
        .unwrap_or_default()
}

async fn update_torrent_trackers(
    state: &AppState,
    hash: &str,
    trackers: Vec<String>,
) -> StatusCode {
    let Some(engine) = &state.engine else {
        return StatusCode::OK;
    };
    match engine
        .update_torrent_trackers(hash.to_owned(), trackers)
        .await
    {
        Ok(()) => {
            state.tracker_projection_cache.write().await.remove(hash);
            StatusCode::OK
        }
        Err(_) => StatusCode::NOT_FOUND,
    }
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

    fn qbit_info(hash: &str, tracker: &str, trackers_count: u32) -> QbTorrentInfo {
        QbTorrentInfo {
            hash: hash.to_owned(),
            name: hash.to_owned(),
            state: "downloading".into(),
            size: 100,
            total_size: 100,
            downloaded: 25,
            downloaded_session: 25,
            uploaded: 5,
            uploaded_session: 5,
            ratio: 0.2,
            save_path: "/data/".into(),
            content_path: format!("/data/{hash}"),
            root_path: format!("/data/{hash}"),
            category: String::new(),
            tags: String::new(),
            added_on: 1,
            completion_on: -1,
            last_activity: 1,
            seen_complete: -1,
            time_active: 0,
            seeding_time: 0,
            num_leechs: 0,
            num_seeds: 0,
            dlspeed: 0,
            upspeed: 0,
            dl_limit: -1,
            up_limit: -1,
            eta: -1,
            progress: 0.25,
            priority: 0,
            amount_left: 75,
            auto_tmm: false,
            seq_dl: false,
            f_l_piece_prio: false,
            force_start: false,
            super_seeding: false,
            ratio_limit: -1.0,
            seeding_time_limit: -1,
            tracker: tracker.to_owned(),
            trackers_count,
            magnet_uri: qbit_magnet_uri(hash),
            infohash_v1: hash.to_owned(),
            infohash_v2: String::new(),
        }
    }

    #[test]
    fn qbit_log_entry_projects_session_events() {
        let row = rt_db::SessionEventRow {
            event_id: Some(42),
            occurred_at: 1_700_000_000,
            info_hash: Some("a".repeat(40)),
            kind: "tracker_warning".to_owned(),
            message: Some("tracker warning".to_owned()),
            payload: "{}".to_owned(),
        };

        let entry = qbit_log_entry(row);
        assert_eq!(entry.id, 42);
        assert_eq!(entry.message, "tracker warning");
        assert_eq!(entry.timestamp, 1_700_000_000);
        assert_eq!(entry.kind, 2);
    }

    #[test]
    fn qbit_log_type_uses_level_payload_and_kind_fallbacks() {
        assert_eq!(qbit_log_type("torrent_added", r#"{"level":"info"}"#), 1);
        assert_eq!(qbit_log_type("tracker_warning", "{}"), 2);
        assert_eq!(qbit_log_type("storage_failed", "{}"), 4);
        assert_eq!(qbit_log_type("tracker", r#"{"level":"critical"}"#), 4);
    }

    #[test]
    fn log_main_query_filters_qbit_types() {
        let all = LogMainQuery {
            limit: None,
            normal: None,
            info: None,
            warning: None,
            critical: None,
        };
        assert!(all.includes_type(1));
        assert!(all.includes_type(2));
        assert!(all.includes_type(4));

        let warnings = LogMainQuery {
            limit: None,
            normal: None,
            info: None,
            warning: Some(true),
            critical: None,
        };
        assert!(!warnings.includes_type(1));
        assert!(warnings.includes_type(2));
        assert!(!warnings.includes_type(4));

        let critical = LogMainQuery {
            limit: None,
            normal: Some(false),
            info: Some(false),
            warning: Some(false),
            critical: Some(true),
        };
        assert!(!critical.includes_type(1));
        assert!(!critical.includes_type(2));
        assert!(critical.includes_type(4));
        assert_eq!(
            critical.included_levels(),
            vec!["error".to_owned(), "critical".to_owned()]
        );
    }

    #[tokio::test]
    async fn login_returns_ok() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .clone()
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
        assert_json_keys(
            &v,
            &[
                "locale",
                "create_subfolder_enabled",
                "start_paused_enabled",
                "auto_delete_mode",
                "preallocate_all",
                "incomplete_files_ext",
                "auto_tmm_enabled",
                "torrent_changed_tmm_enabled",
                "save_path_changed_tmm_enabled",
                "category_changed_tmm_enabled",
                "save_path",
                "temp_path_enabled",
                "temp_path",
                "scan_dirs",
                "download_in_scan_dirs",
                "export_dir_enabled",
                "export_dir",
                "export_dir_fin",
                "mail_notification_enabled",
                "mail_notification_sender",
                "mail_notification_email",
                "mail_notification_smtp",
                "mail_notification_ssl_enabled",
                "mail_notification_auth_enabled",
                "mail_notification_username",
                "mail_notification_password",
                "autorun_enabled",
                "autorun_program",
                "queueing_enabled",
                "max_active_downloads",
                "max_active_torrents",
                "max_active_uploads",
                "dont_count_slow_torrents",
                "slow_torrent_dl_rate_threshold",
                "slow_torrent_ul_rate_threshold",
                "slow_torrent_inactive_timer",
                "max_ratio_enabled",
                "max_ratio",
                "max_ratio_act",
                "max_seeding_time_enabled",
                "max_seeding_time",
                "listen_port",
                "upnp",
                "random_port",
                "dl_limit",
                "up_limit",
                "max_connec",
                "max_connec_per_torrent",
                "max_uploads",
                "max_uploads_per_torrent",
                "stop_tracker_timeout",
                "enable_piece_extent_affinity",
                "bittorrent_protocol",
                "limit_utp_rate",
                "limit_tcp_overhead",
                "limit_lan_peers",
                "alt_dl_limit",
                "alt_up_limit",
                "scheduler_enabled",
                "schedule_from_hour",
                "schedule_from_min",
                "schedule_to_hour",
                "schedule_to_min",
                "scheduler_days",
                "dht",
                "dhtSameAsBT",
                "dht_port",
                "pex",
                "lsd",
                "encryption",
                "anonymous_mode",
                "proxy_type",
                "proxy_ip",
                "proxy_port",
                "proxy_peer_connections",
                "proxy_auth_enabled",
                "proxy_username",
                "proxy_password",
                "proxy_torrents_only",
                "ip_filter_enabled",
                "ip_filter_path",
                "ip_filter_trackers",
                "web_ui_domain_list",
                "web_ui_address",
                "web_ui_port",
                "web_ui_upnp",
                "web_ui_username",
                "web_ui_password",
                "web_ui_csrf_protection_enabled",
                "web_ui_clickjacking_protection_enabled",
                "web_ui_secure_cookie_enabled",
                "web_ui_max_auth_fail_count",
                "web_ui_ban_duration",
                "web_ui_session_timeout",
                "web_ui_host_header_validation_enabled",
                "bypass_local_auth",
                "bypass_auth_subnet_whitelist_enabled",
                "bypass_auth_subnet_whitelist",
                "alternative_webui_enabled",
                "alternative_webui_path",
                "use_https",
                "ssl_key",
                "ssl_cert",
                "web_ui_https_key_path",
                "web_ui_https_cert_path",
                "dyndns_enabled",
                "dyndns_service",
                "dyndns_username",
                "dyndns_password",
                "dyndns_domain",
                "rss_refresh_interval",
                "rss_max_articles_per_feed",
                "rss_processing_enabled",
                "rss_auto_downloading_enabled",
                "rss_download_repack_proper_episodes",
                "rss_smart_episode_filters",
                "add_trackers_enabled",
                "add_trackers",
                "web_ui_use_custom_http_headers_enabled",
                "web_ui_custom_http_headers",
                "announce_to_all_tiers",
                "announce_to_all_trackers",
                "async_io_threads",
                "banned_ips",
                "checking_memory_use",
                "current_interface_address",
                "current_network_interface",
                "disk_cache",
                "disk_cache_ttl",
                "embedded_tracker_port",
                "enable_coalesce_read_write",
                "enable_embedded_tracker",
                "enable_multi_connections_from_same_ip",
                "enable_os_cache",
                "enable_upload_suggestions",
                "file_pool_size",
                "outgoing_ports_max",
                "outgoing_ports_min",
                "recheck_completed_torrents",
                "resolve_peer_countries",
                "save_resume_data_interval",
                "send_buffer_low_watermark",
                "send_buffer_watermark",
                "send_buffer_watermark_factor",
                "socket_backlog_size",
                "upload_choking_algorithm",
                "upload_slots_behavior",
                "upnp_lease_duration",
                "utp_tcp_mixed_mode",
            ],
        );

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
    async fn app_set_preferences_persists_form_and_json_updates() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/setPreferences")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "json=%7B%22locale%22%3A%22fr%22%2C%22scheduler_enabled%22%3Atrue%2C%22web_ui_port%22%3A9090%7D",
                    ))
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
                    .uri("/api/qb/v2/app/setPreferences")
                    .body(Body::from(
                        r#"{"rss_processing_enabled":true,"save_path":"/incoming"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/app/preferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["locale"], "fr");
        assert_eq!(v["scheduler_enabled"], true);
        assert_eq!(v["web_ui_port"], 9090);
        assert_eq!(v["rss_processing_enabled"], true);
        assert_eq!(v["save_path"], "/incoming");
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
    async fn qbit_response_field_matrix_is_present() {
        let hash = "a".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_json_keys(
            &body[0],
            &[
                "hash",
                "name",
                "state",
                "size",
                "total_size",
                "downloaded",
                "downloaded_session",
                "uploaded",
                "uploaded_session",
                "ratio",
                "save_path",
                "content_path",
                "root_path",
                "category",
                "tags",
                "added_on",
                "completion_on",
                "last_activity",
                "seen_complete",
                "time_active",
                "seeding_time",
                "num_leechs",
                "num_seeds",
                "dlspeed",
                "upspeed",
                "dl_limit",
                "up_limit",
                "eta",
                "progress",
                "priority",
                "amount_left",
                "auto_tmm",
                "seq_dl",
                "f_l_piece_prio",
                "force_start",
                "super_seeding",
                "ratio_limit",
                "seeding_time_limit",
                "tracker",
                "trackers_count",
                "magnet_uri",
                "infohash_v1",
                "infohash_v2",
            ],
        );

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/torrents/properties?hash={hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_json_keys(
            &body,
            &[
                "save_path",
                "creation_date",
                "piece_size",
                "comment",
                "total_wasted",
                "total_uploaded",
                "total_uploaded_session",
                "total_downloaded",
                "total_downloaded_session",
                "up_limit",
                "dl_limit",
                "time_elapsed",
                "seeding_time",
                "nb_connections",
                "nb_connections_limit",
                "share_ratio",
                "addition_date",
                "completion_date",
                "created_by",
                "dl_speed_avg",
                "dl_speed",
                "eta",
                "last_seen",
                "peers",
                "peers_total",
                "pieces_have",
                "pieces_num",
                "reannounce",
                "seeds",
                "seeds_total",
                "total_size",
                "up_speed_avg",
                "up_speed",
            ],
        );

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
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_json_keys(
            &body,
            &[
                "rid",
                "full_update",
                "torrents",
                "torrents_removed",
                "server_state",
            ],
        );
        assert_json_keys(
            &body["server_state"],
            &[
                "dl_info_speed",
                "dl_info_data",
                "up_info_speed",
                "up_info_data",
                "alltime_dl",
                "alltime_ul",
                "average_time_queue",
                "connection_status",
                "free_space_on_disk",
                "global_ratio",
                "queued_io_jobs",
                "queueing",
                "read_cache_hits",
                "read_cache_overload",
                "refresh_interval",
                "total_buffers_size",
                "total_peer_connections",
                "total_queued_size",
                "total_wasted_session",
                "dl_rate_limit",
                "up_rate_limit",
                "use_alt_speed_limits",
                "write_cache_overload",
            ],
        );
        let sync_torrent = &body["torrents"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"];
        assert_json_keys(
            sync_torrent,
            &[
                "hash",
                "name",
                "state",
                "size",
                "total_size",
                "downloaded",
                "downloaded_session",
                "uploaded",
                "uploaded_session",
                "ratio",
                "save_path",
                "content_path",
                "root_path",
                "category",
                "tags",
                "added_on",
                "completion_on",
                "last_activity",
                "seen_complete",
                "time_active",
                "seeding_time",
                "dl_limit",
                "up_limit",
                "seq_dl",
                "f_l_piece_prio",
                "force_start",
                "super_seeding",
                "ratio_limit",
                "seeding_time_limit",
                "magnet_uri",
                "infohash_v1",
                "infohash_v2",
            ],
        );
    }

    fn assert_json_keys(value: &serde_json::Value, keys: &[&str]) {
        let obj = value.as_object().expect("expected JSON object");
        for key in keys {
            assert!(obj.contains_key(*key), "missing key {key} in {obj:?}");
        }
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
            entry.added_at = 100;
            entry.completed_at = Some(200);
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
        assert!(v["time_elapsed"].as_i64().unwrap() > 0);
        assert!(v["seeding_time"].as_i64().unwrap() > 0);
        assert_eq!(v["eta"].as_i64().unwrap(), -1);
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
            .clone()
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

    #[test]
    fn sync_rid_is_order_independent_and_covers_metadata_projection() {
        let first = qbit_info("a", "udp://tracker-a", 1);
        let second = qbit_info("b", "udp://tracker-b", 1);
        assert_eq!(
            sync_rid_for_infos(&[first.clone(), second.clone()]),
            sync_rid_for_infos(&[second, first.clone()])
        );

        let changed = qbit_info("a", "udp://tracker-c", 2);
        assert_ne!(sync_rid_for_infos(&[first]), sync_rid_for_infos(&[changed]));
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
    async fn set_category_applies_stored_category_save_path() {
        let hash = "4".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/createCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("category=linux&savePath=%2Fsrv%2Flinux"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&category=linux"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reg = state.registry.read().await;
        let entry = reg.get(&hash).unwrap();
        assert_eq!(entry.category.as_deref(), Some("linux"));
        assert_eq!(entry.save_path, "/srv/linux");
    }

    #[tokio::test]
    async fn qbit_alias_and_broad_compat_routes_are_registered() {
        let app = build_qbit_router(make_state_with(&"a".repeat(40)).await);
        for (method, path, body) in qbit_route_matrix() {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_ne!(resp.status(), StatusCode::NOT_FOUND, "{path}");
            assert_ne!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
        }
    }

    fn qbit_route_matrix() -> Vec<(&'static str, &'static str, &'static str)> {
        vec![
            ("POST", "/api/qb/v2/auth/login", ""),
            ("POST", "/api/qb/v2/auth/logout", ""),
            ("GET", "/api/qb/v2/app/version", ""),
            ("GET", "/api/qb/v2/app/webapiVersion", ""),
            ("GET", "/api/qb/v2/app/buildInfo", ""),
            ("GET", "/api/qb/v2/app/preferences", ""),
            ("POST", "/api/qb/v2/app/setPreferences", "json={}"),
            ("POST", "/api/qb/v2/app/shutdown", ""),
            ("POST", "/api/qb/v2/app/sendTestEmail", ""),
            ("GET", "/api/qb/v2/app/getCookies", ""),
            ("POST", "/api/qb/v2/app/setCookies", "cookies=[]"),
            ("POST", "/api/qb/v2/app/rotateAPIKey", ""),
            ("POST", "/api/qb/v2/app/deleteAPIKey", ""),
            ("GET", "/api/qb/v2/app/networkInterfaceList", ""),
            ("GET", "/api/qb/v2/app/networkInterfaceAddressList", ""),
            ("GET", "/api/qb/v2/app/defaultSavePath", ""),
            ("GET", "/api/qb/v2/torrents/info", ""),
            ("POST", "/api/qb/v2/torrents/add", ""),
            ("POST", "/api/qb/v2/torrents/pause", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/resume", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/start", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/stop", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/delete", ""),
            ("POST", "/api/qb/v2/torrents/reannounce", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/recheck", "hashes=all"),
            ("GET", "/api/qb/v2/torrents/trackers", ""),
            (
                "POST",
                "/api/qb/v2/torrents/addTrackers",
                "hash=a&urls=http://tracker/announce",
            ),
            ("POST", "/api/qb/v2/torrents/editTracker", ""),
            (
                "POST",
                "/api/qb/v2/torrents/removeTrackers",
                "hash=a&urls=http://a",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/addPeers",
                "hashes=a&peers=127.0.0.1:6881",
            ),
            ("GET", "/api/qb/v2/torrents/files", ""),
            ("GET", "/api/qb/v2/torrents/webseeds", ""),
            ("GET", "/api/qb/v2/torrents/pieceStates", ""),
            ("GET", "/api/qb/v2/torrents/pieceHashes", ""),
            ("GET", "/api/qb/v2/torrents/export", ""),
            (
                "POST",
                "/api/qb/v2/torrents/filePrio",
                "hash=a&id=0&priority=1",
            ),
            ("POST", "/api/qb/v2/torrents/increasePrio", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/decreasePrio", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/topPrio", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/bottomPrio", "hashes=all"),
            ("GET", "/api/qb/v2/torrents/properties", ""),
            ("GET", "/api/qb/v2/torrents/categories", ""),
            ("GET", "/api/qb/v2/torrents/tags", ""),
            (
                "POST",
                "/api/qb/v2/torrents/rename",
                "hash=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&name=b",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/renameFile",
                "hash=a&oldPath=a&newPath=b",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/renameFolder",
                "hash=a&oldPath=a&newPath=b",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setLocation",
                "hashes=all&location=/tmp",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setSavePath",
                "hashes=all&savePath=/tmp",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setCategory",
                "hashes=all&category=test",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/createCategory",
                "category=test",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/editCategory",
                "category=test&savePath=/tmp",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/removeCategories",
                "categories=test",
            ),
            ("POST", "/api/qb/v2/torrents/addTags", "hashes=all&tags=a,b"),
            ("POST", "/api/qb/v2/torrents/setTags", "hashes=all&tags=a,b"),
            (
                "POST",
                "/api/qb/v2/torrents/removeTags",
                "hashes=all&tags=a,b",
            ),
            ("POST", "/api/qb/v2/torrents/createTags", "tags=a,b"),
            ("POST", "/api/qb/v2/torrents/deleteTags", "tags=a,b"),
            ("GET", "/api/qb/v2/torrents/downloadLimit", ""),
            (
                "POST",
                "/api/qb/v2/torrents/setDownloadLimit",
                "hashes=all&limit=0",
            ),
            ("GET", "/api/qb/v2/torrents/uploadLimit", ""),
            (
                "POST",
                "/api/qb/v2/torrents/setUploadLimit",
                "hashes=all&limit=0",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setShareLimits",
                "hashes=all&ratioLimit=-2&seedingTimeLimit=-2",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setForceStart",
                "hashes=all&value=false",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setSuperSeeding",
                "hashes=all&value=false",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setAutoTMM",
                "hashes=all&enable=false",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setAutoManagement",
                "hashes=all&enable=false",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/toggleSequentialDownload",
                "hashes=all",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/toggleFirstLastPiecePrio",
                "hashes=all",
            ),
            ("GET", "/api/qb/v2/sync/maindata", ""),
            ("GET", "/api/qb/v2/sync/torrentPeers", ""),
            ("GET", "/api/qb/v2/transfer/info", ""),
            ("GET", "/api/qb/v2/transfer/downloadLimit", ""),
            ("GET", "/api/qb/v2/transfer/uploadLimit", ""),
            ("GET", "/api/qb/v2/transfer/speedLimitsMode", ""),
            ("POST", "/api/qb/v2/transfer/toggleSpeedLimitsMode", ""),
            ("POST", "/api/qb/v2/transfer/setDownloadLimit", "limit=0"),
            ("POST", "/api/qb/v2/transfer/setUploadLimit", "limit=0"),
            (
                "POST",
                "/api/qb/v2/transfer/banPeers",
                "peers=127.0.0.1:6881",
            ),
            ("GET", "/api/qb/v2/log/main", ""),
            ("GET", "/api/qb/v2/log/peers", ""),
            ("GET", "/api/qb/v2/search/status", ""),
            ("GET", "/api/qb/v2/search/categories", ""),
            ("GET", "/api/qb/v2/search/plugins", ""),
            ("POST", "/api/qb/v2/search/installPlugin", "sources="),
            ("POST", "/api/qb/v2/search/uninstallPlugin", "names="),
            (
                "POST",
                "/api/qb/v2/search/enablePlugin",
                "names=&enable=true",
            ),
            ("POST", "/api/qb/v2/search/updatePlugins", ""),
            (
                "POST",
                "/api/qb/v2/search/start",
                "pattern=test&plugins=all&category=all",
            ),
            ("POST", "/api/qb/v2/search/stop", "id=0"),
            ("GET", "/api/qb/v2/search/results", ""),
            ("POST", "/api/qb/v2/search/delete", "id=0"),
            ("GET", "/api/qb/v2/rss/items", ""),
            ("GET", "/api/qb/v2/rss/rules", ""),
            ("GET", "/api/qb/v2/rss/matchingArticles", ""),
            ("POST", "/api/qb/v2/rss/addFolder", "path=test"),
            (
                "POST",
                "/api/qb/v2/rss/addFeed",
                "url=http://example.com/feed&path=test",
            ),
            ("POST", "/api/qb/v2/rss/removeItem", "path=test"),
            (
                "POST",
                "/api/qb/v2/rss/moveItem",
                "itemPath=test&destPath=dest",
            ),
            (
                "POST",
                "/api/qb/v2/rss/markAsRead",
                "itemPath=test&articleId=all",
            ),
            ("POST", "/api/qb/v2/rss/refreshItem", "itemPath=test"),
            ("POST", "/api/qb/v2/rss/setRule", "ruleName=test&ruleDef={}"),
            (
                "POST",
                "/api/qb/v2/rss/renameRule",
                "ruleName=test&newRuleName=test2",
            ),
            ("POST", "/api/qb/v2/rss/removeRule", "ruleName=test"),
        ]
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
        let app = build_qbit_router(state.clone());
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
            .clone()
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

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/deleteTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("tags=hd"))
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
        assert_eq!(tags, vec!["remux".to_owned()]);
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
    fn redact_log_url_removes_sensitive_parts() {
        assert_eq!(
            redact_log_url("magnet:?xt=urn:btih:abcdef"),
            "magnet:redacted"
        );
        let redacted = redact_log_url(
            "https://user:pass@example.test/announce?passkey=secret&foo=bar&api_key=hidden",
        );
        assert!(redacted.starts_with("https://example.test/announce?"));
        assert!(redacted.contains("passkey=redacted"));
        assert!(redacted.contains("api_key=redacted"));
        assert!(redacted.contains("foo=bar"));
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("hidden"));
        assert_eq!(redact_log_url("not a url"), "url:invalid");
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

    #[test]
    fn split_tracker_values_accepts_qbit_separators_and_dedupes() {
        assert_eq!(
            split_tracker_values("udp://one/announce|udp://two/announce\nudp://one/announce"),
            vec![
                "udp://one/announce".to_owned(),
                "udp://two/announce".to_owned()
            ]
        );
    }

    #[test]
    fn parse_peer_addrs_accepts_pipe_separated_socket_addresses() {
        let peers = parse_peer_addrs("127.0.0.1:6881|[::1]:6882|bad");
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0], "127.0.0.1:6881".parse::<SocketAddr>().unwrap());
        assert_eq!(peers[1], "[::1]:6882".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn qbit_peer_log_entry_projects_engine_peer_snapshot() {
        let peer = EnginePeerSnapshot {
            addr: "127.0.0.1:6881".parse().unwrap(),
            client: "BitTorrent peer".to_owned(),
            choked: false,
            upload_choked: false,
            interested: true,
            pieces: 4,
            pieces_total: 8,
            progress: 0.5,
            download_rate: 128,
            upload_rate: 256,
            downloaded: 1024,
            uploaded: 2048,
        };
        let entry = qbit_peer_log_entry(&"a".repeat(40), &peer);
        assert_eq!(entry["torrent"], "a".repeat(40));
        assert_eq!(entry["ip"], "127.0.0.1");
        assert_eq!(entry["port"], 6881);
        assert_eq!(entry["progress"], 0.5);
    }

    #[test]
    fn qbit_torrent_peers_projection_and_rid_are_stable() {
        let first = EnginePeerSnapshot {
            addr: "10.0.0.2:51413".parse().unwrap(),
            client: "peer-a".to_owned(),
            choked: false,
            upload_choked: false,
            interested: true,
            pieces: 5,
            pieces_total: 10,
            progress: 0.5,
            download_rate: 111,
            upload_rate: 222,
            downloaded: 333,
            uploaded: 444,
        };
        let second = EnginePeerSnapshot {
            addr: "10.0.0.3:51413".parse().unwrap(),
            client: "peer-b".to_owned(),
            choked: true,
            upload_choked: true,
            interested: false,
            pieces: 10,
            pieces_total: 10,
            progress: 1.0,
            download_rate: 0,
            upload_rate: 555,
            downloaded: 666,
            uploaded: 777,
        };
        assert_eq!(
            qbit_peer_rid(&[first.clone(), second.clone()]),
            qbit_peer_rid(&[second.clone(), first.clone()])
        );
        let changed = EnginePeerSnapshot {
            download_rate: 999,
            ..first.clone()
        };
        assert_ne!(qbit_peer_rid(&[first.clone()]), qbit_peer_rid(&[changed]));

        let peers = qbit_peer_map(&[first]);
        let peer = &peers["10.0.0.2:51413"];
        assert_json_keys(
            peer,
            &[
                "client",
                "connection",
                "country",
                "country_code",
                "dl_speed",
                "downloaded",
                "files",
                "flags",
                "flags_desc",
                "ip",
                "peer_id_client",
                "port",
                "progress",
                "relevance",
                "up_speed",
                "uploaded",
            ],
        );
    }

    #[test]
    fn parse_qbit_bool_accepts_common_wire_values() {
        assert_eq!(parse_qbit_bool("true"), Some(true));
        assert_eq!(parse_qbit_bool("1"), Some(true));
        assert_eq!(parse_qbit_bool("false"), Some(false));
        assert_eq!(parse_qbit_bool("0"), Some(false));
        assert_eq!(parse_qbit_bool("wat"), None);
    }

    #[test]
    fn qbit_api_snapshot_estimates_scale_with_torrent_count() {
        assert_eq!(estimate_qbit_torrent_info_snapshot_bytes(0), 0);
        assert_eq!(estimate_qbit_torrent_info_snapshot_bytes(10), 20_480);
        assert_eq!(estimate_qbit_maindata_snapshot_bytes(0), 16 * 1024);
        assert_eq!(
            estimate_qbit_maindata_snapshot_bytes(10),
            16 * 1024 + 23_040
        );
        assert_eq!(
            estimate_qbit_metadata_snapshot_bytes(10, 100, 2),
            16 * 1024 + 5_120 + 9_600 + 512
        );
        assert_eq!(estimate_qbit_tracker_snapshot_bytes(10), 8 * 1024 + 5_120);
        assert_eq!(estimate_qbit_label_snapshot_bytes(10), 8 * 1024 + 2_560);
        assert_eq!(estimate_qbit_limit_map_snapshot_bytes(10), 8 * 1024 + 1_920);
        assert_eq!(estimate_qbit_properties_snapshot_bytes(), 32 * 1024);
        assert_eq!(estimate_qbit_peer_snapshot_bytes(10), 8 * 1024 + 10_240);
    }
}
