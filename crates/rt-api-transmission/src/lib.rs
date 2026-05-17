#![recursion_limit = "256"]

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose, Engine as _};
use rt_engine::{
    EngineGlobalLimits, EngineHandle, EnginePeerSnapshot, EnginePieceState, EngineTorrentMetadata,
    QueueMove,
};
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_session::SessionRegistry;
use serde_json::{json, Value};
use tokio::sync::RwLock;

const SESSION_ID: &str = "rtorrentNG";

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub session: Arc<RwLock<TransmissionSessionSettings>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransmissionSessionSettings {
    pub queue_stalled_enabled: bool,
    pub queue_stalled_minutes: i64,
}

impl Default for TransmissionSessionSettings {
    fn default() -> Self {
        Self {
            queue_stalled_enabled: false,
            queue_stalled_minutes: 30,
        }
    }
}

impl AppState {
    pub fn new(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        Self {
            registry,
            engine: None,
            session: Arc::new(RwLock::new(TransmissionSessionSettings::default())),
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        Self {
            registry,
            engine: Some(engine),
            session: Arc::new(RwLock::new(TransmissionSessionSettings::default())),
        }
    }
}

pub fn build_transmission_router(state: AppState) -> Router {
    Router::new()
        .route("/transmission/rpc", post(rpc))
        .route("/api/transmission/rpc", post(rpc))
        .with_state(state)
}

async fn rpc(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if headers.get("x-transmission-session-id").is_none() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-transmission-session-id",
            HeaderValue::from_static(SESSION_ID),
        );
        return (
            StatusCode::CONFLICT,
            headers,
            Json(json!({"result":"missing session-id"})),
        )
            .into_response();
    }

    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let snake_case_rpc = method.contains('_');
    let method_key = method.replace('_', "-");
    let args = normalize_transmission_request_keys(
        body.get("arguments").cloned().unwrap_or_else(|| json!({})),
    );
    let tag = body.get("tag").cloned();
    let result = match method_key.as_str() {
        "session-get" => Ok(session_get(&state).await),
        "session-stats" => Ok(session_stats(&state).await),
        "session-close" => Ok(json!({})),
        "session-set" => session_set(&state, &args).await,
        "session-access-control" => Ok(json!({
            "blocklist-enabled": false,
            "rpc-authentication-required": false,
            "rpc-whitelist-enabled": false,
        })),
        "group-get" => Ok(json!({ "groups": [] })),
        "group-set" => Ok(json!({})),
        "torrent-set" => torrent_set(&state, &args).await,
        "torrent-set-tracker-list" => torrent_set_tracker_list(&state, &args).await,
        "torrent-set-file-priorities" => torrent_set_file_priorities(&state, &args).await,
        "torrent-set-file-wanted" => torrent_set_file_wanted(&state, &args, true).await,
        "torrent-set-file-unwanted" => torrent_set_file_wanted(&state, &args, false).await,
        "queue-move-top" => transmission_queue_move(&state, &args, QueueMove::Top).await,
        "queue-move-up" => transmission_queue_move(&state, &args, QueueMove::Up).await,
        "queue-move-down" => transmission_queue_move(&state, &args, QueueMove::Down).await,
        "queue-move-bottom" => transmission_queue_move(&state, &args, QueueMove::Bottom).await,
        "queue-stalled-enable" => queue_stalled_set(&state, true).await,
        "queue-stalled-disable" => queue_stalled_set(&state, false).await,
        "port-test" => Ok(json!({"port-is-open": true})),
        "blocklist-update" => Ok(json!({"blocklist-size": 0})),
        "free-space" => Ok(
            json!({"path": args.get("path").and_then(Value::as_str).unwrap_or(""), "size-bytes": 0}),
        ),
        "torrent-get" => Ok(torrent_get(&state, &args).await),
        "torrent-add" => torrent_add(&state, &args).await,
        "torrent-set-location" => {
            let Some(location) = args.get("location").and_then(Value::as_str) else {
                return Json(response(tag.clone(), "missing location", json!({}))).into_response();
            };
            for hash in ids(&state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine
                        .update_torrent_fields(hash, None, Some(std::path::PathBuf::from(location)))
                        .await;
                } else {
                    let mut reg = state.registry.write().await;
                    if let Some(entry) = reg.get_mut(&hash) {
                        entry.save_path = location.to_owned();
                    }
                }
            }
            Ok(json!({}))
        }
        "torrent-rename-path" => torrent_rename_path(&state, &args).await,
        "torrent-start" | "torrent-start-now" => {
            for hash in ids(&state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine.resume_torrent(hash).await;
                }
            }
            Ok(json!({}))
        }
        "torrent-stop" => {
            for hash in ids(&state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine.pause_torrent(hash).await;
                }
            }
            Ok(json!({}))
        }
        "torrent-verify" => {
            for hash in ids(&state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine.recheck_torrent(hash).await;
                }
            }
            Ok(json!({}))
        }
        "torrent-reannounce" => {
            for hash in ids(&state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine.reannounce_torrent(hash).await;
                }
            }
            Ok(json!({}))
        }
        "torrent-remove" => {
            let delete_files = args
                .get("delete-local-data")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            for hash in ids(&state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine.remove_torrent(hash, delete_files).await;
                }
            }
            Ok(json!({}))
        }
        _ => Err("method name not recognized".to_owned()),
    };
    let (result, arguments) = match result {
        Ok(arguments) => {
            let arguments = if snake_case_rpc {
                transmission_response_to_snake_case(arguments)
            } else {
                arguments
            };
            ("success".to_owned(), arguments)
        }
        Err(result) => (result, json!({})),
    };
    let mut response = json!({"result": result, "arguments": arguments});
    if let Some(tag) = tag {
        response["tag"] = tag;
    }
    Json(response).into_response()
}

