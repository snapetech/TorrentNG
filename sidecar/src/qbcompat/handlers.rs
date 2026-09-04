use axum::{
    extract::{Form, Multipart, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{AppendHeaders, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    fmt,
    net::SocketAddr,
    sync::{atomic::Ordering, Arc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinSet;

// qBittorrent's maindata protocol has no page or snapshot parameter.  Keep
// its compatibility response bounded and reject an over-limit sync instead
// of returning a truncated object that claims to be a full update.
const MAX_QBIT_SYNC_ENTRIES: usize = 10_000;

use crate::{
    api::{server::AppState, ws::Event},
    backend::{ratio_milli, BackendPeer, BackendPieceState, BackendStatus, QueueMove},
    cache::{
        bounded_page_limit, validate_page_offset, AppEventRow, ListParams, RssRule,
        RssRuleRenameResult, TorrentRow,
    },
    rtorrent::TransferRates,
};

fn backend_error_status(error: &anyhow::Error) -> StatusCode {
    if is_unsupported_error(error) {
        StatusCode::NOT_IMPLEMENTED
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

fn is_unsupported_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().contains("does not support"))
}

#[derive(Debug)]
struct InvalidHashTarget(String);

impl fmt::Display for InvalidHashTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for InvalidHashTarget {}

fn hash_resolution_status(error: &anyhow::Error) -> StatusCode {
    if error.downcast_ref::<InvalidHashTarget>().is_some() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

pub fn build_router(_state: AppState) -> Router<AppState> {
    Router::new()
        // Auth
        .route("/auth/login", post(auth_login))
        .route("/auth/logout", post(auth_logout))
        // App
        .route("/app/version", get(app_version).post(app_version))
        .route(
            "/app/webapiVersion",
            get(app_api_version).post(app_api_version),
        )
        .route("/app/buildInfo", get(app_build_info).post(app_build_info))
        .route(
            "/app/preferences",
            get(app_preferences).post(app_preferences),
        )
        .route(
            "/app/defaultSavePath",
            get(app_default_save_path).post(app_default_save_path),
        )
        // Torrents
        .route("/torrents/info", get(torrents_info))
        .route("/torrents/properties", get(torrents_properties))
        .route("/torrents/add", post(torrents_add))
        .route("/torrents/start", post(torrents_resume))
        .route("/torrents/stop", post(torrents_pause))
        .route("/torrents/pause", post(torrents_pause))
        .route("/torrents/resume", post(torrents_resume))
        .route("/torrents/delete", post(torrents_delete))
        .route("/torrents/recheck", post(torrents_recheck))
        .route("/torrents/reannounce", post(torrents_reannounce))
        .route("/torrents/trackers", get(torrents_trackers))
        .route("/torrents/export", get(torrents_export))
        .route("/torrents/webseeds", get(torrents_webseeds))
        .route("/torrents/files", get(torrents_files))
        .route("/torrents/pieceStates", get(torrents_piece_states))
        .route("/torrents/pieceHashes", get(torrents_piece_hashes))
        .route("/torrents/setCategory", post(torrents_set_category))
        .route("/torrents/addTags", post(torrents_add_tags))
        .route("/torrents/removeTags", post(torrents_remove_tags))
        .route("/torrents/setTags", post(torrents_set_tags))
        .route("/torrents/addPeers", post(torrents_add_peers))
        .route("/torrents/editTracker", post(torrents_edit_tracker))
        .route("/torrents/addTrackers", post(torrents_add_trackers))
        .route("/torrents/removeTrackers", post(torrents_remove_trackers))
        .route("/torrents/increasePrio", post(torrents_increase_prio))
        .route("/torrents/decreasePrio", post(torrents_decrease_prio))
        .route("/torrents/topPrio", post(torrents_top_prio))
        .route("/torrents/bottomPrio", post(torrents_bottom_prio))
        .route("/torrents/filePrio", post(torrents_file_prio))
        .route("/torrents/rename", post(torrents_rename))
        .route("/torrents/renameFile", post(torrents_rename_file))
        .route("/torrents/renameFolder", post(torrents_rename_file))
        .route("/torrents/downloadLimit", get(torrents_download_limit))
        .route("/torrents/setDownloadLimit", post(torrents_set_download_limit))
        .route("/torrents/uploadLimit", get(torrents_upload_limit))
        .route("/torrents/setUploadLimit", post(torrents_set_upload_limit))
        .route("/torrents/setShareLimits", post(torrents_set_share_limits))
        .route("/torrents/setLocation", post(torrents_set_location))
        .route("/torrents/setSavePath", post(torrents_set_location))
        .route("/torrents/setAutoManagement", post(torrents_set_auto_management))
        .route("/torrents/setAutoTMM", post(torrents_set_auto_tmm))
        .route("/torrents/setForceStart", post(torrents_set_force_start))
        .route("/torrents/setSuperSeeding", post(torrents_set_super_seeding))
        .route(
            "/torrents/toggleSequentialDownload",
            post(torrents_toggle_sequential_download),
        )
        .route(
            "/torrents/toggleFirstLastPiecePrio",
            post(torrents_toggle_first_last_piece_prio),
        )
        .route("/torrents/categories", get(categories))
        .route("/torrents/createCategory", post(create_category))
        .route("/torrents/editCategory", post(edit_category))
        .route("/torrents/removeCategories", post(remove_categories))
        .route("/torrents/tags", get(tags))
        .route("/torrents/createTags", post(create_tags))
        .route("/torrents/deleteTags", post(delete_tags))
        // Sync / transfer
        .route("/sync/maindata", get(sync_maindata))
        .route("/transfer/info", get(transfer_info))
        .route("/transfer/speedLimitsMode", get(transfer_speed_limits_mode))
        .route(
            "/transfer/toggleSpeedLimitsMode",
            post(transfer_toggle_speed_limits_mode),
        )
        .route("/transfer/downloadLimit", get(transfer_download_limit))
        .route("/transfer/setDownloadLimit", post(transfer_set_download_limit))
        .route("/transfer/uploadLimit", get(transfer_upload_limit))
        .route("/transfer/setUploadLimit", post(transfer_set_upload_limit))
        .route("/transfer/banPeers", post(transfer_ban_peers))
        .route("/log/main", get(log_main))
        .route("/log/peers", get(log_peers))
        .route("/search/status", get(search_status))
        .route("/search/categories", get(search_categories))
        .route("/search/plugins", get(search_plugins))
        .route("/search/installPlugin", post(search_install_plugin))
        .route("/search/uninstallPlugin", post(search_uninstall_plugin))
        .route("/search/enablePlugin", post(search_enable_plugin))
        .route("/search/updatePlugins", post(search_update_plugins))
        .route("/search/start", post(search_start))
        .route("/search/stop", post(search_stop))
        .route("/search/results", get(search_results))
        .route("/search/delete", post(search_delete))
        .route("/rss/items", get(rss_items))
        .route("/rss/addFolder", post(rss_add_folder))
        .route("/rss/addFeed", post(rss_add_feed))
        .route("/rss/removeItem", post(rss_remove_item))
        .route("/rss/moveItem", post(rss_move_item))
        .route("/rss/markAsRead", post(rss_mark_as_read))
        .route("/rss/refreshItem", post(rss_refresh_item))
        .route("/rss/setRule", post(rss_set_rule))
        .route("/rss/renameRule", post(rss_rename_rule))
        .route("/rss/removeRule", post(rss_remove_rule))
        .route("/rss/rules", get(rss_rules))
        .route("/rss/matchingArticles", get(rss_matching_articles))
        .route("/app/setPreferences", post(app_set_preferences))
}

// --- Auth ---

pub(crate) async fn auth_login(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    if s.cfg.auth.api_tokens.is_empty() {
        return "Ok.".into_response();
    }

    let candidate = f
        .get("password")
        .or_else(|| f.get("username"))
        .map(String::as_str)
        .unwrap_or("");

    if s.cfg.auth.api_tokens.iter().any(|token| token == candidate) {
        // API tokens are operator-provided strings, not cookie-safe strings.
        // Encode the value before putting it in a header; the auth middleware
        // decodes it again before comparison.
        let cookie_value =
            crate::auth::session_cookie_value(s.cfg.auth.secret_key.as_deref(), candidate);
        let tng_cookie = format!("tng_session={cookie_value}; Path=/; HttpOnly; SameSite=Lax");
        let sid_cookie = format!("SID={cookie_value}; Path=/; HttpOnly; SameSite=Lax");
        (
            AppendHeaders([
                (header::SET_COOKIE, tng_cookie),
                (header::SET_COOKIE, sid_cookie),
            ]),
            "Ok.",
        )
            .into_response()
    } else {
        "Fails.".into_response()
    }
}
pub(crate) async fn auth_logout() -> impl IntoResponse {
    (
        AppendHeaders([
            (
                header::SET_COOKIE,
                "tng_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
            ),
            (
                header::SET_COOKIE,
                "SID=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
            ),
        ]),
        StatusCode::OK,
    )
}

#[derive(Debug, Deserialize)]
struct LogMainQuery {
    limit: Option<usize>,
    last_known_id: Option<i64>,
    normal: Option<bool>,
    info: Option<bool>,
    warning: Option<bool>,
    critical: Option<bool>,
}

#[derive(Debug, Serialize)]
struct QbLogEntry {
    id: i64,
    message: String,
    timestamp: i64,
    #[serde(rename = "type")]
    kind: i64,
}

async fn log_main(State(s): State<AppState>, Query(q): Query<LogMainQuery>) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let levels = q.included_levels();
    match s
        .db
        .run_blocking("qbit_log_main", move |db| {
            db.list_app_events_filtered(limit, None, &levels, q.last_known_id)
        })
        .await
    {
        Ok(events) => match events
            .into_iter()
            .map(qbit_log_entry)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(events) => (
                StatusCode::OK,
                Json(
                    events
                        .into_iter()
                        .filter(|entry| q.includes_type(entry.kind))
                        .collect::<Vec<_>>(),
                ),
            )
                .into_response(),
            Err(e) => {
                tracing::warn!(
                    component = "api",
                    operation = "log_main",
                    result = "error",
                    error = %e,
                    "failed to project app event"
                );
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        },
        Err(e) => {
            tracing::warn!(
                component = "api",
                operation = "log_main",
                result = "error",
                error = %e,
                "failed to read app events"
            );
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn log_peers(State(s): State<AppState>) -> impl IntoResponse {
    const MAX_TORRENTS: i64 = 1_000;
    const MAX_ENTRIES: usize = 10_000;
    const BATCH_SIZE: usize = 32;

    if !s.backend.capabilities().supports_peer_snapshots {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }

    let hashes = match s
        .db
        .run_blocking("qbit_log_peers_torrents", |db| {
            db.list_page(&ListParams {
                // qBittorrent's peer log has no pagination contract. Keep this
                // compatibility projection bounded instead of issuing one backend
                // request for every cached torrent in a large library.
                limit: Some(MAX_TORRENTS),
                ..Default::default()
            })
        })
        .await
    {
        Ok(rows) => rows.into_iter().map(|row| row.hash).collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!(
                component = "api",
                operation = "log_peers",
                result = "error",
                error = %e,
                "failed to read torrent cache for peer log"
            );
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };

    let mut entries = Vec::new();
    let mut first_error: Option<anyhow::Error> = None;
    for batch in hashes.chunks(BATCH_SIZE) {
        let mut tasks = JoinSet::new();
        for hash in batch {
            let backend = Arc::clone(&s.backend);
            let hash = hash.clone();
            tasks.spawn(async move { (hash.clone(), backend.list_peers(&hash).await) });
        }
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((hash, Ok(peers))) => {
                    entries.extend(
                        peers
                            .into_iter()
                            .map(|peer| qbit_peer_log_entry(&hash, peer))
                            .take(MAX_ENTRIES.saturating_sub(entries.len())),
                    );
                }
                Ok((hash, Err(error))) => {
                    first_error.get_or_insert_with(|| {
                        anyhow::anyhow!("peer snapshot for {hash} failed: {error:#}")
                    });
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| {
                        anyhow::anyhow!("peer snapshot task failed: {error}")
                    });
                }
            };
        }
        if entries.len() >= MAX_ENTRIES {
            break;
        }
    }
    if let Some(error) = first_error {
        tracing::warn!(
            component = "api",
            operation = "log_peers",
            result = "error",
            error = %error,
            "failed to read peer snapshots"
        );
        return backend_error_status(&error).into_response();
    }
    (StatusCode::OK, Json(entries)).into_response()
}

impl LogMainQuery {
    fn included_levels(&self) -> Vec<&'static str> {
        let any_filter = self.normal.is_some()
            || self.info.is_some()
            || self.warning.is_some()
            || self.critical.is_some();
        if !any_filter {
            return Vec::new();
        }

        let mut levels = Vec::new();
        if self.normal.unwrap_or(false) || self.info.unwrap_or(false) {
            levels.push("info");
        }
        if self.warning.unwrap_or(false) {
            levels.push("warn");
            levels.push("warning");
        }
        if self.critical.unwrap_or(false) {
            levels.push("error");
            levels.push("critical");
        }
        levels
    }

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
}

fn qbit_log_entry(row: crate::cache::AppEventRow) -> Result<QbLogEntry, String> {
    let event_id = row
        .event_id
        .ok_or_else(|| "app event is missing event_id".to_owned())?;
    serde_json::from_str::<serde_json::Value>(&row.payload)
        .map_err(|error| format!("app event payload is invalid JSON: {error}"))?;
    Ok(QbLogEntry {
        id: event_id,
        message: row.message,
        timestamp: row.occurred_at,
        kind: match row.level.as_str() {
            "error" | "critical" => 4,
            "warn" | "warning" => 2,
            _ => 1,
        },
    })
}

fn qbit_peer_log_entry(info_hash: &str, peer: BackendPeer) -> serde_json::Value {
    json!({
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

async fn search_status(State(s): State<AppState>) -> Json<serde_json::Value> {
    let jobs = s.qbit_search_jobs.read().await;
    let running = jobs
        .values()
        .any(|job| job.get("status").and_then(|v| v.as_str()) == Some("Running"));
    let plugins = s
        .qbit_search_plugins
        .read()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    Json(json!({
        "status": if running { "Running" } else { "Stopped" },
        "plugins": plugins,
    }))
}

async fn search_plugins(State(s): State<AppState>) -> Json<serde_json::Value> {
    let plugins = s
        .qbit_search_plugins
        .read()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    Json(json!(plugins))
}

async fn search_categories(State(s): State<AppState>) -> Json<serde_json::Value> {
    let plugins = s.qbit_search_plugins.read().await;
    let mut categories = std::collections::BTreeSet::new();
    for plugin in plugins.values() {
        if plugin.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        let Some(values) = plugin
            .get("supportedCategories")
            .and_then(|value| value.as_array())
        else {
            continue;
        };
        for category in values.iter().filter_map(|value| value.as_str()) {
            let category = category.trim();
            if !category.is_empty() {
                categories.insert(category.to_owned());
            }
        }
    }
    Json(json!(categories.into_iter().collect::<Vec<_>>()))
}

async fn search_install_plugin(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let sources = match required_qbit_form_list(&f, "sources") {
        Ok(sources) => sources,
        Err(status) => return status,
    };
    let mut plugins = s.qbit_search_plugins.write().await;
    for source in sources {
        let name = plugin_name_from_source(&source);
        plugins.insert(name.clone(), search_plugin_value(&name, &source, true));
    }
    StatusCode::OK
}

async fn search_uninstall_plugin(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let names = match required_qbit_form_list(&f, "names") {
        Ok(names) => names,
        Err(status) => return status,
    };
    let mut plugins = s.qbit_search_plugins.write().await;
    for name in names {
        plugins.remove(&name);
    }
    StatusCode::OK
}

async fn search_enable_plugin(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(enabled) = f.get("enable").and_then(|value| parse_wire_bool(value)) else {
        return StatusCode::BAD_REQUEST;
    };
    let names = match required_qbit_form_list(&f, "names") {
        Ok(names) => names,
        Err(status) => return status,
    };
    let mut plugins = s.qbit_search_plugins.write().await;
    for name in names {
        let entry = plugins
            .entry(name.clone())
            .or_insert_with(|| search_plugin_value(&name, "", enabled));
        if let Some(map) = entry.as_object_mut() {
            map.insert("enabled".into(), enabled.into());
        }
    }
    StatusCode::OK
}

async fn search_update_plugins() -> StatusCode {
    StatusCode::OK
}

async fn search_start(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(pattern) = f
        .get("pattern")
        .map(String::as_str)
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let id = s.qbit_next_search_id.fetch_add(1, Ordering::Relaxed);
    let job = json!({
        "id": id,
        "pattern": pattern,
        "plugins": f.get("plugins").cloned().unwrap_or_else(|| "all".to_owned()),
        "category": f.get("category").cloned().unwrap_or_else(|| "all".to_owned()),
        "status": "Stopped",
        "total": 0,
        "results": [],
    });
    s.qbit_search_jobs.write().await.insert(id.to_string(), job);
    Json(json!({ "id": id })).into_response()
}

async fn search_stop(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(id) = f.get("id").filter(|id| !id.trim().is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut jobs = s.qbit_search_jobs.write().await;
    let Some(job) = jobs.get_mut(id) else {
        return StatusCode::NOT_FOUND;
    };
    if let Some(map) = job.as_object_mut() {
        map.insert("status".into(), "Stopped".into());
    }
    StatusCode::OK
}

async fn search_results(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let requested_id = match q.get("id") {
        Some(raw) => match raw.parse::<u64>() {
            Ok(id) => Some(id.to_string()),
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => None,
    };
    let offset = match q.get("offset") {
        Some(raw) => match raw.parse::<usize>() {
            Ok(offset) => offset,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => 0,
    };
    let requested_limit = match q.get("limit") {
        Some(raw) => match raw.parse::<usize>() {
            Ok(limit) => Some(limit),
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => None,
    };

    let jobs = s.qbit_search_jobs.read().await;
    let job = match requested_id.as_deref() {
        Some(id) => jobs.get(id),
        None => jobs.iter().next_back().map(|(_, job)| job),
    };
    let Some(job) = job else {
        if requested_id.is_some() {
            return StatusCode::NOT_FOUND.into_response();
        }
        return (
            StatusCode::OK,
            Json(json!({
            "status": "Stopped",
            "total": 0,
            "results": [],
            })),
        )
            .into_response();
    };
    let mut response = job.clone();
    if let Some(map) = response.as_object_mut() {
        let results = map
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let limit = requested_limit.unwrap_or_else(|| results.len().saturating_sub(offset));
        let sliced = results
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        map.insert("results".into(), serde_json::Value::Array(sliced));
    }
    (StatusCode::OK, Json(response)).into_response()
}

async fn search_delete(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(id) = f.get("id").filter(|id| !id.trim().is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    if s.qbit_search_jobs.write().await.remove(id).is_none() {
        return StatusCode::NOT_FOUND;
    }
    StatusCode::OK
}

async fn rss_items(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::Value::Object(
        s.qbit_rss_items.read().await.clone(),
    ))
}

async fn rss_add_folder(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(path) = f.get("path").filter(|p| !p.trim().is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    s.qbit_rss_items.write().await.insert(
        path.clone(),
        json!({
            "uid": path,
            "name": rss_leaf_name(path),
            "type": "folder",
            "isLoading": false,
            "hasError": false,
            "articles": [],
        }),
    );
    StatusCode::OK
}

async fn rss_add_feed(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(url) = f.get("url").filter(|u| !u.trim().is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    let path = f
        .get("path")
        .filter(|p| !p.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| url.clone());
    s.qbit_rss_items.write().await.insert(
        path.clone(),
        json!({
            "uid": path,
            "name": rss_leaf_name(&path),
            "type": "feed",
            "url": url,
            "isLoading": false,
            "hasError": false,
            "articles": [],
        }),
    );
    StatusCode::OK
}

async fn rss_remove_item(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(path) = f.get("path").filter(|path| !path.trim().is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    if s.qbit_rss_items.write().await.remove(path).is_none() {
        return StatusCode::NOT_FOUND;
    }
    StatusCode::OK
}

async fn rss_move_item(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(item_path) = f.get("itemPath") else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(dest_path) = f.get("destPath") else {
        return StatusCode::BAD_REQUEST;
    };
    if item_path.trim().is_empty() || dest_path.trim().is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    let mut items = s.qbit_rss_items.write().await;
    if item_path == dest_path {
        return StatusCode::OK;
    }
    if items.contains_key(dest_path) {
        return StatusCode::CONFLICT;
    }
    let Some(mut item) = items.remove(item_path) else {
        return StatusCode::NOT_FOUND;
    };
    if let Some(map) = item.as_object_mut() {
        map.insert("uid".into(), dest_path.clone().into());
        map.insert("name".into(), rss_leaf_name(dest_path).into());
    }
    items.insert(dest_path.clone(), item);
    StatusCode::OK
}

async fn rss_mark_as_read(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(item_path) = f.get("itemPath").filter(|path| !path.trim().is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut items = s.qbit_rss_items.write().await;
    let Some(item) = items.get_mut(item_path) else {
        return StatusCode::NOT_FOUND;
    };
    if let Some(map) = item.as_object_mut() {
        map.insert("read".into(), true.into());
    } else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::OK
}

async fn rss_refresh_item(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(item_path) = f.get("itemPath").filter(|path| !path.trim().is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut items = s.qbit_rss_items.write().await;
    let Some(item) = items.get_mut(item_path) else {
        return StatusCode::NOT_FOUND;
    };
    if let Some(map) = item.as_object_mut() {
        map.insert("lastBuildDate".into(), now_unix_secs().into());
    } else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::OK
}

async fn rss_rules(State(s): State<AppState>) -> impl IntoResponse {
    match s
        .db
        .run_blocking("qbit_list_rss_rules", |db| db.list_rss_rules())
        .await
    {
        Ok(rules) => {
            let map: serde_json::Map<String, serde_json::Value> = rules
                .into_iter()
                .map(|rule| {
                    (
                        rule.name.clone(),
                        json!({
                            "name": rule.name,
                            "enabled": rule.enabled,
                            "affectedFeeds": [rule.feed_url],
                            "mustContain": rule.include,
                            "mustNotContain": rule.exclude.unwrap_or_default(),
                            "assignedCategory": rule.category.unwrap_or_default(),
                            "savePath": rule.save_path.unwrap_or_default(),
                            "addPaused": !rule.start,
                            "tags": rule.tags.join(","),
                        }),
                    )
                })
                .collect();
            Json(serde_json::Value::Object(map)).into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_rss_rules",
                result = "error",
                error = %e,
                "qBit RSS rule listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
struct RssSetRuleForm {
    #[serde(rename = "ruleName")]
    rule_name: Option<String>,
    rule: Option<String>,
    #[serde(rename = "ruleDef")]
    rule_def: Option<String>,
}

async fn rss_set_rule(State(s): State<AppState>, Form(f): Form<RssSetRuleForm>) -> StatusCode {
    let Some(name) = f
        .rule_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(raw) = f.rule.as_deref().or(f.rule_def.as_deref()) else {
        return StatusCode::BAD_REQUEST;
    };
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "set_rss_rule",
                result = "error",
                error = %e,
                "qBit RSS rule JSON parse failed"
            );
            return StatusCode::BAD_REQUEST;
        }
    };
    if !value.is_object() {
        return StatusCode::BAD_REQUEST;
    }
    let feed_url = match rss_required_feed_url(&value) {
        Ok(feed_url) => feed_url,
        Err(()) => return StatusCode::BAD_REQUEST,
    };
    let include = match rss_required_string_alias(&value, &["mustContain", "contains"]) {
        Ok(include) => include,
        Err(()) => return StatusCode::BAD_REQUEST,
    };
    let enabled = match rss_optional_bool(&value, "enabled", true) {
        Ok(enabled) => enabled,
        Err(()) => return StatusCode::BAD_REQUEST,
    };
    let exclude = match rss_optional_string(&value, "mustNotContain") {
        Ok(exclude) => exclude.filter(|value| !value.trim().is_empty()),
        Err(()) => return StatusCode::BAD_REQUEST,
    };
    let category = match rss_optional_string(&value, "assignedCategory") {
        Ok(category) => category.filter(|value| !value.trim().is_empty()),
        Err(()) => return StatusCode::BAD_REQUEST,
    };
    let save_path = match rss_optional_string(&value, "savePath") {
        Ok(save_path) => save_path.filter(|value| !value.trim().is_empty()),
        Err(()) => return StatusCode::BAD_REQUEST,
    };
    let tags = match rss_optional_string(&value, "tags") {
        Ok(tags) => tags
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect(),
        Err(()) => return StatusCode::BAD_REQUEST,
    };
    let add_paused = match rss_optional_bool(&value, "addPaused", false) {
        Ok(add_paused) => add_paused,
        Err(()) => return StatusCode::BAD_REQUEST,
    };
    let rule = RssRule {
        id: String::new(),
        name: name.to_owned(),
        enabled,
        feed_url,
        include,
        exclude,
        category,
        save_path,
        tags,
        start: !add_paused,
    };
    match s
        .db
        .run_blocking("qbit_upsert_rss_rule", move |db| db.upsert_rss_rule(rule))
        .await
    {
        Ok(_) => {
            emit(&s, Event::RssRulesUpdated).await;
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "set_rss_rule",
                rule = %name,
                result = "error",
                error = %e,
                "qBit RSS rule update failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn rss_required_feed_url(value: &serde_json::Value) -> Result<String, ()> {
    let feeds = value
        .get("affectedFeeds")
        .and_then(serde_json::Value::as_array)
        .ok_or(())?;
    if feeds.len() != 1 {
        // The durable RSS rule model has one feed URL. Refuse to silently
        // discard additional qBittorrent feeds until the model can represent
        // them without changing matching semantics.
        return Err(());
    }
    let feed = feeds
        .first()
        .and_then(serde_json::Value::as_str)
        .ok_or(())?;
    if feed.trim().is_empty() {
        return Err(());
    }
    Ok(feed.to_owned())
}

fn rss_required_string_alias(value: &serde_json::Value, keys: &[&str]) -> Result<String, ()> {
    let raw = keys
        .iter()
        .find_map(|key| value.get(*key))
        .and_then(serde_json::Value::as_str)
        .ok_or(())?;
    if raw.trim().is_empty() {
        return Err(());
    }
    Ok(raw.to_owned())
}

fn rss_optional_string(value: &serde_json::Value, key: &str) -> Result<Option<String>, ()> {
    match value.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(raw) => raw.as_str().map(|value| Some(value.to_owned())).ok_or(()),
    }
}

fn rss_optional_bool(value: &serde_json::Value, key: &str, default: bool) -> Result<bool, ()> {
    match value.get(key) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(raw) => raw.as_bool().ok_or(()),
    }
}

#[derive(Deserialize)]
struct RssRenameRuleForm {
    #[serde(rename = "ruleName")]
    rule_name: Option<String>,
    #[serde(rename = "newRuleName")]
    new_rule_name: Option<String>,
}

async fn rss_rename_rule(
    State(s): State<AppState>,
    Form(f): Form<RssRenameRuleForm>,
) -> StatusCode {
    let Some(old_name) = f
        .rule_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_name) = f
        .new_rule_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let old_name_for_db = old_name.to_owned();
    let new_name_for_db = new_name.to_owned();
    match s
        .db
        .run_blocking("qbit_rename_rss_rule", move |db| {
            db.rename_rss_rule(&old_name_for_db, &new_name_for_db)
        })
        .await
    {
        Ok(RssRuleRenameResult::Renamed) => {
            emit(&s, Event::RssRulesUpdated).await;
            StatusCode::OK
        }
        Ok(RssRuleRenameResult::Missing) => StatusCode::NOT_FOUND,
        Ok(RssRuleRenameResult::Conflict) => StatusCode::CONFLICT,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "rename_rss_rule",
                rule = %old_name,
                new_rule = %new_name,
                result = "error",
                error = %e,
                "qBit RSS rule rename failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Deserialize)]
struct RssRemoveRuleForm {
    #[serde(rename = "ruleName")]
    rule_name: Option<String>,
}

async fn rss_remove_rule(
    State(s): State<AppState>,
    Form(f): Form<RssRemoveRuleForm>,
) -> StatusCode {
    let Some(name) = f
        .rule_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let name_for_db = name.to_owned();
    match s
        .db
        .run_blocking("qbit_delete_rss_rule", move |db| {
            db.delete_rss_rule_by_name(&name_for_db)
        })
        .await
    {
        Ok(true) => {
            emit(&s, Event::RssRulesUpdated).await;
            StatusCode::OK
        }
        Ok(false) => StatusCode::NOT_FOUND,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "remove_rss_rule",
                rule = %name,
                result = "error",
                error = %e,
                "qBit RSS rule removal failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Deserialize)]
struct RssMatchingQuery {
    article: Option<String>,
}

async fn rss_matching_articles(
    State(s): State<AppState>,
    Query(q): Query<RssMatchingQuery>,
) -> impl IntoResponse {
    let Some(title) = q
        .article
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Json(json!([])).into_response();
    };
    let title = title.to_owned();
    match s
        .db
        .run_blocking("qbit_match_rss_item", move |db| {
            db.match_rss_item(&title, None)
        })
        .await
    {
        Ok(matches) => Json(json!(matches
            .into_iter()
            .filter(|m| m.matched)
            .map(|m| m.rule_name)
            .collect::<Vec<_>>()))
        .into_response(),
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "match_rss_articles",
                result = "error",
                error = %e,
                "qBit RSS article matching failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- App ---

async fn app_version(State(s): State<AppState>) -> String {
    s.cfg.identity.qbittorrent_version.clone()
}
async fn app_api_version(State(s): State<AppState>) -> String {
    s.cfg.identity.qbittorrent_webapi_version.clone()
}
async fn app_build_info(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "qt": s.cfg.identity.qbittorrent_build_qt,
        "libtorrent": s.cfg.identity.qbittorrent_build_libtorrent,
        "boost": "",
        "openssl": "",
        "bitness": 64,
    }))
}
async fn app_preferences(State(s): State<AppState>) -> Json<serde_json::Value> {
    let (dht, pex) = s.backend.feature_status().await;
    let mut prefs = json!({
        "save_path": "/data/downloads",
        "queueing_enabled": false,
        "max_active_torrents": -1,
        "dht": feature_status_to_bool(&dht),
        "pex": feature_status_to_bool(&pex),
    });
    if s.backend.capabilities().supports_runtime_user_agent {
        if let Ok(user_agent) = s.backend.get_user_agent().await {
            prefs["network_http_user_agent"] = json!(user_agent);
        }
    }
    Json(prefs)
}
async fn app_default_save_path() -> &'static str {
    "/data/downloads"
}

async fn app_set_preferences(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(raw_prefs) = f.get("json") else {
        return StatusCode::BAD_REQUEST;
    };
    let prefs: serde_json::Value = match serde_json::from_str(raw_prefs) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "set_preferences",
                result = "error",
                error = %e,
                "qBit preferences JSON parse failed"
            );
            return StatusCode::BAD_REQUEST;
        }
    };
    if !prefs.is_object() {
        return StatusCode::BAD_REQUEST;
    }
    // Validate every backend-backed field before applying any of them. A
    // malformed field must not be silently ignored while a sibling setting
    // is committed, leaving the caller with a partially understood request.
    let dht = match qbit_preference_bool(&prefs, "dht") {
        Ok(value) => value,
        Err(status) => return status,
    };
    let pex = match qbit_preference_bool(&prefs, "pex") {
        Ok(value) => value,
        Err(status) => return status,
    };
    let user_agent = match prefs.get("network_http_user_agent") {
        None => None,
        Some(value) => match value.as_str() {
            Some(value) => Some(value),
            None => return StatusCode::BAD_REQUEST,
        },
    };

    let mut backend_failed = false;
    let mut unsupported = false;
    for (setting, enabled) in [("dht", dht), ("pex", pex)] {
        if let Some(enabled) = enabled {
            let result = match setting {
                "dht" => s.backend.set_dht(enabled).await,
                "pex" => s.backend.set_pex(enabled).await,
                _ => unreachable!(),
            };
            match result {
                Ok(_) => {
                    record_operator_event(
                        &s,
                        "info",
                        "settings_changed",
                        "qBittorrent preferences updated backend session feature",
                        serde_json::json!({
                            "component": "qbcompat",
                            "backend": s.backend.backend_type().as_str(),
                            "operation": "set_preferences",
                            "setting": setting,
                            "enabled": enabled,
                            "result": "updated",
                        }),
                    )
                    .await
                }
                Err(e) => {
                    backend_failed = true;
                    tracing::debug!(
                        component = "qbcompat",
                        backend = s.backend.backend_type().as_str(),
                        operation = "set_preferences",
                        setting,
                        enabled,
                        result = "unsupported",
                        error = %e,
                        "qBit session feature preference could not be applied by backend"
                    );
                }
            }
        }
    }

    if let Some(ua) = user_agent {
        if !s.backend.capabilities().supports_runtime_user_agent {
            unsupported = true;
            tracing::debug!(
                component = "qbcompat",
                backend = s.backend.backend_type().as_str(),
                operation = "set_preferences",
                setting = "network_http_user_agent",
                result = "unsupported",
                "qBit user-agent preference ignored because backend does not support runtime user-agent updates"
            );
        } else {
            match s.backend.set_user_agent(ua).await {
                Ok(_) => {
                    record_operator_event(
                        &s,
                        "info",
                        "settings_changed",
                        "qBittorrent preferences updated backend user agent",
                        serde_json::json!({
                            "component": "qbcompat",
                            "backend": s.backend.backend_type().as_str(),
                            "operation": "set_preferences",
                            "setting": "network_http_user_agent",
                            "result": "updated",
                            "user_agent_len": ua.len(),
                        }),
                    )
                    .await
                }
                Err(e) => {
                    backend_failed = true;
                    tracing::warn!(
                        component = "qbcompat",
                        operation = "set_user_agent",
                    result = "error",
                        error = %e,
                        "qBit user-agent preference update failed"
                    );
                    record_operator_event(
                        &s,
                        "warn",
                        "rtorrent_user_agent_error",
                        "qBittorrent preference update could not apply backend user agent",
                        serde_json::json!({
                            "component": "qbcompat",
                            "backend": s.backend.backend_type().as_str(),
                            "operation": "set_preferences",
                            "setting": "network_http_user_agent",
                            "result": "error",
                            "error": e.to_string(),
                        }),
                    )
                    .await;
                }
            }
        }
    }

    if backend_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else if unsupported {
        StatusCode::NOT_IMPLEMENTED
    } else {
        StatusCode::OK
    }
}

fn qbit_preference_bool(
    preferences: &serde_json::Value,
    key: &str,
) -> Result<Option<bool>, StatusCode> {
    match preferences.get(key) {
        None => Ok(None),
        Some(value) => value.as_bool().map(Some).ok_or(StatusCode::BAD_REQUEST),
    }
}

fn feature_status_to_bool(status: &str) -> Option<bool> {
    match status.trim().to_ascii_lowercase().as_str() {
        "on" | "enabled" | "enable" | "true" | "1" | "yes" => Some(true),
        "off" | "disabled" | "disable" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

// --- Torrents ---

#[derive(Deserialize)]
struct InfoQuery {
    filter: Option<String>,
    category: Option<String>,
    tag: Option<String>,
    sort: Option<String>,
    reverse: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn torrents_info(State(s): State<AppState>, Query(q): Query<InfoQuery>) -> impl IntoResponse {
    let status = match qbit_status_filter(q.filter.as_deref()) {
        Ok(status) => status,
        Err(status) => return status.into_response(),
    };
    let limit = bounded_page_limit(q.limit.map(|limit| limit.min(5_000) as i64));
    let offset = match q.offset {
        Some(offset) => match i64::try_from(offset) {
            Ok(offset) if validate_page_offset(Some(offset)).is_ok() => Some(offset),
            Ok(_) => return StatusCode::BAD_REQUEST.into_response(),
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => None,
    };
    let params = ListParams {
        // qBittorrent's `filter` is a finite status enum, not a free-text
        // search. Passing an unknown value through as ListParams::filter
        // makes the cache search torrent names and can return a successful
        // but semantically unrelated response.
        filter: None,
        status,
        category: q.category,
        tag: q.tag,
        tracker: None,
        media_type: None,
        sort: q.sort.as_deref().map(map_sort).map(String::from),
        dir: if q.reverse.unwrap_or(false) {
            Some("desc".into())
        } else {
            Some("asc".into())
        },
        limit,
        offset,
    };

    match s
        .db
        .run_blocking("qbit_torrents_info", move |db| db.list_page(&params))
        .await
    {
        Ok(rows) => Json(rows.iter().map(to_qb_torrent).collect::<Vec<_>>()).into_response(),
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "torrents_info",
                result = "error",
                error = %e,
                "qBit torrent list query failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
struct HashQuery {
    hash: Option<String>,
}

async fn torrents_properties(
    State(s): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let lookup_hash = hash.clone();
    match s
        .db
        .run_blocking("qbit_torrent_properties", move |db| db.get(&lookup_hash))
        .await
    {
        Ok(Some(t)) => Json(json!({
            "save_path": t.directory,
            "creation_date": t.creation_date,
            "piece_size": 0,
            "comment": "",
            "total_wasted": 0,
            "total_uploaded": t.up_total,
            "total_uploaded_session": 0,
            "total_downloaded": t.down_total,
            "total_downloaded_session": 0,
            "up_limit": -1,
            "dl_limit": -1,
            "time_elapsed": 0,
            "seeding_time": 0,
            "nb_connections": t.peers_connected,
            "nb_connections_limit": -1,
            "share_ratio": t.ratio as f64 / 1000.0,
            "addition_date": t.creation_date,
            "completion_date": t.timestamp_finished,
            "created_by": "",
            "dl_speed_avg": 0,
            "dl_speed": t.down_rate,
            "eta": if t.down_rate > 0 && t.size_bytes > t.bytes_done {
                (t.size_bytes - t.bytes_done) / t.down_rate
            } else {
                8_640_000
            },
            "last_seen": t.updated_at,
            "peers": t.peers_connected,
            "peers_total": t.peers_connected,
            "pieces_have": 0,
            "pieces_num": 0,
            "reannounce": 0,
            "seeds": t.peers_complete,
            "seeds_total": t.peers_complete,
            "total_size": t.size_bytes,
            "up_speed_avg": 0,
            "up_speed": t.up_rate,
        }))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "torrent_properties",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit torrent properties query failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn torrents_add(State(s): State<AppState>, mut multipart: Multipart) -> impl IntoResponse {
    // Try multipart first (with .torrent file), fall back to form
    let mut urls: Option<String> = None;
    let mut save_path = String::new();
    let mut category = String::new();
    let mut paused = false;
    let mut stopped = false;
    let mut torrent_data: Option<Vec<u8>> = None;
    let mut tags: Option<String> = None;
    let mut skip_checking = false;
    let mut content_layout: Option<String> = None;
    let mut auto_tmm: Option<bool> = None;
    let mut ratio_limit: Option<f64> = None;
    let mut seeding_time_limit: Option<i64> = None;

    loop {
        let Some(field) = (match multipart.next_field().await {
            Ok(field) => field,
            Err(error) => {
                tracing::warn!(
                    component = "qbcompat",
                    operation = "add_torrent",
                    result = "bad_request",
                    error = %error,
                    "invalid qBit multipart request"
                );
                return (StatusCode::BAD_REQUEST, "Fails.").into_response();
            }
        }) else {
            break;
        };
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("urls") => {
                urls = Some(match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "urls",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                });
            }
            Some("savepath") => {
                save_path = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "savepath",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                };
            }
            Some("category") => {
                category = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "category",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                };
            }
            Some("paused") => {
                let value = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "paused",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                };
                paused = match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return (StatusCode::BAD_REQUEST, "Fails.").into_response(),
                };
            }
            Some("stopped") => {
                let value = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "stopped",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                };
                stopped = match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return (StatusCode::BAD_REQUEST, "Fails.").into_response(),
                };
            }
            Some("tags") => {
                tags = Some(match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "tags",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                });
            }
            Some("skip_checking") => {
                let value = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "skip_checking",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                };
                skip_checking = match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return (StatusCode::BAD_REQUEST, "Fails.").into_response(),
                };
            }
            Some("contentLayout") => {
                content_layout = Some(match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "contentLayout",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                });
            }
            Some("autoTMM") => {
                let value = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "autoTMM",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                };
                auto_tmm = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return (StatusCode::BAD_REQUEST, "Fails.").into_response(),
                });
            }
            Some("ratioLimit") => {
                let value = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "ratioLimit",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                };
                ratio_limit = match value.parse::<f64>() {
                    Ok(value)
                        if (value.is_finite() && value >= 0.0)
                            || value == -1.0
                            || value == -2.0 =>
                    {
                        Some(value)
                    }
                    _ => return (StatusCode::BAD_REQUEST, "Fails.").into_response(),
                };
            }
            Some("seedingTimeLimit") => {
                let value = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "seedingTimeLimit",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart text field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                };
                seeding_time_limit = match value.parse::<i64>() {
                    Ok(value) if value >= 0 || value == -1 || value == -2 => Some(value),
                    _ => return (StatusCode::BAD_REQUEST, "Fails.").into_response(),
                };
            }
            Some("torrents") => {
                torrent_data = Some(match field.bytes().await {
                    Ok(value) => value.to_vec(),
                    Err(error) => {
                        tracing::warn!(
                            component = "qbcompat",
                            operation = "add_torrent",
                            field = "torrents",
                            result = "bad_request",
                            error = %error,
                            "invalid qBit multipart file field"
                        );
                        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                    }
                });
            }
            Some(name) => {
                let _ = field.text().await;
                tracing::info!(
                    component = "qbcompat",
                    operation = "add_torrent",
                    field = %name,
                    result = "unsupported",
                    "qBit add option has no sidecar backend contract"
                );
                return (StatusCode::NOT_IMPLEMENTED, "Fails.").into_response();
            }
            None => {
                let _ = field.text().await;
                return (StatusCode::BAD_REQUEST, "multipart field is missing a name")
                    .into_response();
            }
        }
    }

    // The generic sidecar backend has no add-time contract for these qBit
    // options. Silently dropping them reports a successful add while creating
    // a torrent with different scheduler/share semantics.
    if tags.as_deref().is_some_and(|tags| !tags.trim().is_empty())
        || skip_checking
        || content_layout.as_deref().is_some_and(|layout| {
            !layout.trim().is_empty() && !layout.eq_ignore_ascii_case("Original")
        })
        || auto_tmm == Some(true)
        || ratio_limit.is_some_and(|limit| limit != -2.0)
        || seeding_time_limit.is_some_and(|limit| limit != -2)
    {
        tracing::info!(
            component = "qbcompat",
            operation = "add_torrent",
            result = "unsupported",
            "qBit add request contains options without a sidecar backend contract"
        );
        return (StatusCode::NOT_IMPLEMENTED, "Fails.").into_response();
    }

    let start = !(paused || stopped);

    if let Some(url_list) = urls {
        let mut added = false;
        let normalized = url_list.replace("\r\n", "\n").replace('\r', "");
        let lines = normalized.split('\n').collect::<Vec<_>>();
        for (index, url) in lines.iter().enumerate() {
            let url = url.trim();
            if url.is_empty() {
                if index + 1 != lines.len() {
                    return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                }
                continue;
            }
            added = true;
            if let Err(e) = s.backend.add_url(url, &save_path, &category, start).await {
                tracing::error!(
                    component = "qbcompat",
                    operation = "add_url",
                    source = %redact_log_url(url),
                result = "error",
                    error = %e,
                    "qb add url failed"
                );
                return "Fails.".into_response();
            }
        }
        if added {
            return "Ok.".into_response();
        }
    }

    if let Some(data) = torrent_data {
        if data.is_empty() {
            return (StatusCode::BAD_REQUEST, "Fails.").into_response();
        }
        if let Err(e) = s
            .backend
            .add_torrent(&data, &save_path, &category, start)
            .await
        {
            tracing::error!(
                component = "qbcompat",
                operation = "add_torrent",
                result = "error",
                error = %e,
                "qb add torrent failed"
            );
            return "Fails.".into_response();
        }
        return "Ok.".into_response();
    }

    (StatusCode::BAD_REQUEST, "Fails.").into_response()
}

#[derive(Deserialize)]
struct HashesForm {
    hashes: Option<String>,
}

async fn torrents_pause(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    bulk_action(&s, &f.hashes, "stop").await
}
async fn torrents_resume(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    bulk_action(&s, &f.hashes, "start").await
}
async fn torrents_recheck(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    bulk_action(&s, &f.hashes, "recheck").await
}
async fn torrents_reannounce(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    bulk_action(&s, &f.hashes, "reannounce").await
}

#[derive(Deserialize)]
struct AddPeersForm {
    hashes: Option<String>,
    hash: Option<String>,
    peers: Option<String>,
}

async fn torrents_add_peers(State(s): State<AppState>, Form(f): Form<AddPeersForm>) -> StatusCode {
    let hashes = f.hashes.as_deref().or(f.hash.as_deref());
    let hashes = match required_resolved_hashes_async(&s.db, hashes).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for peer add"
            );
            return hash_resolution_status(&e);
        }
    };
    let peers = match f.peers.as_deref().map(parse_peer_addrs) {
        Some(Ok(peers)) => peers,
        Some(Err(_)) | None => return StatusCode::BAD_REQUEST,
    };
    if peers.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    if !s.backend.capabilities().supports_peer_add {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let mut failed = false;
    for hash in hashes {
        if let Err(e) = s.backend.add_peers(&hash, &peers).await {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "add_peers",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit explicit peer add failed"
            );
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn torrents_increase_prio(
    State(s): State<AppState>,
    Form(f): Form<HashesForm>,
) -> StatusCode {
    torrents_update_queue_order(s, f.hashes, QueueMove::Up).await
}

async fn torrents_decrease_prio(
    State(s): State<AppState>,
    Form(f): Form<HashesForm>,
) -> StatusCode {
    torrents_update_queue_order(s, f.hashes, QueueMove::Down).await
}

async fn torrents_top_prio(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    torrents_update_queue_order(s, f.hashes, QueueMove::Top).await
}

async fn torrents_bottom_prio(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    torrents_update_queue_order(s, f.hashes, QueueMove::Bottom).await
}

async fn torrents_update_queue_order(
    s: AppState,
    hashes: Option<String>,
    queue_move: QueueMove,
) -> StatusCode {
    let hashes = match required_resolved_hashes_async(&s.db, hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for queue update"
            );
            return hash_resolution_status(&e);
        }
    };
    if hashes.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    if !s.backend.capabilities().supports_queue_order {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let mut failed = false;
    if let Err(e) = s.backend.update_queue_order(&hashes, queue_move).await {
        failed = true;
        tracing::warn!(
            component = "qbcompat",
            operation = "update_queue_order",
            result = "error",
            error = %e,
            "qBit queue order update failed"
        );
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn bulk_action(s: &AppState, hashes_str: &Option<String>, action: &str) -> StatusCode {
    let hashes = match required_resolved_hashes_async(&s.db, hashes_str.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for bulk action"
            );
            return hash_resolution_status(&e);
        }
    };
    let mut failed = false;
    for hash in hashes {
        let res = match action {
            "start" => s.backend.start(&hash).await,
            "stop" => s.backend.stop(&hash).await,
            "recheck" => s.backend.recheck(&hash).await,
            "reannounce" => s.backend.reannounce(&hash).await,
            _ => Err(anyhow::anyhow!("unsupported bulk action {action}")),
        };
        if let Err(e) = res {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = %action,
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit torrent action failed"
            );
        } else if let Err(error) = update_cached_lifecycle_state(s, &hash, action).await {
            failed = true;
            tracing::warn!(
                component = "cache",
                operation = "set_torrent_runtime_state",
                torrent = %hash,
                action,
                result = "error",
                error = %error,
                "qBit action succeeded but cache projection failed"
            );
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

#[derive(Deserialize)]
struct DeleteForm {
    hashes: Option<String>,
    #[serde(rename = "deleteFiles")]
    delete_files: Option<String>,
}

async fn torrents_delete(State(s): State<AppState>, Form(f): Form<DeleteForm>) -> StatusCode {
    let delete_files = match f.delete_files.as_deref() {
        Some(value) => match parse_wire_bool(value) {
            Some(value) => value,
            None => return StatusCode::BAD_REQUEST,
        },
        None => false,
    };
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for delete"
            );
            return hash_resolution_status(&e);
        }
    };
    let mut backend_failed = false;
    let mut cache_failed = false;
    for hash in hashes {
        if let Err(e) = s.backend.remove(&hash, delete_files).await {
            backend_failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "delete_torrent",
                torrent = %hash,
                delete_files,
                result = "error",
                error = %e,
                "qBit delete failed"
            );
            continue;
        }
        let cache_hash = hash.clone();
        if let Err(e) =
            s.db.run_blocking("qbit_delete_torrent", move |db| db.delete(&cache_hash))
                .await
        {
            cache_failed = true;
            tracing::warn!(
                component = "cache",
                operation = "delete_torrent",
                torrent = %hash,
                result = "error",
                error = %e,
                "cache delete failed after qBit delete"
            );
        } else {
            emit(&s, Event::TorrentRemoved { hash: hash.clone() }).await;
            emit(&s, Event::TrackerHealthUpdated).await;
        }
    }
    if cache_failed {
        StatusCode::INTERNAL_SERVER_ERROR
    } else if backend_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn torrents_trackers(
    State(s): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let hash = match required_resolved_hash_async(&s.db, q.get("hash").map(String::as_str)).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.list_trackers(&hash).await {
        Ok(trackers) => {
            let out: Vec<_> = trackers
                .iter()
                .map(|t| {
                    let status = if t.is_enabled && t.success_counter > 0 {
                        2
                    } else if t.failed_counter > 0 {
                        4
                    } else {
                        0
                    };
                    json!({
                        "url":      t.url,
                        "status":   status,
                        "tier":     t.group,
                        "num_peers":   t.scrape_incomplete + t.scrape_complete,
                        "num_seeds":   t.scrape_complete,
                        "num_leechs":  t.scrape_incomplete,
                        "num_downloaded": t.scrape_downloaded,
                        "msg":      t.message,
                    })
                })
                .collect();
            Json(json!(out)).into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_trackers",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit tracker listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn torrents_export(
    State(s): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let hash = match required_resolved_hash_async(&s.db, q.hash.as_deref()).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.torrent_blob(&hash).await {
        Ok(raw) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/x-bittorrent"),
            );
            (StatusCode::OK, headers, raw).into_response()
        }
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "torrent_export",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit torrent export failed"
            );
            backend_error_status(&e).into_response()
        }
    }
}

