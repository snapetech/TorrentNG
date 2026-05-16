use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose, Engine as _};
use rt_engine::{EngineHandle, EngineTorrentMetadata};
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_session::SessionRegistry;
use serde_json::{json, Value};
use tokio::sync::RwLock;

const SESSION_ID: &str = "rtorrentNG";

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
}

impl AppState {
    pub fn new(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        Self {
            registry,
            engine: None,
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        Self {
            registry,
            engine: Some(engine),
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
    let args = body.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let tag = body.get("tag").cloned();
    let result = match method {
        "session-get" => Ok(session_get(&state).await),
        "session-stats" => Ok(session_stats(&state).await),
        "session-close" | "session-set" => Ok(json!({})),
        "session-access-control" => Ok(json!({
            "blocklist-enabled": false,
            "rpc-authentication-required": false,
            "rpc-whitelist-enabled": false,
        })),
        "group-get" => Ok(json!({ "groups": [] })),
        "group-set" => Ok(json!({})),
        "torrent-set" => torrent_set(&state, &args).await,
        "torrent-set-tracker-list" => Ok(json!({})),
        "torrent-set-file-priorities" => torrent_set_file_priorities(&state, &args).await,
        "torrent-set-file-wanted" => torrent_set_file_wanted(&state, &args, true).await,
        "torrent-set-file-unwanted" => torrent_set_file_wanted(&state, &args, false).await,
        "queue-move-top" | "queue-move-up" | "queue-move-down" | "queue-move-bottom" => {
            Ok(json!({}))
        }
        "queue-stalled-enable" | "queue-stalled-disable" => Ok(json!({})),
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
        "torrent-rename-path" => Ok(
            json!({ "path": args.get("path").cloned().unwrap_or(Value::Null), "name": args.get("name").cloned().unwrap_or(Value::Null) }),
        ),
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
        Ok(arguments) => ("success".to_owned(), arguments),
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

async fn session_get(state: &AppState) -> Value {
    json!({
        "version": "rtorrentNG",
        "rpc-version": 17,
        "rpc-version-minimum": 1,
        "download-dir": default_download_dir(state).await,
        "config-dir": "/config",
        "start-added-torrents": true,
        "trash-original-torrent-files": false,
        "speed-limit-down-enabled": false,
        "speed-limit-up-enabled": false,
        "speed-limit-down": 0,
        "speed-limit-up": 0,
        "alt-speed-enabled": false,
        "alt-speed-down": 0,
        "alt-speed-up": 0,
        "download-queue-enabled": false,
        "download-queue-size": 0,
        "seed-queue-enabled": false,
        "seed-queue-size": 0,
        "queue-stalled-enabled": false,
        "queue-stalled-minutes": 30,
        "peer-limit-global": 0,
        "peer-limit-per-torrent": 0,
        "script-torrent-added-enabled": false,
        "script-torrent-done-enabled": false,
        "script-torrent-done-seeding-enabled": false,
        "blocklist-enabled": false,
        "blocklist-size": 0,
        "utp-enabled": true,
        "lpd-enabled": false,
        "dht-enabled": true,
        "pex-enabled": true,
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
    if let Some(engine) = &state.engine {
        for entry in &entries {
            if let Ok(meta) = engine.torrent_metadata(entry.info_hash.clone()).await {
                metadata.insert(entry.info_hash.clone(), meta);
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
                let value = match field.as_str() {
                    "id" => json!(idx + 1),
                    "hashString" | "hash-string" => json!(entry.info_hash),
                    "name" => json!(entry.name),
                    "totalSize" | "total-size" => json!(entry.total_length),
                    "leftUntilDone" | "left-until-done" => json!(entry.amount_left),
                    "percentDone" | "percent-done" => {
                        json!(percent_done(entry.total_length, entry.amount_left))
                    }
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
                    "isPrivate" | "is-private" => {
                        json!(meta.map(|m| m.is_private).unwrap_or(false))
                    }
                    "isFinished" | "is-finished" => json!(entry.completed_at.is_some()),
                    "isStalled" | "is-stalled" => json!(false),
                    "queuePosition" | "queue-position" => json!(idx),
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
                    "peers" => json!([]),
                    "peersConnected" | "peers-connected" => json!(0),
                    "peersGettingFromUs" | "peers-getting-from-us" => json!(0),
                    "peersSendingToUs" | "peers-sending-to-us" => json!(0),
                    "trackers" => json!(transmission_trackers(meta)),
                    "trackerStats" | "tracker-stats" => json!(transmission_tracker_stats(meta)),
                    "files" => json!(transmission_files(entry, meta)),
                    "fileStats" | "file-stats" => json!(transmission_file_stats(entry, meta)),
                    "priorities" => json!(transmission_file_priorities(meta)),
                    "wanted" => json!(transmission_file_wanted(meta)),
                    "comment" => json!(""),
                    "creator" => json!(""),
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
                    "webseeds" => json!([]),
                    "webseedsSendingToUs" | "webseeds-sending-to-us" => json!(0),
                    "bandwidthPriority" | "bandwidth-priority" => json!(0),
                    "honorsSessionLimits" | "honors-session-limits" => json!(true),
                    "magnetLink" | "magnet-link" => {
                        json!(format!("magnet:?xt=urn:btih:{}", entry.info_hash))
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
                    _ => Value::Null,
                };
                obj.insert(field.clone(), value);
            }
            Value::Object(obj)
        })
        .collect::<Vec<_>>();
    json!({ "torrents": torrents })
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
        for method in [
            "torrent-set-tracker-list",
            "torrent-set-file-priorities",
            "torrent-set-file-wanted",
            "torrent-set-file-unwanted",
            "queue-stalled-enable",
            "queue-stalled-disable",
            "session-set",
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/transmission/rpc")
                        .header("content-type", "application/json")
                        .header("x-transmission-session-id", SESSION_ID)
                        .body(Body::from(format!(
                            r#"{{"method":"{method}","arguments":{{}}}}"#
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["result"], "success", "{method}");
        }
    }
}