async fn torrent_set(state: &AppState, args: &Value) -> Result<Value, String> {
    let labels = args.get("labels").and_then(Value::as_array).map(|labels| {
        labels
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    let location = args
        .get("download-dir")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if labels.is_none() && location.is_none() {
        return Ok(json!({}));
    }
    for hash in ids(state, args).await {
        if let Some(labels) = labels.clone() {
            if let Some(engine) = &state.engine {
                let old_labels = {
                    let reg = state.registry.read().await;
                    reg.get(&hash)
                        .map(|entry| entry.tags.clone())
                        .unwrap_or_default()
                };
                let _ = engine
                    .update_torrent_labels(hash.clone(), None, labels.clone(), old_labels)
                    .await;
            } else {
                let mut reg = state.registry.write().await;
                if let Some(entry) = reg.get_mut(&hash) {
                    entry.tags = labels;
                }
            }
        }
        if let Some(location) = &location {
            if let Some(engine) = &state.engine {
                let _ = engine
                    .update_torrent_fields(
                        hash.clone(),
                        None,
                        Some(std::path::PathBuf::from(location)),
                    )
                    .await;
            } else {
                let mut reg = state.registry.write().await;
                if let Some(entry) = reg.get_mut(&hash) {
                    entry.save_path = location.clone();
                }
            }
        }
    }
    Ok(json!({}))
}

async fn torrent_set_tracker_list(state: &AppState, args: &Value) -> Result<Value, String> {
    let trackers = transmission_tracker_list_arg(args);
    let Some(engine) = &state.engine else {
        return Ok(json!({}));
    };
    for hash in ids(state, args).await {
        engine
            .update_torrent_trackers(hash, trackers.clone())
            .await?;
    }
    Ok(json!({}))
}

async fn torrent_set_file_wanted(
    state: &AppState,
    args: &Value,
    wanted: bool,
) -> Result<Value, String> {
    let Some(engine) = &state.engine else {
        return Ok(json!({}));
    };
    let key = if wanted {
        "files-wanted"
    } else {
        "files-unwanted"
    };
    let file_ids = file_ids_arg(args, key);
    for hash in ids(state, args).await {
        engine
            .update_file_priorities(hash, file_ids.clone(), if wanted { 1 } else { 0 })
            .await?;
    }
    Ok(json!({}))
}

fn transmission_tracker_list_arg(args: &Value) -> Vec<String> {
    let value = args
        .get("trackerList")
        .or_else(|| args.get("tracker-list"))
        .or_else(|| args.get("trackers"));
    let Some(value) = value else {
        return Vec::new();
    };
    let mut trackers = Vec::new();
    collect_tracker_values(value, &mut trackers);
    normalize_tracker_values(trackers)
}

fn collect_tracker_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.to_owned()),
        Value::Array(values) => {
            for value in values {
                collect_tracker_values(value, out);
            }
        }
        Value::Object(obj) => {
            if let Some(announce) = obj.get("announce").and_then(Value::as_str) {
                out.push(announce.to_owned());
            }
        }
        _ => {}
    }
}

fn normalize_tracker_values(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !out.iter().any(|existing| existing == value) {
            out.push(value.to_owned());
        }
    }
    out
}

async fn torrent_set_file_priorities(state: &AppState, args: &Value) -> Result<Value, String> {
    let Some(engine) = &state.engine else {
        return Ok(json!({}));
    };
    let mut updates = Vec::new();
    let high = file_ids_arg(args, "priority-high");
    if !high.is_empty() {
        updates.push((high, 2));
    }
    let normal = file_ids_arg(args, "priority-normal");
    if !normal.is_empty() {
        updates.push((normal, 1));
    }
    let low = file_ids_arg(args, "priority-low");
    if !low.is_empty() {
        updates.push((low, 1));
    }
    if updates.is_empty() {
        return Ok(json!({}));
    }
    let hashes = ids(state, args).await;
    for hash in hashes {
        for (file_ids, priority) in &updates {
            engine
                .update_file_priorities(hash.clone(), file_ids.clone(), *priority)
                .await?;
        }
    }
    Ok(json!({}))
}

async fn transmission_queue_move(
    state: &AppState,
    args: &Value,
    queue_move: QueueMove,
) -> Result<Value, String> {
    let hashes = ids(state, args).await;
    let Some(engine) = &state.engine else {
        return Ok(json!({}));
    };
    engine.update_queue_order(hashes, queue_move).await?;
    Ok(json!({}))
}

async fn torrent_rename_path(state: &AppState, args: &Value) -> Result<Value, String> {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Err("missing path".to_owned());
    };
    let Some(name) = args.get("name").and_then(Value::as_str) else {
        return Err("missing name".to_owned());
    };
    let hashes = ids(state, args).await;
    let Some(engine) = &state.engine else {
        return Ok(json!({ "path": path, "name": name }));
    };
    for hash in hashes {
        let meta = engine.torrent_metadata(hash.clone()).await?;
        if let Some(file) = meta.files.iter().find(|file| file.path == path) {
            let new_path = renamed_file_path(path, name);
            engine.rename_file_path(hash, file.index, new_path).await?;
        } else {
            engine
                .rename_folder_path(hash, path.to_owned(), name.to_owned())
                .await?;
        }
    }
    Ok(json!({ "path": path, "name": name }))
}

fn renamed_file_path(path: &str, name: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => format!("{parent}/{name}"),
        _ => name.to_owned(),
    }
}

fn file_ids_arg(args: &Value, key: &str) -> Vec<u32> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_u64().and_then(|id| u32::try_from(id).ok()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn response(tag: Option<Value>, result: &str, arguments: Value) -> Value {
    let mut response = json!({"result": result, "arguments": arguments});
    if let Some(tag) = tag {
        response["tag"] = tag;
    }
    response
}

fn normalize_transmission_request_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    (
                        key.replace('_', "-"),
                        normalize_transmission_request_keys(value),
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(normalize_transmission_request_keys)
                .collect(),
        ),
        other => other,
    }
}

fn transmission_response_to_snake_case(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    (
                        transmission_key_to_snake_case(&key),
                        transmission_response_to_snake_case(value),
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(transmission_response_to_snake_case)
                .collect(),
        ),
        other => other,
    }
}