async fn torrents_files(
    State(s): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let hash = match required_resolved_hash_async(&s.db, q.get("hash").map(String::as_str)).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.list_files(&hash).await {
        Ok(files) => {
            let out: Vec<_> = files
                .iter()
                .map(|f| {
                    let progress = if f.size_chunks > 0 {
                        f.completed_chunks as f64 / f.size_chunks as f64
                    } else {
                        1.0
                    };
                    // qBit priority: 0=do not download, 1=normal, 6=high, 7=maximal
                    let priority = match f.priority {
                        0 => 0,
                        2 => 6,
                        _ => 1,
                    };
                    json!({
                        "index":        f.index,
                        "name":         f.path,
                        "size":         f.size_bytes,
                        "progress":     progress,
                        "priority":     priority,
                        "is_seed":      f.completed_chunks >= f.size_chunks,
                        "piece_range":  [0, 0],
                        "availability": if f.is_created { 1.0f64 } else { 0.0f64 },
                    })
                })
                .collect();
            Json(json!(out)).into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_files",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit file listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn torrents_webseeds(
    State(s): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let hash = match required_resolved_hash_async(&s.db, q.hash.as_deref()).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.list_webseeds(&hash).await {
        Ok(webseeds) => Json(json!(webseeds)).into_response(),
        Err(e) if is_unsupported_error(&e) => {
            // qBittorrent represents an absent optional capability as an
            // empty collection. Preserve that wire contract for adapters
            // that explicitly do not expose the feature; transport and
            // backend failures still return a non-success status below.
            Json(json!([])).into_response()
        }
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "list_webseeds",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit webseed listing failed"
            );
            backend_error_status(&e).into_response()
        }
    }
}