fn transmission_key_to_snake_case(key: &str) -> String {
    if key.contains('-') {
        return key.replace('-', "_");
    }
    let mut out = String::with_capacity(key.len());
    for (idx, ch) in key.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if idx > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

async fn session_get(state: &AppState) -> Value {
    let limits = transmission_global_limits(state).await;
    let session = state.session.read().await.clone();
    json!({
        "version": "rtorrentNG",
        "rpc-version": 17,
        "rpc-version-minimum": 1,
        "rpc-version-semver": "6.0.0",
        "session-id": SESSION_ID,
        "download-dir": default_download_dir(state).await,
        "config-dir": "/config",
        "incomplete-dir": "",
        "incomplete-dir-enabled": false,
        "rename-partial-files": false,
        "start-added-torrents": true,
        "trash-original-torrent-files": false,
        "speed-limit-down-enabled": limits.download_limit > 0,
        "speed-limit-up-enabled": limits.upload_limit > 0,
        "speed-limit-down": bytes_to_transmission_kib(limits.download_limit),
        "speed-limit-up": bytes_to_transmission_kib(limits.upload_limit),
        "alt-speed-enabled": limits.speed_limits_mode,
        "alt-speed-down": bytes_to_transmission_kib(limits.download_limit),
        "alt-speed-up": bytes_to_transmission_kib(limits.upload_limit),
        "download-queue-enabled": false,
        "download-queue-size": 0,
        "seed-queue-enabled": false,
        "seed-queue-size": 0,
        "queue-stalled-enabled": session.queue_stalled_enabled,
        "queue-stalled-minutes": session.queue_stalled_minutes,
        "peer-limit-global": 0,
        "peer-limit-per-torrent": 0,
        "script-torrent-added-enabled": false,
        "script-torrent-done-enabled": false,
        "script-torrent-done-seeding-enabled": false,
        "blocklist-enabled": false,
        "blocklist-size": 0,
        "blocklist-url": "",
        "utp-enabled": true,
        "lpd-enabled": false,
        "dht-enabled": true,
        "pex-enabled": true,
        "peer-port": 0,
        "port-forwarding-enabled": false,
        "seedRatioLimit": -1.0,
        "seedRatioLimited": false,
        "idle-seeding-limit": 0,
        "idle-seeding-limit-enabled": false,
        "units": {
            "speed-units": ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"],
            "speed-bytes": 1000,
            "size-units": ["B", "KB", "MB", "GB", "TB"],
            "size-bytes": 1000,
            "memory-units": ["B", "KiB", "MiB", "GiB", "TiB"],
            "memory-bytes": 1024,
        },
    })
}

async fn session_stats(state: &AppState) -> Value {
    let reg = state.registry.read().await;
    let torrent_count = reg.iter().count();
    let (downloaded, uploaded) = reg.iter().fold((0_u64, 0_u64), |(down, up), entry| {
        (
            down.saturating_add(entry.stats.downloaded),
            up.saturating_add(entry.stats.uploaded),
        )
    });
    json!({
        "activeTorrentCount": torrent_count,
        "downloadSpeed": 0,
        "pausedTorrentCount": reg.iter().filter(|entry| matches!(entry.state.as_str(), "paused" | "stopped")).count(),
        "torrentCount": torrent_count,
        "uploadSpeed": 0,
        "cumulative-stats": {
            "downloadedBytes": downloaded,
            "uploadedBytes": uploaded,
            "filesAdded": torrent_count,
            "sessionCount": 1,
            "secondsActive": 0,
        },
        "current-stats": {
            "downloadedBytes": downloaded,
            "uploadedBytes": uploaded,
            "filesAdded": torrent_count,
            "sessionCount": 1,
            "secondsActive": 0,
        }
    })
}

async fn torrent_get(state: &AppState, args: &Value) -> Value {
    let fields = args
        .get("fields")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            ["hashString", "name", "totalSize", "percentDone", "status"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        });
    let requested = ids(state, args).await;
    let entries = {
        let reg = state.registry.read().await;
        reg.iter()
            .filter(|entry| requested.is_empty() || requested.contains(&entry.info_hash))
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut metadata = std::collections::HashMap::new();
    let mut queue_positions = std::collections::HashMap::new();
    if let Some(engine) = &state.engine {
        for entry in &entries {
            if let Ok(meta) = engine.torrent_metadata(entry.info_hash.clone()).await {
                metadata.insert(entry.info_hash.clone(), meta);
            }
            if let Ok(position) = engine.queue_priority(entry.info_hash.clone()).await {
                queue_positions.insert(entry.info_hash.clone(), position);
            }
        }
    }
    let mut peers = std::collections::HashMap::new();
    if let Some(engine) = &state.engine {
        for entry in &entries {
            if fields
                .iter()
                .any(|field| transmission_field_needs_peers(field))
            {
                if let Ok(snapshot) = engine.torrent_peers(entry.info_hash.clone()).await {
                    peers.insert(entry.info_hash.clone(), snapshot);
                }
            }
        }
    }
    let torrents = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let meta = metadata.get(&entry.info_hash);
            let mut obj = serde_json::Map::new();
            for field in &fields {
                let normalized_field = field.replace('_', "-");
                let value = match normalized_field.as_str() {
                    "id" => json!(idx + 1),
                    "hashString" | "hash-string" => json!(entry.info_hash),
                    "name" => json!(entry.name),
                    "totalSize" | "total-size" => json!(entry.total_length),
                    "sizeWhenDone" | "size-when-done" => json!(entry.total_length),
                    "leftUntilDone" | "left-until-done" => json!(entry.amount_left),
                    "percentComplete" | "percent-complete" => {
                        json!(percent_done(entry.total_length, entry.amount_left))
                    }
                    "percentDone" | "percent-done" => {
                        json!(percent_done(entry.total_length, entry.amount_left))
                    }
                    "bytesCompleted" | "bytes-completed" => {
                        json!(entry.total_length.saturating_sub(entry.amount_left))
                    }
                    "availability" => json!(transmission_availability(meta)),
                    "downloadedEver" | "downloaded-ever" => json!(entry.stats.downloaded),
                    "uploadedEver" | "uploaded-ever" => json!(entry.stats.uploaded),
                    "uploadRatio" | "upload-ratio" => json!(entry.stats.ratio()),
                    "rateDownload" | "rate-download" => json!(0),
                    "rateUpload" | "rate-upload" => json!(0),
                    "downloadLimit" | "download-limit" => json!(0),
                    "downloadLimited" | "download-limited" => json!(false),
                    "uploadLimit" | "upload-limit" => json!(0),
                    "uploadLimited" | "upload-limited" => json!(false),
                    "status" => json!(transmission_status(entry.state.as_str())),
                    "downloadDir" | "download-dir" => json!(entry.save_path),
                    "labels" => json!(entry.tags),
                    "error" => json!(0),
                    "errorString" | "error-string" => {
                        json!(entry.error_message.clone().unwrap_or_default())
                    }
                    "eta" => json!(-1),
                    "etaIdle" | "eta-idle" => json!(-1),
                    "isPrivate" | "is-private" => {
                        json!(meta.map(|m| m.is_private).unwrap_or(false))
                    }
                    "isFinished" | "is-finished" => json!(entry.completed_at.is_some()),
                    "isStalled" | "is-stalled" => json!(false),
                    "queuePosition" | "queue-position" => {
                        json!(queue_positions
                            .get(&entry.info_hash)
                            .copied()
                            .unwrap_or(idx as i32))
                    }
                    "recheckProgress" | "recheck-progress" => json!(0.0),
                    "seedRatioLimit" | "seed-ratio-limit" => json!(-1.0),
                    "seedRatioMode" | "seed-ratio-mode" => json!(0),
                    "seedIdleLimit" | "seed-idle-limit" => json!(0),
                    "seedIdleMode" | "seed-idle-mode" => json!(0),
                    "addedDate" | "added-date" => json!(entry.added_at),
                    "activityDate" | "activity-date" => json!(entry.added_at),
                    "doneDate" | "done-date" => json!(entry.completed_at.unwrap_or(0)),
                    "startDate" | "start-date" => json!(entry.added_at),
                    "dateCreated" | "date-created" => json!(entry.added_at),
                    "peers" => json!(transmission_peers(
                        peers
                            .get(&entry.info_hash)
                            .map(Vec::as_slice)
                            .unwrap_or(&[])
                    )),
                    "peersConnected" | "peers-connected" => {
                        json!(peers.get(&entry.info_hash).map(Vec::len).unwrap_or(0))
                    }
                    "peersGettingFromUs" | "peers-getting-from-us" => json!(peers
                        .get(&entry.info_hash)
                        .map(|peers| peers.iter().filter(|peer| peer.upload_rate > 0).count())
                        .unwrap_or(0)),
                    "peersSendingToUs" | "peers-sending-to-us" => json!(peers
                        .get(&entry.info_hash)
                        .map(|peers| peers.iter().filter(|peer| peer.download_rate > 0).count())
                        .unwrap_or(0)),
                    "peersFrom" | "peers-from" => json!({
                        "fromCache": 0,
                        "fromDht": 0,
                        "fromIncoming": 0,
                        "fromLpd": 0,
                        "fromLtep": 0,
                        "fromPex": 0,
                        "fromTracker": peers.get(&entry.info_hash).map(Vec::len).unwrap_or(0),
                    }),
                    "trackers" => json!(transmission_trackers(meta)),
                    "trackerStats" | "tracker-stats" => json!(transmission_tracker_stats(meta)),
                    "files" => json!(transmission_files(entry, meta)),
                    "fileStats" | "file-stats" => json!(transmission_file_stats(entry, meta)),
                    "priorities" => json!(transmission_file_priorities(meta)),
                    "wanted" => json!(transmission_file_wanted(meta)),
                    "comment" => json!(""),
                    "creator" => json!(""),
                    "primaryMimeType" | "primary-mime-type" => json!(""),
                    "pieceCount" | "piece-count" => json!(meta.map(|m| m.piece_count).unwrap_or(0)),
                    "pieceSize" | "piece-size" => json!(meta.map(|m| m.piece_length).unwrap_or(0)),
                    "pieces" => json!(""),
                    "haveUnchecked" | "have-unchecked" => json!(0),
                    "haveValid" | "have-valid" => {
                        json!(entry.total_length.saturating_sub(entry.amount_left))
                    }
                    "desiredAvailable" | "desired-available" => json!(0),
                    "corruptEver" | "corrupt-ever" => json!(0),
                    "manualAnnounceTime" | "manual-announce-time" => json!(0),
                    "maxConnectedPeers" | "max-connected-peers" => json!(0),
                    "webseeds" => json!(meta.map(|m| m.webseeds.clone()).unwrap_or_default()),
                    "webseedsSendingToUs" | "webseeds-sending-to-us" => json!(0),
                    "webseedsEx" | "webseeds-ex" => json!(transmission_webseeds_ex(meta)),
                    "bandwidthPriority" | "bandwidth-priority" => json!(0),
                    "honorsSessionLimits" | "honors-session-limits" => json!(true),
                    "group" => json!("default"),
                    "magnetLink" | "magnet-link" => {
                        json!(transmission_magnet_link(&entry.info_hash))
                    }
                    "metadataPercentComplete" | "metadata-percent-complete" => {
                        json!(if entry.state.as_str() == "metadata_pending" {
                            0.0
                        } else {
                            1.0
                        })
                    }
                    "secondsDownloading" | "seconds-downloading" => json!(0),
                    "secondsSeeding" | "seconds-seeding" => json!(0),
                    "sequentialDownload" | "sequential-download" => json!(false),
                    "sequentialDownloadFromPiece" | "sequential-download-from-piece" => json!(0),
                    _ => Value::Null,
                };
                obj.insert(field.clone(), value);
            }
            Value::Object(obj)
        })
        .collect::<Vec<_>>();
    json!({ "torrents": torrents })
}