async fn torrents_piece_states(
    State(s): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let hash = match required_resolved_hash_async(&s.db, q.hash.as_deref()).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.piece_states(&hash).await {
        Ok(states) => {
            let states: Vec<i32> = states
                .into_iter()
                .map(|state| match state {
                    BackendPieceState::Missing => 0,
                    BackendPieceState::Partial => 1,
                    BackendPieceState::Complete => 2,
                })
                .collect();
            Json(json!(states)).into_response()
        }
        Err(e) if is_unsupported_error(&e) => Json(json!([])).into_response(),
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "piece_states",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit piece state query failed"
            );
            backend_error_status(&e).into_response()
        }
    }
}

async fn torrents_piece_hashes(
    State(s): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let hash = match required_resolved_hash_async(&s.db, q.hash.as_deref()).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.piece_hashes(&hash).await {
        Ok(hashes) => Json(json!(hashes)).into_response(),
        Err(e) if is_unsupported_error(&e) => Json(json!([])).into_response(),
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "piece_hashes",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit piece hash query failed"
            );
            backend_error_status(&e).into_response()
        }
    }
}

async fn categories(State(s): State<AppState>) -> impl IntoResponse {
    match s
        .db
        .run_blocking("qbit_list_categories", |db| db.list_categories())
        .await
    {
        Ok(cats) => {
            let map: serde_json::Map<String, serde_json::Value> = cats
                .iter()
                .map(|c| {
                    (
                        c.name.clone(),
                        json!({ "name": c.name, "savePath": c.save_path }),
                    )
                })
                .collect();
            Json(serde_json::Value::Object(map)).into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_categories",
                result = "error",
                error = %e,
                "qBit category listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn tags(State(s): State<AppState>) -> impl IntoResponse {
    match s
        .db
        .run_blocking("qbit_list_tags", |db| db.list_tags())
        .await
    {
        Ok(tags) => Json(tags).into_response(),
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_tags",
                result = "error",
                error = %e,
                "qBit tag listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
struct SetCategoryForm {
    hashes: Option<String>,
    category: Option<String>,
}

async fn torrents_set_category(
    State(s): State<AppState>,
    Form(f): Form<SetCategoryForm>,
) -> StatusCode {
    let Some(category) = f.category.as_deref() else {
        return StatusCode::BAD_REQUEST;
    };
    if !s.backend.capabilities().supports_categories {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for category update"
            );
            return hash_resolution_status(&e);
        }
    };
    let mut backend_failed = false;
    let mut cache_failed = false;
    for hash in hashes {
        if let Err(e) = s.backend.set_category(&hash, category).await {
            backend_failed = true;
            tracing::warn!(
                component = "backend",
                operation = "set_category",
                torrent = %hash,
                category = %category,
                result = "error",
                error = %e,
                "backend category update failed"
            );
            continue;
        }
        let cache_hash = hash.clone();
        let cache_category = category.to_owned();
        if let Err(e) =
            s.db.run_blocking("qbit_set_torrent_category", move |db| {
                db.set_torrent_category(&cache_hash, &cache_category)
            })
            .await
        {
            cache_failed = true;
            tracing::warn!(
                component = "cache",
                operation = "set_category",
                torrent = %hash,
                category = %category,
                result = "error",
                error = %e,
                "cache category update failed after backend update"
            );
        } else {
            emit_torrent_updated(&s, &hash).await;
            emit(&s, Event::CategoriesUpdated).await;
        }
    }
    if cache_failed {
        StatusCode::INTERNAL_SERVER_ERROR
    } else if backend_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

#[derive(Deserialize)]
struct TagsForm {
    hashes: Option<String>,
    tags: Option<String>,
}

async fn torrents_add_tags(State(s): State<AppState>, Form(f): Form<TagsForm>) -> StatusCode {
    let tag_list = match strict_tag_values(f.tags.as_deref(), false) {
        Ok(tags) => tags,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    if !s.backend.capabilities().supports_tags {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for tag add"
            );
            return hash_resolution_status(&e);
        }
    };
    let mut backend_failed = false;
    let mut cache_failed = false;
    for hash in hashes {
        if let Err(e) = s.backend.add_tags(&hash, &tag_list).await {
            backend_failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "add_tags",
                torrent = %hash,
                result = "error",
                error = %e,
                "backend tag add failed"
            );
            continue;
        }
        let cache_hash = hash.clone();
        let cache_tags = tag_list
            .iter()
            .map(|tag| (*tag).to_owned())
            .collect::<Vec<_>>();
        if let Err(e) =
            s.db.run_blocking("qbit_add_torrent_tags", move |db| {
                let tag_refs = cache_tags.iter().map(String::as_str).collect::<Vec<_>>();
                db.add_torrent_tags(&cache_hash, &tag_refs)
            })
            .await
        {
            cache_failed = true;
            tracing::warn!(
                component = "cache",
                operation = "add_tags",
                torrent = %hash,
                result = "error",
                error = %e,
                "cache tag add failed after backend update"
            );
        } else {
            emit_torrent_updated(&s, &hash).await;
            emit(&s, Event::TagsUpdated).await;
        }
    }
    if cache_failed {
        StatusCode::INTERNAL_SERVER_ERROR
    } else if backend_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn torrents_remove_tags(State(s): State<AppState>, Form(f): Form<TagsForm>) -> StatusCode {
    let tag_list = match strict_tag_values(f.tags.as_deref(), false) {
        Ok(tags) => tags,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    if !s.backend.capabilities().supports_tags {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for tag removal"
            );
            return hash_resolution_status(&e);
        }
    };
    let mut backend_failed = false;
    let mut cache_failed = false;
    for hash in hashes {
        if let Err(e) = s.backend.remove_tags(&hash, &tag_list).await {
            backend_failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "remove_tags",
                torrent = %hash,
                result = "error",
                error = %e,
                "backend tag removal failed"
            );
            continue;
        }
        let cache_hash = hash.clone();
        let cache_tags = tag_list
            .iter()
            .map(|tag| (*tag).to_owned())
            .collect::<Vec<_>>();
        if let Err(e) =
            s.db.run_blocking("qbit_remove_torrent_tags", move |db| {
                let tag_refs = cache_tags.iter().map(String::as_str).collect::<Vec<_>>();
                db.remove_torrent_tags(&cache_hash, &tag_refs)
            })
            .await
        {
            cache_failed = true;
            tracing::warn!(
                component = "cache",
                operation = "remove_tags",
                torrent = %hash,
                result = "error",
                error = %e,
                "cache tag removal failed after backend update"
            );
        } else {
            emit_torrent_updated(&s, &hash).await;
            emit(&s, Event::TagsUpdated).await;
        }
    }
    if cache_failed {
        StatusCode::INTERNAL_SERVER_ERROR
    } else if backend_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn torrents_set_tags(State(s): State<AppState>, Form(f): Form<TagsForm>) -> StatusCode {
    let tag_list = match strict_tag_values(f.tags.as_deref(), true) {
        Ok(tags) => tags,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    if !s.backend.capabilities().supports_tags {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for tag replacement"
            );
            return hash_resolution_status(&e);
        }
    };
    let mut backend_failed = false;
    let mut cache_failed = false;
    for hash in hashes {
        if let Err(e) = s.backend.set_tags(&hash, &tag_list).await {
            backend_failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "set_tags",
                torrent = %hash,
                result = "error",
                error = %e,
                "backend tag replace failed"
            );
            continue;
        }
        let cache_hash = hash.clone();
        let cache_tags = tag_list
            .iter()
            .map(|tag| (*tag).to_owned())
            .collect::<Vec<_>>();
        if let Err(e) =
            s.db.run_blocking("qbit_set_torrent_tags", move |db| {
                let tag_refs = cache_tags.iter().map(String::as_str).collect::<Vec<_>>();
                db.set_torrent_tags(&cache_hash, &tag_refs)
            })
            .await
        {
            cache_failed = true;
            tracing::warn!(
                component = "cache",
                operation = "set_tags",
                torrent = %hash,
                result = "error",
                error = %e,
                "cache tag replace failed after backend update"
            );
        } else {
            emit_torrent_updated(&s, &hash).await;
            emit(&s, Event::TagsUpdated).await;
        }
    }
    if cache_failed {
        StatusCode::INTERNAL_SERVER_ERROR
    } else if backend_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

// --- qBit category/tag management ---

#[derive(Deserialize)]
struct CreateCategoryForm {
    category: Option<String>,
    #[serde(rename = "savePath")]
    save_path: Option<String>,
}

async fn create_category(
    State(s): State<AppState>,
    Form(f): Form<CreateCategoryForm>,
) -> StatusCode {
    if !s.backend.capabilities().supports_categories {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let name = match f.category.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => n,
        _ => return StatusCode::BAD_REQUEST,
    };
    let save_path = f.save_path.as_deref().unwrap_or("");
    let category_name = name.to_owned();
    let category_path = save_path.to_owned();
    match s
        .db
        .run_blocking("qbit_create_category", move |db| {
            db.upsert_category(&category_name, &category_path)
        })
        .await
    {
        Ok(_) => {
            emit(&s, Event::CategoriesUpdated).await;
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "create_category",
                category = %name,
                result = "error",
                error = %e,
                "qBit category create failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn edit_category(State(s): State<AppState>, Form(f): Form<CreateCategoryForm>) -> StatusCode {
    if !s.backend.capabilities().supports_categories {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let name = match f.category.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => n,
        _ => return StatusCode::BAD_REQUEST,
    };
    let save_path = f.save_path.as_deref().unwrap_or("");
    let category_name = name.to_owned();
    let category_path = save_path.to_owned();
    match s
        .db
        .run_blocking("qbit_edit_category", move |db| {
            db.upsert_category(&category_name, &category_path)
        })
        .await
    {
        Ok(_) => {
            emit(&s, Event::CategoriesUpdated).await;
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "edit_category",
                category = %name,
                result = "error",
                error = %e,
                "qBit category edit failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Deserialize)]
struct RemoveCategoriesForm {
    categories: Option<String>,
}

async fn remove_categories(
    State(s): State<AppState>,
    Form(f): Form<RemoveCategoriesForm>,
) -> StatusCode {
    if !s.backend.capabilities().supports_categories {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let categories = match required_qbit_list(f.categories.as_deref()) {
        Ok(categories) => categories,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    let mut failed = false;
    for name in categories {
        let delete_name = name.clone();
        if let Err(e) =
            s.db.run_blocking("qbit_delete_category", move |db| {
                db.delete_category(&delete_name)
            })
            .await
        {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "remove_category",
                category = %name,
                result = "error",
                error = %e,
                "qBit category removal failed"
            );
        } else {
            emit(&s, Event::CategoriesUpdated).await;
            emit(&s, Event::TrackerHealthUpdated).await;
        }
    }
    if failed {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::OK
    }
}

#[derive(Deserialize)]
struct CreateTagsForm {
    tags: Option<String>,
}

async fn create_tags(State(s): State<AppState>, Form(f): Form<CreateTagsForm>) -> StatusCode {
    if !s.backend.capabilities().supports_tags {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let tags = match strict_tag_values(f.tags.as_deref(), false) {
        Ok(tags) => tags,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    let mut failed = false;
    for tag in tags {
        let tag_name = tag.to_owned();
        if let Err(e) =
            s.db.run_blocking("qbit_ensure_tag", move |db| db.ensure_tag(&tag_name))
                .await
        {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "create_tag",
                tag = %tag,
                result = "error",
                error = %e,
                "qBit tag create failed"
            );
        } else {
            emit(&s, Event::TagsUpdated).await;
        }
    }
    if failed {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::OK
    }
}

async fn delete_tags(State(s): State<AppState>, Form(f): Form<CreateTagsForm>) -> StatusCode {
    if !s.backend.capabilities().supports_tags {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let tags = match strict_tag_values(f.tags.as_deref(), false) {
        Ok(tags) => tags,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    let mut failed = false;
    for tag in tags {
        let tag_name = tag.to_owned();
        if let Err(e) =
            s.db.run_blocking("qbit_delete_tag", move |db| db.delete_tag(&tag_name))
                .await
        {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "delete_tag",
                tag = %tag,
                result = "error",
                error = %e,
                "qBit tag delete failed"
            );
        } else {
            emit(&s, Event::TagsUpdated).await;
            emit(&s, Event::TrackerHealthUpdated).await;
        }
    }
    if failed {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::OK
    }
}

#[derive(Deserialize)]
struct FilePrioForm {
    hash: Option<String>,
    id: Option<String>, // pipe-separated file indices
    priority: Option<String>,
}

async fn torrents_file_prio(State(s): State<AppState>, Form(f): Form<FilePrioForm>) -> StatusCode {
    let hash = match required_resolved_hash_async(&s.db, f.hash.as_deref()).await {
        Ok(hash) => hash,
        Err(status) => return status,
    };
    let priority: i64 = match f.priority.as_deref() {
        Some("0") => 0,
        Some("6") | Some("7") => 2,
        Some("1") => 1,
        _ => return StatusCode::BAD_REQUEST,
    };
    if !s.backend.capabilities().supports_file_priority {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let ids = match f.id {
        Some(ids) if !ids.trim().is_empty() => ids,
        None | Some(_) => return StatusCode::BAD_REQUEST,
    };
    let ids = match ids
        .split('|')
        .map(|id| id.trim().parse::<usize>().map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(ids) if !ids.is_empty() => ids,
        _ => return StatusCode::BAD_REQUEST,
    };
    let mut failed = false;
    for idx in ids {
        if let Err(e) = s.backend.set_file_priority(&hash, idx, priority).await {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "set_file_priority",
                torrent = %hash,
                file_index = idx,
                priority,
                result = "error",
                error = %e,
                "qBit file priority update failed"
            );
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

#[derive(Deserialize)]
struct AddTrackersForm {
    hashes: Option<String>,
    urls: Option<String>,
}

async fn torrents_add_trackers(
    State(s): State<AppState>,
    Form(f): Form<AddTrackersForm>,
) -> StatusCode {
    let urls = match required_qbit_lines(f.urls.as_deref()) {
        Ok(urls) => urls,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    if !s.backend.capabilities().supports_tracker_edit {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for tracker add"
            );
            return hash_resolution_status(&e);
        }
    };
    let mut failed = false;

    for hash in hashes {
        for url in &urls {
            if let Err(e) = s.backend.add_tracker(&hash, url).await {
                failed = true;
                tracing::warn!(
                    component = "qbcompat",
                    operation = "add_tracker",
                    torrent = %hash,
                    tracker = %redact_log_url(url),
                result = "error",
                    error = %e,
                    "qb add tracker failed"
                );
            }
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

#[derive(Deserialize)]
struct RemoveTrackersForm {
    hash: Option<String>,
    urls: Option<String>,
}

async fn torrents_remove_trackers(
    State(s): State<AppState>,
    Form(f): Form<RemoveTrackersForm>,
) -> StatusCode {
    let hash = match required_resolved_hash_async(&s.db, f.hash.as_deref()).await {
        Ok(hash) => hash,
        Err(status) => return status,
    };
    let urls = match required_qbit_list(f.urls.as_deref()) {
        Ok(urls) => urls,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    if !s.backend.capabilities().supports_tracker_edit {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let mut failed = false;

    for url in urls {
        if let Err(e) = s.backend.remove_tracker(&hash, &url).await {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "remove_tracker",
                torrent = %hash,
                tracker = %redact_log_url(&url),
                result = "error",
                error = %e,
                "qb remove tracker failed"
            );
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

#[derive(Deserialize)]
struct EditTrackerForm {
    hash: Option<String>,
    #[serde(rename = "origUrl")]
    orig_url: Option<String>,
    #[serde(rename = "newUrl")]
    new_url: Option<String>,
}

async fn torrents_edit_tracker(
    State(s): State<AppState>,
    Form(f): Form<EditTrackerForm>,
) -> StatusCode {
    let hash = match required_resolved_hash_async(&s.db, f.hash.as_deref()).await {
        Ok(hash) => hash,
        Err(status) => return status,
    };
    let Some(orig_url) = f.orig_url.filter(|url| !url.trim().is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_url) = f.new_url.filter(|url| !url.trim().is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    if !s.backend.capabilities().supports_tracker_edit {
        return StatusCode::NOT_IMPLEMENTED;
    }
    if let Err(e) = s.backend.edit_tracker(&hash, &orig_url, &new_url).await {
        tracing::warn!(
            component = "qbcompat",
            operation = "edit_tracker",
            torrent = %hash,
            tracker = %redact_log_url(&orig_url),
            new_tracker = %redact_log_url(&new_url),
                result = "error",
            error = %e,
            "qBit tracker edit failed"
        );
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct RenameForm {
    hash: Option<String>,
    name: Option<String>,
}

async fn torrents_rename(State(s): State<AppState>, Form(f): Form<RenameForm>) -> StatusCode {
    let hash = match required_resolved_hash_async(&s.db, f.hash.as_deref()).await {
        Ok(hash) => hash,
        Err(status) => return status,
    };
    let Some(name) = f.name else {
        return StatusCode::BAD_REQUEST;
    };
    if !s.backend.capabilities().supports_torrent_rename {
        return StatusCode::NOT_IMPLEMENTED;
    }
    if let Err(e) = s.backend.rename_torrent(&hash, &name).await {
        tracing::warn!(
            component = "qbcompat",
            operation = "rename_torrent",
            torrent = %hash,
                result = "error",
            error = %e,
            "qBit torrent rename failed"
        );
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct RenameFileForm {
    hash: Option<String>,
    id: Option<usize>,
    name: Option<String>,
}

async fn torrents_rename_file(
    State(s): State<AppState>,
    Form(f): Form<RenameFileForm>,
) -> StatusCode {
    let hash = match required_resolved_hash_async(&s.db, f.hash.as_deref()).await {
        Ok(hash) => hash,
        Err(status) => return status,
    };
    let Some(id) = f.id else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(name) = f.name else {
        return StatusCode::BAD_REQUEST;
    };
    if !s.backend.capabilities().supports_file_rename {
        return StatusCode::NOT_IMPLEMENTED;
    }
    if let Err(e) = s.backend.rename_file(&hash, id, &name).await {
        tracing::warn!(
            component = "qbcompat",
            operation = "rename_file",
            torrent = %hash,
            file_index = id,
                result = "error",
            error = %e,
            "qBit file rename failed"
        );
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct ShareLimitsForm {
    hashes: Option<String>,
    #[serde(rename = "ratioLimit")]
    ratio_limit: Option<f64>,
    #[serde(rename = "seedingTimeLimit")]
    seeding_time_limit: Option<i64>,
}

#[derive(Deserialize)]
struct SpeedLimitForm {
    hashes: Option<String>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
struct HashesQuery {
    hashes: Option<String>,
}

async fn torrents_download_limit(
    State(s): State<AppState>,
    Query(q): Query<HashesQuery>,
) -> impl IntoResponse {
    torrents_limit_map(s, q.hashes.as_deref(), true).await
}

async fn torrents_upload_limit(
    State(s): State<AppState>,
    Query(q): Query<HashesQuery>,
) -> impl IntoResponse {
    torrents_limit_map(s, q.hashes.as_deref(), false).await
}

async fn torrents_limit_map(
    s: AppState,
    hashes: Option<&str>,
    download: bool,
) -> impl IntoResponse {
    if !s.backend.capabilities().supports_per_torrent_limits {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    let hashes = match required_resolved_hashes_async(&s.db, hashes).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = if download {
                    "resolve_download_limit_hashes"
                } else {
                    "resolve_upload_limit_hashes"
                },
                result = "error",
                error = %e,
                "failed to resolve hashes for per-torrent limit read"
            );
            return hash_resolution_status(&e).into_response();
        }
    };
    let result = if download {
        s.backend.download_limits(&hashes).await
    } else {
        s.backend.upload_limits(&hashes).await
    };
    match result {
        Ok(limits) => Json(limits).into_response(),
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = if download {
                    "download_limits"
                } else {
                    "upload_limits"
                },
                result = "error",
                error = %e,
                "qBit per-torrent limit read failed"
            );
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn torrents_set_download_limit(
    State(s): State<AppState>,
    Form(f): Form<SpeedLimitForm>,
) -> StatusCode {
    torrents_set_speed_limit(s, f, true).await
}

async fn torrents_set_upload_limit(
    State(s): State<AppState>,
    Form(f): Form<SpeedLimitForm>,
) -> StatusCode {
    torrents_set_speed_limit(s, f, false).await
}

async fn torrents_set_speed_limit(s: AppState, f: SpeedLimitForm, download: bool) -> StatusCode {
    if !s.backend.capabilities().supports_per_torrent_limits {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let Some(raw_limit) = f.limit.filter(|value| *value >= 0) else {
        return StatusCode::BAD_REQUEST;
    };
    let limit = (raw_limit > 0).then_some(raw_limit);
    let operation = if download {
        "set_download_limit"
    } else {
        "set_upload_limit"
    };
    let mut failed = false;
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for per-torrent limit update"
            );
            return hash_resolution_status(&e);
        }
    };
    for hash in hashes {
        let result = if download {
            s.backend.set_download_limit(&hash, limit).await
        } else {
            s.backend.set_upload_limit(&hash, limit).await
        };
        if let Err(e) = result {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation,
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit speed limit update failed"
            );
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn transfer_set_download_limit(
    State(s): State<AppState>,
    Form(f): Form<SpeedLimitForm>,
) -> StatusCode {
    let Some(limit) = f.limit.filter(|value| *value >= 0) else {
        return StatusCode::BAD_REQUEST;
    };
    transfer_set_speed_limit(s, limit, true).await
}

async fn transfer_set_upload_limit(
    State(s): State<AppState>,
    Form(f): Form<SpeedLimitForm>,
) -> StatusCode {
    let Some(limit) = f.limit.filter(|value| *value >= 0) else {
        return StatusCode::BAD_REQUEST;
    };
    transfer_set_speed_limit(s, limit, false).await
}

async fn transfer_ban_peers(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(raw_peers) = f.get("peers") else {
        return StatusCode::BAD_REQUEST;
    };
    let peers = match parse_peer_addrs(raw_peers) {
        Ok(peers) => peers,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    if peers.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    if !s.backend.capabilities().supports_peer_ban {
        return StatusCode::NOT_IMPLEMENTED;
    }
    if let Err(e) = s.backend.ban_peers(&peers).await {
        tracing::warn!(
            component = "qbcompat",
            operation = "ban_peers",
            result = "unsupported",
            error = %e,
            "qBit peer ban not supported by backend"
        );
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::OK
}

async fn transfer_set_speed_limit(s: AppState, limit: i64, download: bool) -> StatusCode {
    if !s.backend.capabilities().supports_global_limits {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let limit = limit.max(0);
    let operation = if download {
        "set_global_download_limit"
    } else {
        "set_global_upload_limit"
    };
    let result = if download {
        s.backend.set_global_download_limit(limit).await
    } else {
        s.backend.set_global_upload_limit(limit).await
    };
    match result {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation,
                result = "error",
                error = %e,
                "qBit global speed limit update failed"
            );
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

async fn transfer_toggle_speed_limits_mode(State(s): State<AppState>) -> StatusCode {
    if !s.backend.capabilities().supports_global_limits {
        return StatusCode::NOT_IMPLEMENTED;
    }
    match s.backend.toggle_global_speed_limits_mode().await {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "toggle_global_speed_limits_mode",
                result = "error",
                error = %e,
                "qBit global speed-limit mode toggle failed"
            );
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

async fn transfer_speed_limits_mode(State(s): State<AppState>) -> impl IntoResponse {
    match s.backend.global_limits().await {
        Ok(limits) if limits.speed_limits_mode => "1".to_owned().into_response(),
        Ok(_) => "0".to_owned().into_response(),
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "speed_limits_mode",
                result = "error",
                error = %e,
                "qBit speed-limit mode read failed"
            );
            backend_error_status(&e).into_response()
        }
    }
}

async fn transfer_download_limit(State(s): State<AppState>) -> impl IntoResponse {
    match s.backend.global_limits().await {
        Ok(limits) => limits.download_limit.max(0).to_string().into_response(),
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "download_limit",
                result = "error",
                error = %e,
                "qBit download-limit read failed"
            );
            backend_error_status(&e).into_response()
        }
    }
}

async fn transfer_upload_limit(State(s): State<AppState>) -> impl IntoResponse {
    match s.backend.global_limits().await {
        Ok(limits) => limits.upload_limit.max(0).to_string().into_response(),
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "upload_limit",
                result = "error",
                error = %e,
                "qBit upload-limit read failed"
            );
            backend_error_status(&e).into_response()
        }
    }
}

async fn torrents_set_share_limits(
    State(s): State<AppState>,
    Form(f): Form<ShareLimitsForm>,
) -> StatusCode {
    if !s.backend.capabilities().supports_share_limits {
        return StatusCode::NOT_IMPLEMENTED;
    }
    if f.ratio_limit
        .is_some_and(|ratio| !ratio.is_finite() || (ratio < 0.0 && ratio != -1.0 && ratio != -2.0))
    {
        return StatusCode::BAD_REQUEST;
    }
    if f.ratio_limit.is_none() && f.seeding_time_limit.is_none() {
        return StatusCode::BAD_REQUEST;
    }
    if f.seeding_time_limit
        .is_some_and(|limit| limit < 0 && limit != -1 && limit != -2)
    {
        return StatusCode::BAD_REQUEST;
    }
    let ratio_limit_milli = f.ratio_limit.map(qbit_ratio_limit_milli).unwrap_or(-2);
    let seeding_time_limit = f.seeding_time_limit.unwrap_or(-2);
    let mut failed = false;
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for share-limit update"
            );
            return hash_resolution_status(&e);
        }
    };
    for hash in hashes {
        if let Err(e) = s
            .backend
            .set_share_limits(&hash, ratio_limit_milli, seeding_time_limit)
            .await
        {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "set_share_limits",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit share limit update failed"
            );
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

fn qbit_ratio_limit_milli(ratio: f64) -> i64 {
    // qBittorrent uses -2 as the "use global/default" sentinel. It is not a
    // real negative ratio and must survive the fixed-point conversion
    // unchanged. -1 means explicitly disable the ratio limit.
    if ratio == -2.0 || ratio == -1.0 {
        ratio as i64
    } else {
        ratio_milli(Some(ratio))
    }
}

#[derive(Deserialize)]
struct LocationForm {
    hashes: Option<String>,
    location: Option<String>,
}

async fn torrents_set_location(
    State(s): State<AppState>,
    Form(f): Form<LocationForm>,
) -> StatusCode {
    let Some(location) = f.location.as_deref().map(str::trim) else {
        return StatusCode::BAD_REQUEST;
    };
    if location.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    if !s.backend.capabilities().supports_location_update {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let mut backend_failed = false;
    let mut cache_failed = false;
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for location update"
            );
            return hash_resolution_status(&e);
        }
    };
    for hash in hashes {
        if let Err(e) = s.backend.set_location(&hash, location).await {
            backend_failed = true;
            tracing::warn!(
                component = "backend",
                operation = "set_location",
                torrent = %hash,
                result = "error",
                error = %e,
                "backend location update failed"
            );
            continue;
        }
        let lookup_hash = hash.clone();
        match s
            .db
            .run_blocking("qbit_set_location_exists", move |db| {
                db.exists(&lookup_hash)
            })
            .await
        {
            Ok(true) => {
                let cache_hash = hash.clone();
                let cache_location = location.to_owned();
                if let Err(e) =
                    s.db.run_blocking("qbit_set_location_cache", move |db| {
                        db.set_torrent_location(&cache_hash, &cache_location)
                    })
                    .await
                {
                    cache_failed = true;
                    tracing::warn!(
                            component = "cache",
                            operation = "set_location",
                            torrent = %hash,
                    result = "error",
                            error = %e,
                            "cache location update failed"
                        );
                } else {
                    emit_torrent_updated(&s, &hash).await;
                }
            }
            Ok(false) => {}
            Err(e) => {
                cache_failed = true;
                tracing::warn!(
                    component = "cache",
                    operation = "exists",
                    torrent = %hash,
                    result = "error",
                    error = %e,
                    "cache torrent existence check failed"
                );
            }
        }
    }
    if cache_failed {
        StatusCode::INTERNAL_SERVER_ERROR
    } else if backend_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

#[derive(Deserialize)]
struct ToggleSequentialForm {
    hashes: Option<String>,
}

#[derive(Deserialize)]
struct TorrentModeForm {
    hashes: Option<String>,
    value: Option<bool>,
    enable: Option<bool>,
}

async fn torrents_set_force_start(
    State(s): State<AppState>,
    Form(f): Form<TorrentModeForm>,
) -> StatusCode {
    torrents_set_mode_flag(s, f, "set_force_start").await
}

async fn torrents_set_super_seeding(
    State(s): State<AppState>,
    Form(f): Form<TorrentModeForm>,
) -> StatusCode {
    torrents_set_mode_flag(s, f, "set_super_seeding").await
}

async fn torrents_set_auto_tmm(
    State(s): State<AppState>,
    Form(f): Form<TorrentModeForm>,
) -> StatusCode {
    torrents_set_mode_flag(s, f, "set_auto_tmm").await
}

async fn torrents_set_auto_management(
    State(s): State<AppState>,
    Form(f): Form<TorrentModeForm>,
) -> StatusCode {
    torrents_set_mode_flag(s, f, "set_auto_management").await
}

async fn torrents_set_mode_flag(s: AppState, f: TorrentModeForm, operation: &str) -> StatusCode {
    if !s.backend.capabilities().supports_mode_flags {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let Some(enabled) = f.value.or(f.enable) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut failed = false;
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for mode update"
            );
            return hash_resolution_status(&e);
        }
    };
    for hash in hashes {
        let result = match operation {
            "set_force_start" => s.backend.set_force_start(&hash, enabled).await,
            "set_super_seeding" => s.backend.set_super_seeding(&hash, enabled).await,
            "set_auto_tmm" => s.backend.set_auto_tmm(&hash, enabled).await,
            "set_auto_management" => s.backend.set_auto_management(&hash, enabled).await,
            _ => Err(anyhow::anyhow!(
                "unsupported torrent mode operation {operation}"
            )),
        };
        if let Err(e) = result {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation,
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit torrent mode update failed"
            );
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn torrents_toggle_sequential_download(
    State(s): State<AppState>,
    Form(f): Form<ToggleSequentialForm>,
) -> StatusCode {
    if !s.backend.capabilities().supports_per_torrent_limits {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let mut failed = false;
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for sequential toggle"
            );
            return hash_resolution_status(&e);
        }
    };
    for hash in hashes {
        if let Err(e) = s.backend.toggle_sequential_download(&hash).await {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "toggle_sequential_download",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit sequential download toggle failed"
            );
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn torrents_toggle_first_last_piece_prio(
    State(s): State<AppState>,
    Form(f): Form<ToggleSequentialForm>,
) -> StatusCode {
    if !s.backend.capabilities().supports_per_torrent_limits {
        return StatusCode::NOT_IMPLEMENTED;
    }
    let mut failed = false;
    let hashes = match required_resolved_hashes_async(&s.db, f.hashes.as_deref()).await {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "resolve_hashes",
                result = "error",
                error = %e,
                "failed to resolve hashes for first-last toggle"
            );
            return hash_resolution_status(&e);
        }
    };
    for hash in hashes {
        if let Err(e) = s.backend.toggle_first_last_piece_priority(&hash).await {
            failed = true;
            tracing::warn!(
                component = "qbcompat",
                operation = "toggle_first_last_piece_prio",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit first/last piece priority toggle failed"
            );
        }
    }
    if failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

// --- Sync ---

#[derive(Deserialize)]
struct MaindataQuery {
    rid: Option<i64>,
}

async fn sync_maindata(
    State(s): State<AppState>,
    Query(q): Query<MaindataQuery>,
) -> impl IntoResponse {
    // rid absent or 0 → full update; rid>0 → incremental since that rid
    let rid = q.rid.unwrap_or(0);
    if rid < 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let full = rid == 0;
    let (categories, tags) = match sync_metadata(&s).await {
        Ok(metadata) => metadata,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "sync_maindata_metadata",
                result = "error",
                error = %e,
                "qBit maindata metadata load failed"
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let backend_status =
        match tokio::time::timeout(Duration::from_secs(3), s.backend.health()).await {
            Ok(status) => status,
            Err(_) => BackendStatus::Unreachable,
        };
    let rates = match crate::stats::current_rates_result(s.backend.clone()).await {
        Ok(rates) => rates,
        Err(error) if backend_status == BackendStatus::Connected => {
            tracing::warn!(
                component = "qbcompat",
                operation = "sync_maindata_transfer_stats",
                result = "error",
                error = %crate::sync::error_chain(&error),
                "connected backend transfer stats are unavailable"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "transfer rates unavailable",
                    "connection_status": "unreachable",
                })),
            )
                .into_response();
        }
        Err(_) => TransferRates::default(),
    };
    let server_state = qb_server_state(rates, backend_status == BackendStatus::Connected);

    if full {
        let params = ListParams {
            limit: Some((MAX_QBIT_SYNC_ENTRIES + 1) as i64),
            ..Default::default()
        };
        // Read the cursor before the list. If a concurrent mutation lands
        // while the page is materialized, the following incremental request
        // will replay it; reading the cursor after the list would falsely
        // claim that mutation was already included and lose it.
        let revision = match s
            .db
            .run_blocking("qbit_sync_maindata_revision", |db| db.current_revision())
            .await
        {
            Ok(revision) => revision,
            Err(e) => {
                tracing::error!(
                    component = "qbcompat",
                    operation = "sync_maindata_revision",
                    result = "error",
                    error = %e,
                    "qBit maindata revision load failed"
                );
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
        match s
            .db
            .run_blocking("qbit_sync_maindata_full", move |db| db.list_page(&params))
            .await
        {
            Ok(rows) if rows.len() > MAX_QBIT_SYNC_ENTRIES => {
                tracing::warn!(
                    component = "qbcompat",
                    operation = "sync_maindata",
                    result = "rejected",
                    total = rows.len(),
                    maximum = MAX_QBIT_SYNC_ENTRIES,
                    "qBit full sync exceeds the bounded compatibility response; use paged native endpoints"
                );
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "qBit full sync contains more than {} torrents; use the paged native API",
                        MAX_QBIT_SYNC_ENTRIES
                    ),
                )
                    .into_response()
            }
            Ok(rows) => {
                let torrents = match torrents_map(&rows) {
                    Ok(torrents) => torrents,
                    Err(e) => {
                        tracing::error!(
                                    component = "qbcompat",
                                    operation = "sync_maindata_serialize",
                        result = "error",
                                    error = %e,
                                    "qBit maindata serialization failed"
                                );
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                };
                Json(json!({
                    "rid": revision,
                    "full_update": true,
                    "torrents": torrents,
                    "torrents_removed": [],
                    "categories": categories,
                    "tags": tags,
                    "server_state": server_state,
                }))
                .into_response()
            }
            Err(e) => {
                tracing::error!(
                    component = "qbcompat",
                    operation = "sync_maindata",
                result = "error",
                    error = %e,
                    "qBit maindata query failed"
                );
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    } else {
        match s
            .db
            .run_blocking("qbit_sync_maindata_delta", move |db| {
                db.list_since_bounded(rid, MAX_QBIT_SYNC_ENTRIES)
            })
            .await
        {
            Ok(Some(delta)) => {
                let torrents = match torrents_map(&delta.changed) {
                    Ok(torrents) => torrents,
                    Err(e) => {
                        tracing::error!(
                                    component = "qbcompat",
                                    operation = "sync_maindata_delta_serialize",
                        result = "error",
                                    error = %e,
                                    "qBit maindata delta serialization failed"
                                );
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                };
                Json(json!({
                    "rid": delta.revision,
                    "full_update": false,
                    "torrents": torrents,
                    "torrents_removed": delta.removed,
                    "categories": categories,
                    "tags": tags,
                    "server_state": server_state,
                }))
                .into_response()
            }
            Ok(None) => {
                tracing::warn!(
                    component = "qbcompat",
                    operation = "sync_maindata",
                    result = "rejected",
                    rid,
                    maximum = MAX_QBIT_SYNC_ENTRIES,
                    "qBit incremental sync exceeds the bounded compatibility response; request a fresh paged/native sync"
                );
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "qBit incremental sync exceeds the {}-torrent limit; request a fresh sync or use the paged native API",
                        MAX_QBIT_SYNC_ENTRIES
                    ),
                )
                    .into_response()
            }
            Err(e) => {
                tracing::error!(
                    component = "qbcompat",
                    operation = "sync_maindata_delta",
                result = "error",
                    error = %e,
                    "qBit maindata delta query failed"
                );
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

// --- Transfer ---

fn torrents_map(
    rows: &[TorrentRow],
) -> serde_json::Result<serde_json::Map<String, serde_json::Value>> {
    rows.iter()
        .map(|t| serde_json::to_value(to_qb_torrent(t)).map(|value| (t.hash.clone(), value)))
        .collect()
}

async fn sync_metadata(
    s: &AppState,
) -> anyhow::Result<(serde_json::Map<String, serde_json::Value>, Vec<String>)> {
    let (categories, tags) =
        s.db.run_blocking("qbit_sync_metadata", |db| {
            Ok((db.list_categories()?, db.list_tags()?))
        })
        .await?;
    let categories = categories
        .into_iter()
        .map(|c| {
            (
                c.name.clone(),
                json!({ "name": c.name, "savePath": c.save_path }),
            )
        })
        .collect();
    Ok((categories, tags))
}

async fn transfer_info(State(s): State<AppState>) -> impl IntoResponse {
    let backend_status =
        match tokio::time::timeout(Duration::from_secs(3), s.backend.health()).await {
            Ok(status) => status,
            Err(_) => BackendStatus::Unreachable,
        };
    if backend_status != BackendStatus::Connected {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "backend is unreachable",
                "connection_status": backend_status.as_str(),
            })),
        )
            .into_response();
    }
    let rates = match crate::stats::current_rates_result(s.backend.clone()).await {
        Ok(rates) => rates,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "transfer_info",
                result = "error",
                error = %crate::sync::error_chain(&e),
                "transfer rates unavailable"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "transfer rates unavailable",
                    "connection_status": "unreachable",
                })),
            )
                .into_response();
        }
    };
    let limits = if s.backend.capabilities().supports_global_limits {
        match s.backend.global_limits().await {
            Ok(limits) => limits,
            Err(e) => {
                tracing::warn!(
                    component = "qbcompat",
                    operation = "transfer_info",
                    result = "error",
                    error = %e,
                    "global transfer limits unavailable"
                );
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "error": "global transfer limits unavailable",
                        "connection_status": "unreachable",
                    })),
                )
                    .into_response();
            }
        }
    } else {
        crate::backend::BackendTransferLimits::default()
    };
    let totals = crate::stats::session_totals();

    (
        StatusCode::OK,
        Json(json!({
            "connection_status": "connected",
            "dl_info_speed": rates.download,
            "dl_info_data": totals.download,
            "up_info_speed": rates.upload,
            "up_info_data": totals.upload,
            "dl_rate_limit": limits.download_limit,
            "up_rate_limit": limits.upload_limit,
        })),
    )
        .into_response()
}

fn qb_server_state(rates: TransferRates, connected: bool) -> serde_json::Value {
    json!({
        "connection_status": if connected { "connected" } else { "disconnected" },
        "dl_info_speed": rates.download,
        "up_info_speed": rates.upload,
    })
}

#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use crate::{
        cache::{AppEventRow, TorrentRow},
        rtorrent::TransferRates,
    };

    use super::{
        is_status_filter, qb_server_state, qbit_log_entry, qbit_ratio_limit_milli,
        qbit_status_filter, resolve_hashes, split_hashes, to_qb_torrent, LogMainQuery,
    };

    #[test]
    fn is_status_filter_recognizes_every_bucket_build_where_handles() {
        // Every status string cache::query::build_where() actually matches
        // on must be recognized here too, or qb-compat callers (Sonarr,
        // Radarr, Prowlarr, autobrr) filtering on it silently get treated
        // as a free-text name search instead.
        for status in [
            "downloading",
            "seeding",
            "completed",
            "running",
            "queued",
            "paused",
            "stopped",
            "active",
            "inactive",
            "stalled",
            "stalled_uploading",
            "stalled_downloading",
            "checking",
            "moving",
            "errored",
            "tracker_error",
        ] {
            assert!(
                is_status_filter(status),
                "{status} should be a recognized status filter"
            );
        }
        assert!(!is_status_filter("not-a-real-status"));
    }

    #[test]
    fn qbit_status_filter_does_not_turn_protocol_values_into_name_searches() {
        assert_eq!(qbit_status_filter(None).unwrap(), None);
        assert_eq!(qbit_status_filter(Some("all")).unwrap(), None);
        assert_eq!(
            qbit_status_filter(Some("uploading")).unwrap(),
            Some("seeding".to_owned())
        );
        assert_eq!(
            qbit_status_filter(Some("missingFiles")).unwrap_err(),
            StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(
            qbit_status_filter(Some("not-a-real-status")).unwrap_err(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn split_hashes_deduplicates_repeated_mutation_targets() {
        let dir = tempfile::tempdir().expect("create cache tempdir");
        let db = crate::cache::Db::open(&dir.path().join("cache.db")).expect("open cache");
        assert_eq!(split_hashes(&db, Some("A| a |B|A|B")), vec!["A", "B"]);
        assert!(resolve_hashes(&db, Some("A||B")).is_err());
        assert!(resolve_hashes(&db, Some("all|A")).is_err());
    }

    #[test]
    fn required_hashes_are_existing_and_canonical_case_insensitively() {
        let dir = tempfile::tempdir().expect("create cache tempdir");
        let db = crate::cache::Db::open(&dir.path().join("cache.db")).expect("open cache");
        db.upsert(&torrent_row("ABCDEF", true, true, false))
            .expect("seed cache");

        assert_eq!(
            super::required_resolved_hashes(&db, Some("abcdef")).expect("resolve cached hash"),
            vec!["ABCDEF"]
        );
        assert!(super::required_resolved_hashes(&db, Some("123456")).is_err());
        assert_eq!(
            super::required_resolved_hash(&db, Some("abcdef"))
                .expect("resolve singular cached hash"),
            "ABCDEF"
        );
        assert_eq!(
            super::required_resolved_hash(&db, Some("ALL")).unwrap_err(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            super::required_resolved_hash(&db, Some("123456")).unwrap_err(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            super::resolve_hashes(&db, Some("ALL")).expect("resolve all"),
            vec!["ABCDEF"]
        );
        assert!(super::resolve_hashes(&db, Some("all|ABCDEF")).is_err());
    }

    #[test]
    fn qbit_default_ratio_sentinel_is_preserved() {
        assert_eq!(qbit_ratio_limit_milli(-2.0), -2);
        assert_eq!(qbit_ratio_limit_milli(-1.0), -1);
        assert_eq!(qbit_ratio_limit_milli(1.25), 1_250);
    }

    #[test]
    fn qb_server_state_includes_current_transfer_rates() {
        let state = qb_server_state(
            TransferRates {
                download: 1_234,
                upload: 567,
            },
            true,
        );

        assert_eq!(state["connection_status"], "connected");
        assert_eq!(state["dl_info_speed"], 1_234);
        assert_eq!(state["up_info_speed"], 567);
    }

    #[test]
    fn qbit_log_entry_maps_app_event_levels() {
        let entry = qbit_log_entry(AppEventRow {
            event_id: Some(7),
            occurred_at: 1_700_000_000,
            level: "warning".to_owned(),
            kind: "rtorrent_log".to_owned(),
            message: "tracker warning".to_owned(),
            payload: "{}".to_owned(),
        })
        .unwrap();
        assert_eq!(entry.id, 7);
        assert_eq!(entry.message, "tracker warning");
        assert_eq!(entry.timestamp, 1_700_000_000);
        assert_eq!(entry.kind, 2);
    }

    #[test]
    fn qbit_log_entry_rejects_missing_event_id() {
        assert!(qbit_log_entry(AppEventRow {
            event_id: None,
            occurred_at: 1_700_000_000,
            level: "info".to_owned(),
            kind: "native_event".to_owned(),
            message: "event".to_owned(),
            payload: "{}".to_owned(),
        })
        .is_err());
    }

    #[test]
    fn qbit_log_entry_rejects_corrupt_payload() {
        assert!(qbit_log_entry(AppEventRow {
            event_id: Some(7),
            occurred_at: 1_700_000_000,
            level: "info".to_owned(),
            kind: "native_event".to_owned(),
            message: "event".to_owned(),
            payload: "not json".to_owned(),
        })
        .is_err());
    }

    #[test]
    fn qb_torrent_maps_started_idle_rows_as_stalled() {
        let stalled_downloading = to_qb_torrent(&torrent_row("down", false, true, false));
        let stalled_uploading = to_qb_torrent(&torrent_row("up", false, true, true));

        assert_eq!(stalled_downloading["state"], "stalledDL");
        assert_eq!(stalled_uploading["state"], "stalledUP");
        assert_eq!(stalled_uploading["content_path"], "/downloads/test");
    }

    #[test]
    fn log_main_query_filters_qbit_types() {
        let all = LogMainQuery {
            limit: None,
            last_known_id: None,
            normal: None,
            info: None,
            warning: None,
            critical: None,
        };
        assert!(all.includes_type(1));
        assert!(all.includes_type(2));
        assert!(all.includes_type(4));

        let warning = LogMainQuery {
            limit: None,
            last_known_id: None,
            normal: None,
            info: None,
            warning: Some(true),
            critical: None,
        };
        assert!(!warning.includes_type(1));
        assert!(warning.includes_type(2));
        assert!(!warning.includes_type(4));
    }

    #[test]
    fn redact_log_url_removes_sensitive_url_parts() {
        assert_eq!(
            super::redact_log_url("magnet:?xt=urn:btih:abc"),
            "[redacted-magnet]"
        );
        assert_eq!(
            super::redact_log_url("https://tracker.example/announce?passkey=secret#frag"),
            "https://tracker.example/announce"
        );
        assert_eq!(
            super::redact_log_url("/data/private/file.torrent"),
            "[redacted-path]"
        );
    }

    fn torrent_row(hash: &str, is_active: bool, is_open: bool, complete: bool) -> TorrentRow {
        TorrentRow {
            hash: hash.to_owned(),
            name: hash.to_owned(),
            size_bytes: 100,
            bytes_done: if complete { 100 } else { 0 },
            down_rate: 0,
            up_rate: 0,
            up_total: 0,
            down_total: 0,
            ratio: 0,
            is_active,
            is_open,
            complete,
            state: 1,
            priority: 0,
            category: String::new(),
            base_path: "/downloads/test".to_owned(),
            directory: "/downloads/test".to_owned(),
            creation_date: 0,
            timestamp_finished: 0,
            tracker_focus: 0,
            peers_connected: 0,
            peers_complete: 0,
            message: String::new(),
            tracker_url: String::new(),
            tags: String::new(),
            updated_at: 0,
        }
    }
}

// --- mapping helpers ---

pub fn to_qb_torrent(t: &TorrentRow) -> serde_json::Value {
    let (down_rate, up_rate) = current_row_rates(t);
    let progress = if t.size_bytes > 0 {
        t.bytes_done as f64 / t.size_bytes as f64
    } else {
        0.0
    };
    let state = if !t.is_open {
        "pausedUP"
    } else if t.state == 2 {
        "checkingUP"
    } else if t.complete && t.is_active {
        "uploading"
    } else if !t.complete && t.is_active {
        "downloading"
    } else if t.complete {
        "stalledUP"
    } else if t.state == 1 {
        "stalledDL"
    } else {
        "pausedUP"
    };
    let eta = if down_rate > 0 && t.size_bytes > t.bytes_done {
        (t.size_bytes - t.bytes_done) / down_rate
    } else {
        8_640_000
    };

    json!({
        "hash":          t.hash,
        "name":          t.name,
        "size":          t.size_bytes,
        "progress":      progress,
        "dlspeed":       down_rate,
        "upspeed":       up_rate,
        "priority":      t.priority,
        "num_seeds":     t.peers_complete,
        "num_leechs":    t.peers_connected,
        "ratio":         t.ratio as f64 / 1000.0,
        "eta":           eta,
        "state":         state,
        "category":      t.category,
        "tags":          t.tags,
        "added_on":      t.creation_date,
        "completion_on": t.timestamp_finished,
        "save_path":     t.directory,
        "content_path":  t.base_path,
        "downloaded":    t.down_total,
        "uploaded":      t.up_total,
        "amount_left":   (t.size_bytes - t.bytes_done).max(0),
        "completed":     t.bytes_done,
        "tracker":       t.tracker_url,
        "magnet_uri":    "",
    })
}

fn current_row_rates(t: &TorrentRow) -> (i64, i64) {
    const STALE_AFTER_SECS: i64 = 15;
    let now = chrono::Utc::now().timestamp();
    if t.updated_at <= 0 || now.saturating_sub(t.updated_at) > STALE_AFTER_SECS {
        return (0, 0);
    }
    (t.down_rate.max(0), t.up_rate.max(0))
}

#[cfg(test)]
fn resolve_hashes(db: &crate::cache::Db, s: Option<&str>) -> anyhow::Result<Vec<String>> {
    match s {
        None | Some("") => Ok(vec![]),
        Some(s) if s.trim().eq_ignore_ascii_case("all") => match db.all_hashes() {
            Ok(hashes) => Ok(hashes.into_iter().collect()),
            Err(e) => Err(anyhow::anyhow!("failed to resolve hashes=all: {e}")),
        },
        Some(s) => {
            let values = s.split('|').map(str::trim).collect::<Vec<_>>();
            if values
                .iter()
                .any(|hash| hash.is_empty() || hash.eq_ignore_ascii_case("all"))
            {
                return Err(anyhow::Error::new(InvalidHashTarget(
                    "hashes must contain non-empty torrent hashes or exactly 'all'".to_owned(),
                )));
            }
            let mut seen = std::collections::HashSet::new();
            Ok(values
                .into_iter()
                .filter_map(|hash| {
                    if seen.insert(hash.to_ascii_lowercase()) {
                        Some(hash.to_owned())
                    } else {
                        None
                    }
                })
                .collect())
        }
    }
}

async fn resolve_hashes_async(
    db: &crate::cache::Db,
    s: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    match s {
        None | Some("") => Ok(vec![]),
        Some(s) if s.trim().eq_ignore_ascii_case("all") => db
            .run_blocking("resolve_hashes_all", |db| db.all_hashes())
            .await
            .map(|hashes| hashes.into_iter().collect())
            .map_err(|e| anyhow::anyhow!("failed to resolve hashes=all: {e}")),
        Some(s) => {
            let values = s.split('|').map(str::trim).collect::<Vec<_>>();
            if values
                .iter()
                .any(|hash| hash.is_empty() || hash.eq_ignore_ascii_case("all"))
            {
                return Err(anyhow::Error::new(InvalidHashTarget(
                    "hashes must contain non-empty torrent hashes or exactly 'all'".to_owned(),
                )));
            }
            let mut seen = std::collections::HashSet::new();
            Ok(values
                .into_iter()
                .filter_map(|hash| {
                    if seen.insert(hash.to_ascii_lowercase()) {
                        Some(hash.to_owned())
                    } else {
                        None
                    }
                })
                .collect())
        }
    }
}

async fn required_resolved_hashes_async(
    db: &crate::cache::Db,
    raw: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let raw = raw
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| anyhow::Error::new(InvalidHashTarget("hashes is required".to_owned())))?;
    let hashes = resolve_hashes_async(db, Some(raw)).await?;
    if hashes.is_empty() {
        return Err(anyhow::Error::new(InvalidHashTarget(
            "hashes resolved to no torrents".to_owned(),
        )));
    }

    db.clone()
        .run_blocking("canonicalize_torrent_hashes", move |db| {
            let mut resolved = Vec::with_capacity(hashes.len());
            for hash in hashes {
                let Some(canonical) = db.canonical_hash(&hash).map_err(|error| {
                    anyhow::anyhow!("failed to resolve torrent hash {hash}: {error}")
                })?
                else {
                    return Err(anyhow::Error::new(InvalidHashTarget(format!(
                        "torrent hash not found: {hash}"
                    ))));
                };
                resolved.push(canonical);
            }
            Ok(resolved)
        })
        .await
}

async fn required_resolved_hash_async(
    db: &crate::cache::Db,
    raw: Option<&str>,
) -> Result<String, StatusCode> {
    if raw
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("all"))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    match required_resolved_hashes_async(db, raw).await {
        Ok(mut hashes) if hashes.len() == 1 => Ok(hashes.remove(0)),
        Ok(_) => Err(StatusCode::BAD_REQUEST),
        Err(error) => Err(hash_resolution_status(&error)),
    }
}

#[cfg(test)]
fn required_resolved_hashes(
    db: &crate::cache::Db,
    raw: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let raw = raw
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| anyhow::Error::new(InvalidHashTarget("hashes is required".to_owned())))?;
    let hashes = resolve_hashes(db, Some(raw))?;
    if hashes.is_empty() {
        return Err(anyhow::Error::new(InvalidHashTarget(
            "hashes resolved to no torrents".to_owned(),
        )));
    }

    // Do not delegate existence semantics to the backend. Transmission and
    // some other compatibility targets can acknowledge an unknown id as a
    // successful no-op, which would turn a typo into a false-successful
    // mutation. Resolve each target against the sidecar cache and preserve
    // its canonical spelling for downstream cache/backend operations.
    let mut resolved = Vec::with_capacity(hashes.len());
    for hash in hashes {
        let Some(canonical) = db
            .canonical_hash(&hash)
            .map_err(|error| anyhow::anyhow!("failed to resolve torrent hash {hash}: {error}"))?
        else {
            return Err(anyhow::Error::new(InvalidHashTarget(format!(
                "torrent hash not found: {hash}"
            ))));
        };
        resolved.push(canonical);
    }
    Ok(resolved)
}

#[cfg(test)]
fn required_resolved_hash(db: &crate::cache::Db, raw: Option<&str>) -> Result<String, StatusCode> {
    if raw
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("all"))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    match required_resolved_hashes(db, raw) {
        Ok(mut hashes) if hashes.len() == 1 => Ok(hashes.remove(0)),
        Ok(_) => Err(StatusCode::BAD_REQUEST),
        Err(error) => Err(hash_resolution_status(&error)),
    }
}

#[cfg(test)]
fn split_hashes(db: &crate::cache::Db, s: Option<&str>) -> Vec<String> {
    resolve_hashes(db, s).expect("test hash resolution should succeed")
}

fn parse_peer_addrs(values: &str) -> Result<Vec<SocketAddr>, ()> {
    let peers = values.split('|').collect::<Vec<_>>();
    if peers.is_empty() || peers.iter().any(|peer| peer.trim().is_empty()) {
        return Err(());
    }
    peers
        .into_iter()
        .map(|peer| peer.trim().parse::<SocketAddr>().map_err(|_| ()))
        .collect()
}

async fn emit(s: &AppState, event: Event) {
    record_app_event(s, &event).await;
    let _ = s.events.send(event);
}

async fn record_app_event(s: &AppState, event: &Event) {
    let Some((kind, message, payload)) = app_event_projection(event) else {
        return;
    };
    append_operator_event(s, "info", kind, message, payload).await;
}

async fn record_operator_event(
    s: &AppState,
    level: &str,
    kind: &str,
    message: &str,
    payload: serde_json::Value,
) {
    append_operator_event(
        s,
        level,
        kind.to_owned(),
        message.to_owned(),
        payload.to_string(),
    )
    .await;
}

async fn append_operator_event(
    s: &AppState,
    level: &str,
    kind: String,
    message: String,
    payload: String,
) {
    let event = AppEventRow {
        event_id: None,
        occurred_at: chrono::Utc::now().timestamp(),
        level: level.to_owned(),
        kind,
        message,
        payload,
    };
    let retention = s.cfg.logging.event_retention;
    if let Err(e) =
        s.db.run_blocking("append_operator_event", move |db| {
            db.append_app_event(&event, retention)
        })
        .await
    {
        tracing::warn!(component = "app_events", operation = "append", result = "error", error = %e, "failed to append app event");
    }
}

fn app_event_projection(event: &Event) -> Option<(String, String, String)> {
    let (kind, message) = match event {
        Event::TorrentAdded { .. } => ("torrent_added", "torrent added"),
        Event::TorrentRemoved { .. } => ("torrent_removed", "torrent removed"),
        Event::TorrentUpdated { .. } => ("torrent_updated", "torrent updated"),
        Event::CategoriesUpdated => ("categories_updated", "categories updated"),
        Event::TagsUpdated => ("tags_updated", "tags updated"),
        Event::StorageUpdated => ("storage_updated", "storage updated"),
        Event::RatioGroupsUpdated => ("ratio_groups_updated", "ratio groups updated"),
        Event::WorkflowsUpdated => ("workflows_updated", "workflows updated"),
        Event::WorkflowRunsUpdated => ("workflow_runs_updated", "workflow runs updated"),
        Event::RssRulesUpdated => ("rss_rules_updated", "RSS rules updated"),
        Event::SavedViewsUpdated => ("saved_views_updated", "saved views updated"),
        Event::TrackerHealthUpdated | Event::Stats { .. } => return None,
    };
    Some((
        kind.to_owned(),
        message.to_owned(),
        serde_json::to_string(event).unwrap_or_else(|_| "{}".to_owned()),
    ))
}

async fn emit_torrent_updated(s: &AppState, hash: &str) {
    emit(
        s,
        Event::TorrentUpdated {
            hash: hash.to_owned(),
        },
    )
    .await;
    emit(s, Event::TrackerHealthUpdated).await;
}

async fn update_cached_lifecycle_state(
    s: &AppState,
    hash: &str,
    action: &str,
) -> std::result::Result<(), String> {
    let Some((state, active, open)) = (match action {
        "start" => Some((1, false, true)),
        "stop" => Some((0, false, false)),
        _ => None,
    }) else {
        return Ok(());
    };
    let hash = hash.to_owned();
    s.db.run_blocking("qbit_set_torrent_runtime_state", move |db| {
        db.set_torrent_runtime_state(&hash, state, active, open)
    })
    .await
    .map_err(|error| error.to_string())
}

fn map_sort(s: &str) -> &str {
    match s {
        "name" => "name",
        "size" => "size",
        "added_on" => "added",
        "ratio" => "ratio",
        "dlspeed" => "speed_down",
        "upspeed" => "speed_up",
        "progress" => "progress",
        _ => "name",
    }
}

fn is_status_filter(f: &str) -> bool {
    // Kept in sync with every status arm build_where() (cache/query.rs)
    // actually handles -- a name missing here silently falls back to a
    // free-text name search instead of the intended status bucket.
    matches!(
        f,
        "downloading"
            | "seeding"
            | "completed"
            | "paused"
            | "stopped"
            | "running"
            | "queued"
            | "active"
            | "inactive"
            | "resumed"
            | "stalled"
            | "stalled_uploading"
            | "stalled_downloading"
            | "checking"
            | "moving"
            | "errored"
            | "tracker_error"
    )
}

fn qbit_status_filter(filter: Option<&str>) -> Result<Option<String>, StatusCode> {
    let Some(filter) = filter.map(str::trim) else {
        return Ok(None);
    };
    if filter.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    match filter {
        "all" => Ok(None),
        "uploading" => Ok(Some("seeding".to_owned())),
        "missingFiles" => Err(StatusCode::NOT_IMPLEMENTED),
        value if is_status_filter(value) => Ok(Some(value.to_owned())),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

fn search_plugin_value(name: &str, source: &str, enabled: bool) -> serde_json::Value {
    json!({
        "name": name,
        "fullName": name,
        "version": "",
        "url": source,
        "enabled": enabled,
        "supportedCategories": ["all"],
    })
}

fn plugin_name_from_source(source: &str) -> String {
    source
        .trim_end_matches('/')
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(source)
        .to_owned()
}

fn required_qbit_form_list(
    params: &HashMap<String, String>,
    key: &str,
) -> Result<Vec<String>, StatusCode> {
    let raw = params.get(key).ok_or(StatusCode::BAD_REQUEST)?;
    let values = raw.split(['|', ',']).map(str::trim).collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| value.is_empty()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(values.into_iter().map(str::to_owned).collect())
}

fn required_qbit_list(raw: Option<&str>) -> Result<Vec<String>, ()> {
    let raw = raw.ok_or(())?;
    let values = raw
        .split(['|', '\n', '\r'])
        .map(str::trim)
        .collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| value.is_empty()) {
        return Err(());
    }
    Ok(values.into_iter().map(str::to_owned).collect())
}

fn required_qbit_lines(raw: Option<&str>) -> Result<Vec<String>, ()> {
    let raw = raw.ok_or(())?;
    let values = raw.lines().map(str::trim).collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| value.is_empty()) {
        return Err(());
    }
    Ok(values.into_iter().map(str::to_owned).collect())
}

fn strict_tag_values(raw: Option<&str>, allow_empty: bool) -> Result<Vec<&str>, ()> {
    let raw = raw.ok_or(())?;
    if raw.trim().is_empty() {
        return if allow_empty { Ok(Vec::new()) } else { Err(()) };
    }
    let values = raw.split(',').map(str::trim).collect::<Vec<_>>();
    if values.iter().any(|value| value.is_empty()) {
        return Err(());
    }
    Ok(values)
}

fn parse_wire_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn rss_leaf_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
        .to_owned()
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn redact_log_url(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("magnet:?") {
        return "[redacted-magnet]".to_owned();
    }
    let without_query = value.split(['?', '#']).next().unwrap_or(value);
    if without_query.starts_with('/')
        || without_query.starts_with("~/")
        || without_query.starts_with("./")
        || without_query.starts_with("../")
    {
        return "[redacted-path]".to_owned();
    }
    without_query.to_owned()
}