fn transmission_field_needs_peers(field: &str) -> bool {
    let field = field.replace('_', "-");
    matches!(
        field.as_str(),
        "peers"
            | "peersConnected"
            | "peers-connected"
            | "peersGettingFromUs"
            | "peers-getting-from-us"
            | "peersSendingToUs"
            | "peers-sending-to-us"
    )
}

fn transmission_magnet_link(info_hash: &str) -> String {
    if info_hash.len() == 64 && info_hash.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("magnet:?xt=urn:btmh:1220{}", info_hash.to_ascii_lowercase())
    } else {
        format!("magnet:?xt=urn:btih:{info_hash}")
    }
}

fn transmission_peers(peers: &[EnginePeerSnapshot]) -> Vec<Value> {
    peers
        .iter()
        .map(|peer| {
            json!({
                "address": peer.addr.ip().to_string(),
                "clientName": peer.client,
                "clientIsChoked": peer.upload_choked,
                "clientIsInterested": peer.interested,
                "flagStr": "",
                "isDownloadingFrom": peer.download_rate > 0,
                "isEncrypted": false,
                "isIncoming": false,
                "isUTP": false,
                "isUploadingTo": peer.upload_rate > 0,
                "isSeed": peer.progress >= 1.0,
                "peerIsChoked": peer.choked,
                "peerIsInterested": peer.interested,
                "port": peer.addr.port(),
                "progress": peer.progress,
                "rateToClient": peer.upload_rate,
                "rateToPeer": peer.download_rate,
            })
        })
        .collect()
}

fn transmission_files(
    entry: &rt_session::TorrentEntry,
    meta: Option<&EngineTorrentMetadata>,
) -> Vec<Value> {
    meta.map(|meta| {
        let completed = file_completed_bytes(entry, meta);
        meta.files
            .iter()
            .enumerate()
            .map(|(idx, file)| {
                json!({
                    "name": file.path,
                    "length": file.length,
                    "bytesCompleted": completed[idx],
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn transmission_file_stats(
    entry: &rt_session::TorrentEntry,
    meta: Option<&EngineTorrentMetadata>,
) -> Vec<Value> {
    meta.map(|meta| {
        let completed = file_completed_bytes(entry, meta);
        meta.files
            .iter()
            .enumerate()
            .map(|(idx, file)| {
                json!({
                    "bytesCompleted": completed[idx],
                    "wanted": file.wanted,
                    "priority": file.priority,
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn file_completed_bytes(
    entry: &rt_session::TorrentEntry,
    meta: &EngineTorrentMetadata,
) -> Vec<u64> {
    let done = entry.total_length.saturating_sub(entry.amount_left);
    let mut offset = 0u64;
    meta.files
        .iter()
        .map(|file| {
            let file_start = offset;
            offset = offset.saturating_add(file.length);
            done.saturating_sub(file_start).min(file.length)
        })
        .collect()
}

fn transmission_file_priorities(meta: Option<&EngineTorrentMetadata>) -> Vec<i64> {
    meta.map(|meta| meta.files.iter().map(|file| file.priority).collect())
        .unwrap_or_default()
}

fn transmission_file_wanted(meta: Option<&EngineTorrentMetadata>) -> Vec<bool> {
    meta.map(|meta| meta.files.iter().map(|file| file.wanted).collect())
        .unwrap_or_default()
}

fn transmission_availability(meta: Option<&EngineTorrentMetadata>) -> Vec<i64> {
    meta.map(|meta| {
        meta.piece_states
            .iter()
            .map(|state| match state {
                EnginePieceState::Complete | EnginePieceState::Partial => 1,
                EnginePieceState::Missing => 0,
            })
            .collect()
    })
    .unwrap_or_default()
}

fn transmission_webseeds_ex(meta: Option<&EngineTorrentMetadata>) -> Vec<Value> {
    meta.map(|meta| {
        meta.webseeds
            .iter()
            .map(|url| {
                json!({
                    "url": url,
                    "is_downloading": false,
                    "download_bytes_per_second": 0,
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn transmission_trackers(meta: Option<&EngineTorrentMetadata>) -> Vec<Value> {
    meta.map(|meta| {
        meta.trackers
            .iter()
            .enumerate()
            .map(|(id, announce)| {
                json!({
                    "id": id,
                    "announce": announce,
                    "scrape": "",
                    "tier": id,
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn transmission_tracker_stats(meta: Option<&EngineTorrentMetadata>) -> Vec<Value> {
    meta.map(|meta| {
        meta.trackers
            .iter()
            .enumerate()
            .map(|(id, announce)| {
                json!({
                    "id": id,
                    "announce": announce,
                    "host": tracker_host(announce),
                    "tier": id,
                    "lastAnnounceSucceeded": false,
                    "lastAnnounceTime": 0,
                    "lastAnnounceResult": "",
                    "nextAnnounceTime": 0,
                    "lastScrapeSucceeded": false,
                    "lastScrapeTime": 0,
                    "lastScrapeResult": "",
                    "nextScrapeTime": 0,
                    "seederCount": -1,
                    "leecherCount": -1,
                    "downloadCount": -1,
                    "hasAnnounced": false,
                    "hasScraped": false,
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn tracker_host(announce: &str) -> String {
    announce
        .split("://")
        .nth(1)
        .unwrap_or(announce)
        .split('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

async fn torrent_add(state: &AppState, args: &Value) -> Result<Value, String> {
    let Some(engine) = &state.engine else {
        return Err("engine unavailable".to_owned());
    };
    let paused = args.get("paused").and_then(Value::as_bool).unwrap_or(false);
    let download_dir = args
        .get("download-dir")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    let labels = args
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let hash = if let Some(filename) = args.get("filename").and_then(Value::as_str) {
        let magnet = parse_magnet(filename).map_err(|e| e.to_string())?;
        engine
            .add_magnet_with_labels(magnet, download_dir, paused, None, labels)
            .await?
    } else if let Some(metainfo) = args.get("metainfo").and_then(Value::as_str) {
        let raw = general_purpose::STANDARD
            .decode(metainfo)
            .map_err(|e| e.to_string())?;
        let meta = parse_torrent(&raw).map_err(|e| e.to_string())?;
        engine
            .add_torrent_with_labels(meta, download_dir, paused, None, labels)
            .await?
    } else {
        return Err("missing filename or metainfo".to_owned());
    };
    Ok(json!({ "torrent-added": { "hashString": hash } }))
}

async fn ids(state: &AppState, args: &Value) -> Vec<String> {
    let Some(values) = args.get("ids").and_then(Value::as_array) else {
        return Vec::new();
    };
    let reg = state.registry.read().await;
    values
        .iter()
        .filter_map(|value| {
            if let Some(hash) = value.as_str() {
                Some(hash.to_owned())
            } else {
                value.as_u64().and_then(|id| {
                    reg.iter()
                        .nth(id.saturating_sub(1) as usize)
                        .map(|entry| entry.info_hash.clone())
                })
            }
        })
        .collect()
}

async fn default_download_dir(state: &AppState) -> String {
    let reg = state.registry.read().await;
    let dir = reg
        .iter()
        .next()
        .map(|entry| entry.save_path.clone())
        .unwrap_or_else(|| "/downloads".to_owned());
    dir
}

fn percent_done(total: u64, left: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        total.saturating_sub(left) as f64 / total as f64
    }
}

async fn session_set(state: &AppState, args: &Value) -> Result<Value, String> {
    if let Some(engine) = &state.engine {
        let mut limits = engine.global_limits().await.unwrap_or_default();
        if let Some(value) = transmission_i64_arg(args, "speed-limit-down")
            .or_else(|| transmission_i64_arg(args, "alt-speed-down"))
        {
            limits.download_limit = transmission_kib_to_bytes(value);
        }
        if let Some(value) = transmission_i64_arg(args, "speed-limit-up")
            .or_else(|| transmission_i64_arg(args, "alt-speed-up"))
        {
            limits.upload_limit = transmission_kib_to_bytes(value);
        }
        if matches!(
            transmission_bool_arg(args, "speed-limit-down-enabled"),
            Some(false)
        ) {
            limits.download_limit = 0;
        }
        if matches!(
            transmission_bool_arg(args, "speed-limit-up-enabled"),
            Some(false)
        ) {
            limits.upload_limit = 0;
        }
        if let Some(enabled) = transmission_bool_arg(args, "alt-speed-enabled") {
            limits.speed_limits_mode = enabled;
        }
        engine.update_global_limits(limits).await?;
    }
    if let Some(enabled) = transmission_bool_arg(args, "queue-stalled-enabled") {
        state.session.write().await.queue_stalled_enabled = enabled;
    }
    if let Some(minutes) = transmission_i64_arg(args, "queue-stalled-minutes") {
        state.session.write().await.queue_stalled_minutes = minutes.max(0);
    }
    Ok(json!({}))
}

async fn queue_stalled_set(state: &AppState, enabled: bool) -> Result<Value, String> {
    state.session.write().await.queue_stalled_enabled = enabled;
    Ok(json!({}))
}

async fn transmission_global_limits(state: &AppState) -> EngineGlobalLimits {
    let Some(engine) = &state.engine else {
        return EngineGlobalLimits::default();
    };
    engine.global_limits().await.unwrap_or_default()
}

fn transmission_i64_arg(args: &Value, key: &str) -> Option<i64> {
    args.get(key)
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
}

fn transmission_bool_arg(args: &Value, key: &str) -> Option<bool> {
    match args.get(key)? {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => Some(value.as_i64()? != 0),
        Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn transmission_kib_to_bytes(value: i64) -> i64 {
    value.max(0).saturating_mul(1024)
}

fn bytes_to_transmission_kib(value: i64) -> i64 {
    value.max(0) / 1024
}

fn transmission_status(state: &str) -> i64 {
    match state {
        "paused" | "stopped" => 0,
        "checking" => 2,
        "downloading" | "metadata_pending" => 4,
        "seeding" => 6,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use rt_engine::{EnginePieceState, EngineTorrentFile};
    use rt_session::TorrentEntry;
    use tower::ServiceExt;

    #[tokio::test]
    async fn transmission_session_id_handshake() {
        let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
        let app = build_transmission_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"method":"session-get"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(resp.headers().contains_key("x-transmission-session-id"));
    }

    #[tokio::test]
    async fn transmission_torrent_get_projects_registry() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data".into());
            entry.total_length = 100;
            entry.amount_left = 25;
            reg.add(entry).unwrap();
        }
        let app = build_transmission_router(AppState::new(registry));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"torrent-get","arguments":{"fields":["hashString","name","percentDone","eta","files","trackers","magnetLink"]}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["result"], "success");
        assert_eq!(body["arguments"]["torrents"][0]["name"], "alpha");
        assert_eq!(body["arguments"]["torrents"][0]["percentDone"], 0.75);
        assert_eq!(body["arguments"]["torrents"][0]["eta"], -1);
        assert!(body["arguments"]["torrents"][0]["files"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(body["arguments"]["torrents"][0]["magnetLink"]
            .as_str()
            .unwrap()
            .starts_with("magnet:?xt=urn:btih:"));
    }

    #[tokio::test]
    async fn transmission_response_field_matrix_is_present() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data".into());
            entry.total_length = 100;
            entry.amount_left = 25;
            reg.add(entry).unwrap();
        }
        let app = build_transmission_router(AppState::new(registry));
        let fields = [
            "id",
            "hashString",
            "name",
            "totalSize",
            "sizeWhenDone",
            "leftUntilDone",
            "percentComplete",
            "percentDone",
            "bytesCompleted",
            "availability",
            "downloadedEver",
            "uploadedEver",
            "uploadRatio",
            "rateDownload",
            "rateUpload",
            "downloadLimit",
            "downloadLimited",
            "uploadLimit",
            "uploadLimited",
            "status",
            "downloadDir",
            "labels",
            "error",
            "errorString",
            "eta",
            "etaIdle",
            "isPrivate",
            "isFinished",
            "isStalled",
            "queuePosition",
            "recheckProgress",
            "seedRatioLimit",
            "seedRatioMode",
            "seedIdleLimit",
            "seedIdleMode",
            "addedDate",
            "activityDate",
            "doneDate",
            "startDate",
            "dateCreated",
            "peers",
            "peersConnected",
            "peersGettingFromUs",
            "peersSendingToUs",
            "peersFrom",
            "trackers",
            "trackerStats",
            "files",
            "fileStats",
            "priorities",
            "wanted",
            "comment",
            "creator",
            "primaryMimeType",
            "pieceCount",
            "pieceSize",
            "pieces",
            "haveUnchecked",
            "haveValid",
            "desiredAvailable",
            "corruptEver",
            "manualAnnounceTime",
            "maxConnectedPeers",
            "webseeds",
            "webseedsSendingToUs",
            "webseedsEx",
            "bandwidthPriority",
            "honorsSessionLimits",
            "group",
            "magnetLink",
            "metadataPercentComplete",
            "secondsDownloading",
            "secondsSeeding",
            "sequentialDownload",
            "sequentialDownloadFromPiece",
        ];
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(format!(
                        r#"{{"method":"torrent-get","arguments":{{"fields":{}}}}}"#,
                        serde_json::to_string(fields.as_slice()).unwrap()
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 16384).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let torrent = &body["arguments"]["torrents"][0];
        for field in fields {
            assert!(
                torrent.get(field).is_some_and(|value| !value.is_null()),
                "missing or null Transmission torrent field {field}: {torrent:?}"
            );
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(r#"{"method":"session-get"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 16384).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_json_keys(
            &body["arguments"],
            &[
                "version",
                "rpc-version",
                "rpc-version-minimum",
                "rpc-version-semver",
                "session-id",
                "download-dir",
                "config-dir",
                "incomplete-dir",
                "incomplete-dir-enabled",
                "rename-partial-files",
                "start-added-torrents",
                "trash-original-torrent-files",
                "speed-limit-down-enabled",
                "speed-limit-up-enabled",
                "speed-limit-down",
                "speed-limit-up",
                "alt-speed-enabled",
                "alt-speed-down",
                "alt-speed-up",
                "download-queue-enabled",
                "download-queue-size",
                "seed-queue-enabled",
                "seed-queue-size",
                "queue-stalled-enabled",
                "queue-stalled-minutes",
                "peer-limit-global",
                "peer-limit-per-torrent",
                "script-torrent-added-enabled",
                "script-torrent-done-enabled",
                "script-torrent-done-seeding-enabled",
                "blocklist-enabled",
                "blocklist-size",
                "blocklist-url",
                "utp-enabled",
                "lpd-enabled",
                "dht-enabled",
                "pex-enabled",
                "peer-port",
                "port-forwarding-enabled",
                "seedRatioLimit",
                "seedRatioLimited",
                "idle-seeding-limit",
                "idle-seeding-limit-enabled",
                "units",
            ],
        );
    }

    fn assert_json_keys(value: &Value, keys: &[&str]) {
        let obj = value.as_object().expect("expected JSON object");
        for key in keys {
            assert!(obj.contains_key(*key), "missing key {key} in {obj:?}");
        }
    }

    #[tokio::test]
    async fn transmission_torrent_get_projects_v2_magnet_links() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("B".repeat(64), "v2".into(), "/data".into());
            entry.total_length = 100;
            entry.amount_left = 100;
            reg.add(entry).unwrap();
        }
        let app = build_transmission_router(AppState::new(registry));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"torrent-get","arguments":{"fields":["hashString","magnetLink"]}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["arguments"]["torrents"][0]["magnetLink"],
            format!("magnet:?xt=urn:btmh:1220{}", "b".repeat(64))
        );
    }

    #[test]
    fn transmission_magnet_link_formats_v1_and_v2() {
        assert_eq!(
            transmission_magnet_link(&"a".repeat(40)),
            format!("magnet:?xt=urn:btih:{}", "a".repeat(40))
        );
        assert_eq!(
            transmission_magnet_link(&"A".repeat(64)),
            format!("magnet:?xt=urn:btmh:1220{}", "a".repeat(64))
        );
    }

    #[test]
    fn transmission_file_completion_is_projected_per_file() {
        let mut entry = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data".into());
        entry.total_length = 300;
        entry.amount_left = 125;
        let meta = EngineTorrentMetadata {
            piece_length: 100,
            piece_count: 3,
            piece_hashes: Vec::new(),
            piece_states: vec![
                EnginePieceState::Complete,
                EnginePieceState::Partial,
                EnginePieceState::Missing,
            ],
            is_private: false,
            trackers: Vec::new(),
            webseeds: vec!["https://seed.example/one.bin".to_owned()],
            files: vec![
                EngineTorrentFile {
                    index: 0,
                    path: "one.bin".into(),
                    length: 100,
                    priority: 1,
                    wanted: true,
                },
                EngineTorrentFile {
                    index: 1,
                    path: "two.bin".into(),
                    length: 200,
                    priority: 0,
                    wanted: false,
                },
            ],
        };

        let files = transmission_files(&entry, Some(&meta));
        assert_eq!(files[0]["bytesCompleted"], 100);
        assert_eq!(files[1]["bytesCompleted"], 75);
        let stats = transmission_file_stats(&entry, Some(&meta));
        assert_eq!(stats[1]["bytesCompleted"], 75);
    }

    #[tokio::test]
    async fn transmission_stats_and_location_are_supported() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let entry = TorrentEntry::new("b".repeat(40), "beta".into(), "/old".into());
            reg.add(entry).unwrap();
        }
        let app = build_transmission_router(AppState::new(Arc::clone(&registry)));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(r#"{"method":"session-stats"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["arguments"]["torrentCount"], 1);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(format!(
                        r#"{{"method":"torrent-set-location","arguments":{{"ids":["{}"],"location":"/new"}}}}"#,
                        "b".repeat(40)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            registry
                .read()
                .await
                .get(&"b".repeat(40))
                .unwrap()
                .save_path,
            "/new"
        );
    }

    #[tokio::test]
    async fn transmission_queue_stalled_settings_roundtrip_without_engine() {
        let app =
            build_transmission_router(AppState::new(Arc::new(RwLock::new(SessionRegistry::new()))));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"session-set","arguments":{"queue-stalled-enabled":true,"queue-stalled-minutes":7}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(r#"{"method":"session-get"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["arguments"]["queue-stalled-enabled"], true);
        assert_eq!(body["arguments"]["queue-stalled-minutes"], 7);
    }

    #[tokio::test]
    async fn transmission_snake_case_rpc_roundtrips_v41_shape() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("d".repeat(40), "delta".into(), "/old".into());
            entry.total_length = 100;
            entry.amount_left = 40;
            reg.add(entry).unwrap();
        }
        let app = build_transmission_router(AppState::new(Arc::clone(&registry)));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"session_set","arguments":{"queue_stalled_enabled":true,"queue_stalled_minutes":9}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"torrent_get","arguments":{"fields":["hash_string","percent_done","left_until_done","download_dir"]}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["result"], "success");
        assert_eq!(
            body["arguments"]["torrents"][0]["hash_string"],
            "d".repeat(40)
        );
        assert_eq!(body["arguments"]["torrents"][0]["percent_done"], 0.6);
        assert_eq!(body["arguments"]["torrents"][0]["left_until_done"], 40);
        assert_eq!(body["arguments"]["torrents"][0]["download_dir"], "/old");
    }

    #[tokio::test]
    async fn transmission_torrent_set_updates_labels_and_download_dir() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let entry = TorrentEntry::new("c".repeat(40), "gamma".into(), "/old".into());
            reg.add(entry).unwrap();
        }
        let app = build_transmission_router(AppState::new(Arc::clone(&registry)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(format!(
                        r#"{{"method":"torrent-set","arguments":{{"ids":["{}"],"labels":["tv","hd"],"download-dir":"/new"}}}}"#,
                        "c".repeat(40)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reg = registry.read().await;
        let entry = reg.get(&"c".repeat(40)).unwrap();
        assert_eq!(entry.tags, vec!["tv".to_owned(), "hd".to_owned()]);
        assert_eq!(entry.save_path, "/new");
    }

    #[tokio::test]
    async fn transmission_common_mutators_are_accepted() {
        let app =
            build_transmission_router(AppState::new(Arc::new(RwLock::new(SessionRegistry::new()))));
        for (method, args) in transmission_method_matrix() {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/transmission/rpc")
                        .header("content-type", "application/json")
                        .header("x-transmission-session-id", SESSION_ID)
                        .body(Body::from(format!(
                            r#"{{"method":"{method}","arguments":{args}}}"#
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_ne!(body["result"], "method name not recognized", "{method}");
        }
    }

    fn transmission_method_matrix() -> Vec<(&'static str, &'static str)> {
        vec![
            ("session-get", r#"{}"#),
            ("session-stats", r#"{}"#),
            ("session-close", r#"{}"#),
            ("session-set", r#"{"queue-stalled-enabled":true}"#),
            ("session-access-control", r#"{}"#),
            ("group-get", r#"{}"#),
            ("group-set", r#"{"name":"default"}"#),
            ("torrent-set", r#"{"ids":[]}"#),
            ("torrent-set-tracker-list", r#"{"ids":[],"trackerList":[]}"#),
            (
                "torrent-set-file-priorities",
                r#"{"ids":[],"priority-normal":[]}"#,
            ),
            ("torrent-set-file-wanted", r#"{"ids":[],"files-wanted":[]}"#),
            (
                "torrent-set-file-unwanted",
                r#"{"ids":[],"files-unwanted":[]}"#,
            ),
            ("queue-move-top", r#"{"ids":[]}"#),
            ("queue-move-up", r#"{"ids":[]}"#),
            ("queue-move-down", r#"{"ids":[]}"#),
            ("queue-move-bottom", r#"{"ids":[]}"#),
            ("queue-stalled-enable", r#"{}"#),
            ("queue-stalled-disable", r#"{}"#),
            ("port-test", r#"{}"#),
            ("blocklist-update", r#"{}"#),
            ("free-space", r#"{"path":"/tmp"}"#),
            ("torrent-get", r#"{"fields":["id","name"]}"#),
            (
                "torrent-add",
                r#"{"filename":"magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
            ),
            ("torrent-set-location", r#"{"ids":[],"location":"/tmp"}"#),
            (
                "torrent-rename-path",
                r#"{"ids":[],"path":"old","name":"new"}"#,
            ),
            ("torrent-start", r#"{"ids":[]}"#),
            ("torrent-start-now", r#"{"ids":[]}"#),
            ("torrent-stop", r#"{"ids":[]}"#),
            ("torrent-verify", r#"{"ids":[]}"#),
            ("torrent-reannounce", r#"{"ids":[]}"#),
            ("torrent-remove", r#"{"ids":[],"delete-local-data":false}"#),
        ]
    }

    #[test]
    fn renamed_file_path_preserves_parent_directory() {
        assert_eq!(renamed_file_path("dir/old.bin", "new.bin"), "dir/new.bin");
        assert_eq!(renamed_file_path("old.bin", "new.bin"), "new.bin");
    }

    #[test]
    fn transmission_tracker_list_arg_accepts_common_shapes() {
        let args = json!({
            "trackerList": [
                [" udp://one/announce ", "udp://one/announce"],
                "https://two/announce",
                { "announce": "http://three/announce" }
            ]
        });
        assert_eq!(
            transmission_tracker_list_arg(&args),
            vec![
                "udp://one/announce".to_owned(),
                "https://two/announce".to_owned(),
                "http://three/announce".to_owned(),
            ]
        );
    }

    #[test]
    fn transmission_session_limit_args_use_kib_wire_units() {
        let args = json!({
            "speed-limit-down": "128",
            "speed-limit-up-enabled": 0,
            "alt-speed-enabled": "on"
        });
        assert_eq!(transmission_i64_arg(&args, "speed-limit-down"), Some(128));
        assert_eq!(
            transmission_kib_to_bytes(transmission_i64_arg(&args, "speed-limit-down").unwrap()),
            131_072
        );
        assert_eq!(bytes_to_transmission_kib(131_072), 128);
        assert_eq!(
            transmission_bool_arg(&args, "speed-limit-up-enabled"),
            Some(false)
        );
        assert_eq!(
            transmission_bool_arg(&args, "alt-speed-enabled"),
            Some(true)
        );
    }
}
