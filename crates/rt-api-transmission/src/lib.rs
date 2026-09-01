#![recursion_limit = "256"]

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose, Engine as _};
use rt_engine::{
    EngineGlobalLimits, EngineHandle, EngineJob, EnginePeerSnapshot, EnginePieceState,
    EngineTorrentLimits, EngineTorrentMetadata, EngineTrackerSnapshot, EngineWebseedSnapshot,
    QueueMove,
};
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_metrics::{MemoryClass, MemoryLease};
use rt_session::SessionRegistry;
use serde_json::{json, Value};
use tokio::sync::RwLock;

const SESSION_ID: &str = "TorrentNG";
const MAX_TRANSMISSION_BATCH_REQUESTS: usize = 128;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub api_tokens: Arc<Vec<String>>,
    pub session: Arc<RwLock<TransmissionSessionSettings>>,
    pub torrent_limits: Arc<RwLock<HashMap<String, EngineTorrentLimits>>>,
    pub torrent_groups: Arc<RwLock<HashMap<String, String>>>,
    pub torrent_sequential_from_piece: Arc<RwLock<HashMap<String, i64>>>,
    pub groups: Arc<RwLock<BTreeMap<String, TransmissionGroup>>>,
    pub notification_subscriptions: Arc<RwLock<BTreeSet<String>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransmissionSessionSettings {
    pub queue_stalled_enabled: bool,
    pub queue_stalled_minutes: i64,
    pub download_dir: Option<String>,
    pub incomplete_dir: String,
    pub incomplete_dir_enabled: bool,
    pub rename_partial_files: bool,
    pub start_added_torrents: bool,
    pub trash_original_torrent_files: bool,
    pub alt_speed_time_enabled: bool,
    pub alt_speed_time_begin: i64,
    pub alt_speed_time_end: i64,
    pub alt_speed_time_day: i64,
    pub download_queue_enabled: bool,
    pub download_queue_size: i64,
    pub seed_queue_enabled: bool,
    pub seed_queue_size: i64,
    pub peer_limit_global: i64,
    pub peer_limit_per_torrent: i64,
    pub peer_port: i64,
    pub port_forwarding_enabled: bool,
    pub rpc_authentication_required: bool,
    pub rpc_whitelist_enabled: bool,
    pub rpc_username: String,
    pub rpc_bind_address: String,
    pub dht_enabled: bool,
    pub pex_enabled: bool,
    pub lpd_enabled: bool,
    pub utp_enabled: bool,
    pub preferred_transport: String,
    pub blocklist_enabled: bool,
    pub blocklist_size: i64,
    pub blocklist_url: String,
    pub script_torrent_added_enabled: bool,
    pub script_torrent_added_filename: String,
    pub script_torrent_done_enabled: bool,
    pub script_torrent_done_filename: String,
    pub script_torrent_done_seeding_enabled: bool,
    pub script_torrent_done_seeding_filename: String,
    pub seed_ratio_limit: f64,
    pub seed_ratio_limited: bool,
    pub idle_seeding_limit: i64,
    pub idle_seeding_limit_enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransmissionGroup {
    pub name: String,
    pub honors_session_limits: bool,
    pub speed_limit_down_enabled: bool,
    pub speed_limit_down: i64,
    pub speed_limit_up_enabled: bool,
    pub speed_limit_up: i64,
}

impl TransmissionGroup {
    fn new(name: String) -> Self {
        Self {
            name,
            honors_session_limits: true,
            speed_limit_down_enabled: false,
            speed_limit_down: 0,
            speed_limit_up_enabled: false,
            speed_limit_up: 0,
        }
    }
}

impl Default for TransmissionSessionSettings {
    fn default() -> Self {
        Self {
            queue_stalled_enabled: false,
            queue_stalled_minutes: 30,
            download_dir: None,
            incomplete_dir: String::new(),
            incomplete_dir_enabled: false,
            rename_partial_files: false,
            start_added_torrents: true,
            trash_original_torrent_files: false,
            alt_speed_time_enabled: false,
            alt_speed_time_begin: 540,
            alt_speed_time_end: 1020,
            alt_speed_time_day: 127,
            download_queue_enabled: false,
            download_queue_size: 0,
            seed_queue_enabled: false,
            seed_queue_size: 0,
            peer_limit_global: 0,
            peer_limit_per_torrent: 0,
            peer_port: 0,
            port_forwarding_enabled: false,
            rpc_authentication_required: false,
            rpc_whitelist_enabled: false,
            rpc_username: String::new(),
            rpc_bind_address: "0.0.0.0".to_owned(),
            dht_enabled: true,
            pex_enabled: true,
            lpd_enabled: false,
            utp_enabled: true,
            preferred_transport: "tcp".to_owned(),
            blocklist_enabled: false,
            blocklist_size: 0,
            blocklist_url: String::new(),
            script_torrent_added_enabled: false,
            script_torrent_added_filename: String::new(),
            script_torrent_done_enabled: false,
            script_torrent_done_filename: String::new(),
            script_torrent_done_seeding_enabled: false,
            script_torrent_done_seeding_filename: String::new(),
            seed_ratio_limit: -1.0,
            seed_ratio_limited: false,
            idle_seeding_limit: 0,
            idle_seeding_limit_enabled: false,
        }
    }
}

impl AppState {
    pub fn new(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        Self {
            registry,
            engine: None,
            api_tokens: Arc::new(Vec::new()),
            session: Arc::new(RwLock::new(TransmissionSessionSettings::default())),
            torrent_limits: Arc::new(RwLock::new(HashMap::new())),
            torrent_groups: Arc::new(RwLock::new(HashMap::new())),
            torrent_sequential_from_piece: Arc::new(RwLock::new(HashMap::new())),
            groups: Arc::new(RwLock::new(BTreeMap::new())),
            notification_subscriptions: Arc::new(RwLock::new(BTreeSet::new())),
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        Self::with_engine_and_tokens(registry, engine, Vec::new())
    }

    pub fn with_engine_and_tokens(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: EngineHandle,
        api_tokens: Vec<String>,
    ) -> Self {
        Self {
            registry,
            engine: Some(engine),
            api_tokens: Arc::new(api_tokens),
            session: Arc::new(RwLock::new(TransmissionSessionSettings::default())),
            torrent_limits: Arc::new(RwLock::new(HashMap::new())),
            torrent_groups: Arc::new(RwLock::new(HashMap::new())),
            torrent_sequential_from_piece: Arc::new(RwLock::new(HashMap::new())),
            groups: Arc::new(RwLock::new(BTreeMap::new())),
            notification_subscriptions: Arc::new(RwLock::new(BTreeSet::new())),
        }
    }
}

async fn reserve_transmission_api_snapshot(
    state: &AppState,
    bytes: u64,
) -> Result<Option<MemoryLease>, String> {
    let Some(engine) = &state.engine else {
        return Ok(None);
    };
    engine.reserve_memory(MemoryClass::ApiSnapshot, bytes).await
}

fn estimate_transmission_torrent_get_snapshot_bytes(
    torrent_count: usize,
    field_count: usize,
) -> u64 {
    let fields = field_count.max(1) as u64;
    16 * 1024 + (torrent_count as u64).saturating_mul(1024 + fields.saturating_mul(384))
}

pub fn build_transmission_router(state: AppState) -> Router {
    Router::new()
        .route("/transmission/rpc", post(rpc))
        .route("/api/transmission/rpc", post(rpc))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            transmission_auth_guard,
        ))
        .with_state(state)
}

async fn transmission_auth_guard(
    State(state): State<AppState>,
    req: axum::http::Request<Body>,
    next: Next,
) -> Response {
    if state.api_tokens.is_empty()
        || presented_token(req.headers())
            .is_some_and(|token| state.api_tokens.iter().any(|allowed| allowed == &token))
    {
        return next.run(req).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "result": "authentication required"
        })),
    )
        .into_response()
}

fn presented_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get("cookie")
                .and_then(|value| value.to_str().ok())
                .and_then(|cookie| {
                    cookie.split(';').find_map(|part| {
                        part.trim()
                            .strip_prefix("tng_session=")
                            .or_else(|| part.trim().strip_prefix("SID="))
                            .map(str::to_owned)
                    })
                })
        })
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

    if let Value::Array(requests) = body {
        if requests.len() > MAX_TRANSMISSION_BATCH_REQUESTS {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "result": "request batch exceeds the maximum supported size"
                })),
            )
                .into_response();
        }
        let mut responses = Vec::new();
        for request in requests {
            responses.push(transmission_rpc_payload(&state, request).await);
        }
        return Json(Value::Array(responses)).into_response();
    }

    Json(transmission_rpc_payload(&state, body).await).into_response()
}

async fn transmission_rpc_payload(state: &AppState, body: Value) -> Value {
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let json_rpc = body
        .get("jsonrpc")
        .and_then(Value::as_str)
        .is_some_and(|version| version == "2.0");
    let snake_case_rpc = method.contains('_');
    let method_key = method.replace('_', "-");
    let args = normalize_transmission_request_keys(
        body.get(if json_rpc { "params" } else { "arguments" })
            .or_else(|| body.get("arguments"))
            .or_else(|| body.get("params"))
            .cloned()
            .unwrap_or_else(|| json!({})),
    );
    let tag = body.get("tag").cloned();
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let result = match method_key.as_str() {
        "session-get" => Ok(session_get(state, &args).await),
        "session-stats" => Ok(session_stats(state).await),
        "session-close" => Ok(json!({})),
        "session-set" => session_set(state, &args).await,
        "session-subscribe" => session_subscribe(state, &args).await,
        "session-unsubscribe" => session_unsubscribe(state, &args).await,
        "session-access-control" => {
            let session = state.session.read().await;
            Ok(json!({
                "blocklist-enabled": session.blocklist_enabled,
                "rpc-authentication-required": session.rpc_authentication_required,
                "rpc-whitelist-enabled": session.rpc_whitelist_enabled,
                "rpc-username": session.rpc_username,
                "rpc-bind-address": session.rpc_bind_address,
            }))
        }
        "group-get" => group_get(state, &args).await,
        "group-set" => group_set(state, &args).await,
        "torrent-set" => torrent_set(state, &args).await,
        "torrent-set-tracker-list" => torrent_set_tracker_list(state, &args).await,
        "torrent-set-file-priorities" => torrent_set_file_priorities(state, &args).await,
        "torrent-set-file-wanted" => torrent_set_file_wanted(state, &args, true).await,
        "torrent-set-file-unwanted" => torrent_set_file_wanted(state, &args, false).await,
        "queue-move-top" => transmission_queue_move(state, &args, QueueMove::Top).await,
        "queue-move-up" => transmission_queue_move(state, &args, QueueMove::Up).await,
        "queue-move-down" => transmission_queue_move(state, &args, QueueMove::Down).await,
        "queue-move-bottom" => transmission_queue_move(state, &args, QueueMove::Bottom).await,
        "queue-stalled-enable" => queue_stalled_set(state, true).await,
        "queue-stalled-disable" => queue_stalled_set(state, false).await,
        "port-test" => Ok(json!({"port-is-open": true})),
        "blocklist-update" => Ok(json!({
            "blocklist-size": state.session.read().await.blocklist_size,
        })),
        "free-space" => Ok(
            json!({"path": args.get("path").and_then(Value::as_str).unwrap_or(""), "size-bytes": 0}),
        ),
        "torrent-get" => torrent_get(state, &args).await,
        "torrent-add" => torrent_add(state, &args).await,
        "torrent-set-location" => {
            let Some(location) = args.get("location").and_then(Value::as_str) else {
                return transmission_response(
                    tag.clone(),
                    id,
                    json_rpc,
                    Err("missing location".to_owned()),
                );
            };
            for hash in ids(state, &args).await {
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
        "torrent-rename-path" => torrent_rename_path(state, &args).await,
        "torrent-start" | "torrent-start-now" => {
            for hash in ids(state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine.resume_torrent(hash).await;
                }
            }
            Ok(json!({}))
        }
        "torrent-stop" => {
            for hash in ids(state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine.pause_torrent(hash).await;
                }
            }
            Ok(json!({}))
        }
        "torrent-verify" => {
            for hash in ids(state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine.recheck_torrent(hash).await;
                }
            }
            Ok(json!({}))
        }
        "torrent-reannounce" => {
            for hash in ids(state, &args).await {
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
            for hash in ids(state, &args).await {
                if let Some(engine) = &state.engine {
                    let _ = engine.remove_torrent(hash, delete_files).await;
                }
            }
            Ok(json!({}))
        }
        _ => Err("method name not recognized".to_owned()),
    };
    let payload = match result {
        Ok(arguments) => {
            let arguments = if snake_case_rpc {
                transmission_response_to_snake_case(arguments)
            } else {
                arguments
            };
            Ok(arguments)
        }
        Err(result) => Err(result),
    };
    transmission_response(tag, id, json_rpc, payload)
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
    let group = args
        .get("group")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_owned);
    let sequential_from_piece = transmission_i64_arg(args, "sequential-download-from-piece")
        .or_else(|| transmission_i64_arg(args, "sequentialDownloadFromPiece"))
        .map(|piece| piece.max(0));
    let limit_updates = transmission_torrent_limit_updates(args);
    if labels.is_none()
        && location.is_none()
        && group.is_none()
        && sequential_from_piece.is_none()
        && limit_updates.is_none()
    {
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
        if let Some(group) = &group {
            state
                .groups
                .write()
                .await
                .entry(group.clone())
                .or_insert_with(|| TransmissionGroup::new(group.clone()));
            state
                .torrent_groups
                .write()
                .await
                .insert(hash.clone(), group.clone());
            let group_state = state.groups.read().await.get(group).cloned();
            if let Some(group_state) = group_state {
                apply_transmission_group_limits(state, &hash, &group_state).await?;
            }
        }
        if let Some(piece) = sequential_from_piece {
            state
                .torrent_sequential_from_piece
                .write()
                .await
                .insert(hash.clone(), piece);
            if let Some(engine) = &state.engine {
                let mut limits = transmission_torrent_limits(state, &hash)
                    .await
                    .unwrap_or_default();
                limits.sequential_download_from_piece = Some(piece);
                state
                    .torrent_limits
                    .write()
                    .await
                    .insert(hash.clone(), limits.clone());
                engine.update_torrent_limits(hash.clone(), limits).await?;
            }
        }
        if let Some(updates) = &limit_updates {
            let mut limits = transmission_torrent_limits(state, &hash)
                .await
                .unwrap_or_default();
            updates.apply(&mut limits);
            state
                .torrent_limits
                .write()
                .await
                .insert(hash.clone(), limits.clone());
            if let Some(engine) = &state.engine {
                engine.update_torrent_limits(hash, limits).await?;
            }
        }
    }
    Ok(json!({}))
}

async fn group_get(state: &AppState, args: &Value) -> Result<Value, String> {
    let requested = args
        .get("group")
        .or_else(|| args.get("name"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let groups = state.groups.read().await;
    let groups = groups
        .values()
        .filter(|group| {
            requested
                .as_ref()
                .map(|name| &group.name == name)
                .unwrap_or(true)
        })
        .map(transmission_group_json)
        .collect::<Vec<_>>();
    Ok(json!({ "groups": groups }))
}

async fn group_set(state: &AppState, args: &Value) -> Result<Value, String> {
    let name = args
        .get("group")
        .or_else(|| args.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "missing group".to_owned())?;
    let group = {
        let mut groups = state.groups.write().await;
        let group = groups
            .entry(name.to_owned())
            .or_insert_with(|| TransmissionGroup::new(name.to_owned()));
        if let Some(value) = transmission_bool_arg(args, "honors-session-limits") {
            group.honors_session_limits = value;
        }
        if let Some(value) = transmission_bool_arg(args, "speed-limit-down-enabled") {
            group.speed_limit_down_enabled = value;
        }
        if let Some(value) = transmission_i64_arg(args, "speed-limit-down") {
            group.speed_limit_down = value.max(0);
        }
        if let Some(value) = transmission_bool_arg(args, "speed-limit-up-enabled") {
            group.speed_limit_up_enabled = value;
        }
        if let Some(value) = transmission_i64_arg(args, "speed-limit-up") {
            group.speed_limit_up = value.max(0);
        }
        group.clone()
    };
    let assigned = state
        .torrent_groups
        .read()
        .await
        .iter()
        .filter_map(|(hash, assigned)| (assigned == name).then_some(hash.clone()))
        .collect::<Vec<_>>();
    for hash in assigned {
        apply_transmission_group_limits(state, &hash, &group).await?;
    }
    Ok(json!({}))
}

async fn apply_transmission_group_limits(
    state: &AppState,
    hash: &str,
    group: &TransmissionGroup,
) -> Result<(), String> {
    if !group.speed_limit_down_enabled && !group.speed_limit_up_enabled {
        return Ok(());
    }
    let Some(engine) = &state.engine else {
        return Ok(());
    };
    let mut limits = transmission_torrent_limits(state, hash)
        .await
        .unwrap_or_default();
    if group.speed_limit_down_enabled {
        limits.download_limit = Some(transmission_kib_to_bytes(group.speed_limit_down));
    }
    if group.speed_limit_up_enabled {
        limits.upload_limit = Some(transmission_kib_to_bytes(group.speed_limit_up));
    }
    state
        .torrent_limits
        .write()
        .await
        .insert(hash.to_owned(), limits.clone());
    engine.update_torrent_limits(hash.to_owned(), limits).await
}

async fn session_subscribe(state: &AppState, args: &Value) -> Result<Value, String> {
    let requested = transmission_subscription_fields(args);
    let mut subscriptions = state.notification_subscriptions.write().await;
    for field in requested {
        subscriptions.insert(field);
    }
    Ok(json!({
        "subscriptions": subscriptions.iter().cloned().collect::<Vec<_>>(),
    }))
}

async fn session_unsubscribe(state: &AppState, args: &Value) -> Result<Value, String> {
    let requested = transmission_subscription_fields(args);
    let mut subscriptions = state.notification_subscriptions.write().await;
    if requested.is_empty() {
        subscriptions.clear();
    } else {
        for field in requested {
            subscriptions.remove(&field);
        }
    }
    Ok(json!({
        "subscriptions": subscriptions.iter().cloned().collect::<Vec<_>>(),
    }))
}

fn transmission_subscription_fields(args: &Value) -> Vec<String> {
    args.get("fields")
        .or_else(|| args.get("events"))
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|field| !field.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn transmission_group_json(group: &TransmissionGroup) -> Value {
    json!({
        "name": group.name,
        "honors-session-limits": group.honors_session_limits,
        "speed-limit-down-enabled": group.speed_limit_down_enabled,
        "speed-limit-down": group.speed_limit_down,
        "speed-limit-up-enabled": group.speed_limit_up_enabled,
        "speed-limit-up": group.speed_limit_up,
    })
}

#[derive(Debug, Default)]
struct TransmissionTorrentLimitUpdates {
    download_limit: Option<Option<i64>>,
    upload_limit: Option<Option<i64>>,
    max_connections: Option<Option<i64>>,
    seed_ratio_limit: Option<Option<f64>>,
    seed_idle_limit: Option<Option<i64>>,
    bandwidth_priority: Option<i64>,
    sequential_download: Option<bool>,
}

impl TransmissionTorrentLimitUpdates {
    fn apply(&self, limits: &mut EngineTorrentLimits) {
        if let Some(value) = self.download_limit {
            limits.download_limit = value;
        }
        if let Some(value) = self.upload_limit {
            limits.upload_limit = value;
        }
        if let Some(value) = self.max_connections {
            limits.max_connections = value;
        }
        if let Some(value) = self.seed_ratio_limit {
            limits.seed_ratio_limit = value;
        }
        if let Some(value) = self.seed_idle_limit {
            limits.seed_idle_limit = value;
        }
        if let Some(value) = self.sequential_download {
            limits.sequential_download = value;
        }
    }

    fn has_updates(&self) -> bool {
        self.download_limit.is_some()
            || self.upload_limit.is_some()
            || self.max_connections.is_some()
            || self.seed_ratio_limit.is_some()
            || self.seed_idle_limit.is_some()
            || self.bandwidth_priority.is_some()
            || self.sequential_download.is_some()
    }
}

fn transmission_torrent_limit_updates(args: &Value) -> Option<TransmissionTorrentLimitUpdates> {
    let mut updates = TransmissionTorrentLimitUpdates::default();
    if let Some(value) = transmission_i64_arg(args, "download-limit") {
        updates.download_limit = Some(Some(transmission_kib_to_bytes(value)));
    }
    if matches!(transmission_bool_arg(args, "download-limited"), Some(false)) {
        updates.download_limit = Some(None);
    }
    if let Some(value) = transmission_i64_arg(args, "upload-limit") {
        updates.upload_limit = Some(Some(transmission_kib_to_bytes(value)));
    }
    if matches!(transmission_bool_arg(args, "upload-limited"), Some(false)) {
        updates.upload_limit = Some(None);
    }
    if let Some(value) = transmission_i64_arg(args, "peer-limit") {
        updates.max_connections = Some(Some(value.max(0)));
    }
    if let Some(value) = transmission_i64_arg(args, "max-connected-peers") {
        updates.max_connections = Some(Some(value.max(0)));
    }
    if let Some(mode) = transmission_i64_arg(args, "seed-ratio-mode") {
        if mode == 0 {
            updates.seed_ratio_limit = Some(None);
        }
    }
    if let Some(value) = transmission_f64_arg(args, "seed-ratio-limit") {
        updates.seed_ratio_limit = Some(Some(value));
    }
    if let Some(mode) = transmission_i64_arg(args, "seed-idle-mode") {
        if mode == 0 {
            updates.seed_idle_limit = Some(None);
        }
    }
    if let Some(value) = transmission_i64_arg(args, "seed-idle-limit") {
        updates.seed_idle_limit = Some(Some(value.max(0)));
    }
    if let Some(value) = transmission_i64_arg(args, "bandwidth-priority") {
        updates.bandwidth_priority = Some(value.clamp(-1, 1));
    }
    if let Some(value) = transmission_bool_arg(args, "sequential-download") {
        updates.sequential_download = Some(value);
    }
    if updates.has_updates() {
        Some(updates)
    } else {
        None
    }
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

fn transmission_response(
    tag: Option<Value>,
    id: Value,
    json_rpc: bool,
    payload: Result<Value, String>,
) -> Value {
    if json_rpc {
        return match payload {
            Ok(result) => json!({
                "jsonrpc": "2.0",
                "result": result,
                "id": id,
            }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": transmission_json_rpc_error_code(&message),
                    "message": message,
                    "data": {
                        "error_string": message,
                    },
                },
                "id": id,
            }),
        };
    }

    let mut response = match payload {
        Ok(arguments) => json!({"result": "success", "arguments": arguments}),
        Err(result) => json!({"result": result, "arguments": {}}),
    };
    if let Some(tag) = tag {
        response["tag"] = tag;
    }
    response
}

fn transmission_json_rpc_error_code(message: &str) -> i64 {
    match message {
        "method name not recognized" => -32601,
        _ => -32602,
    }
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

async fn session_get(state: &AppState, args: &Value) -> Value {
    let limits = transmission_global_limits(state).await;
    let default_dir = default_download_dir(state).await;
    let session = state.session.read().await.clone();
    let notification_subscriptions = state
        .notification_subscriptions
        .read()
        .await
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let value = json!({
        "version": "TorrentNG",
        "rpc-version": 17,
        "rpc-version-minimum": 1,
        "rpc-version-semver": "6.0.0",
        "session-id": SESSION_ID,
        "download-dir": session.download_dir.clone().unwrap_or(default_dir),
        "config-dir": "/config",
        "incomplete-dir": session.incomplete_dir,
        "incomplete-dir-enabled": session.incomplete_dir_enabled,
        "rename-partial-files": session.rename_partial_files,
        "start-added-torrents": session.start_added_torrents,
        "trash-original-torrent-files": session.trash_original_torrent_files,
        "speed-limit-down-enabled": limits.download_limit > 0,
        "speed-limit-up-enabled": limits.upload_limit > 0,
        "speed-limit-down": bytes_to_transmission_kib(limits.download_limit),
        "speed-limit-up": bytes_to_transmission_kib(limits.upload_limit),
        "alt-speed-enabled": limits.speed_limits_mode,
        "alt-speed-down": bytes_to_transmission_kib(limits.download_limit),
        "alt-speed-up": bytes_to_transmission_kib(limits.upload_limit),
        "alt-speed-time-enabled": session.alt_speed_time_enabled,
        "alt-speed-time-begin": session.alt_speed_time_begin,
        "alt-speed-time-end": session.alt_speed_time_end,
        "alt-speed-time-day": session.alt_speed_time_day,
        "download-queue-enabled": session.download_queue_enabled,
        "download-queue-size": session.download_queue_size,
        "seed-queue-enabled": session.seed_queue_enabled,
        "seed-queue-size": session.seed_queue_size,
        "queue-stalled-enabled": session.queue_stalled_enabled,
        "queue-stalled-minutes": session.queue_stalled_minutes,
        "peer-limit-global": session.peer_limit_global,
        "peer-limit-per-torrent": session.peer_limit_per_torrent,
        "rpc-authentication-required": session.rpc_authentication_required,
        "rpc-whitelist-enabled": session.rpc_whitelist_enabled,
        "rpc-username": session.rpc_username,
        "rpc-bind-address": session.rpc_bind_address,
        "script-torrent-added-enabled": session.script_torrent_added_enabled,
        "script-torrent-added-filename": session.script_torrent_added_filename,
        "script-torrent-done-enabled": session.script_torrent_done_enabled,
        "script-torrent-done-filename": session.script_torrent_done_filename,
        "script-torrent-done-seeding-enabled": session.script_torrent_done_seeding_enabled,
        "script-torrent-done-seeding-filename": session.script_torrent_done_seeding_filename,
        "blocklist-enabled": session.blocklist_enabled,
        "blocklist-size": session.blocklist_size,
        "blocklist-url": session.blocklist_url,
        "utp-enabled": session.utp_enabled,
        "lpd-enabled": session.lpd_enabled,
        "dht-enabled": session.dht_enabled,
        "pex-enabled": session.pex_enabled,
        "peer-port": session.peer_port,
        "port-forwarding-enabled": session.port_forwarding_enabled,
        "preferred-transport": session.preferred_transport,
        "seedRatioLimit": session.seed_ratio_limit,
        "seedRatioLimited": session.seed_ratio_limited,
        "idle-seeding-limit": session.idle_seeding_limit,
        "idle-seeding-limit-enabled": session.idle_seeding_limit_enabled,
        "notification-subscriptions": notification_subscriptions,
        "units": {
            "speed-units": ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"],
            "speed-bytes": 1000,
            "size-units": ["B", "KB", "MB", "GB", "TB"],
            "size-bytes": 1000,
            "memory-units": ["B", "KiB", "MiB", "GiB", "TiB"],
            "memory-bytes": 1024,
        },
    });
    transmission_project_fields(value, args)
}

fn transmission_project_fields(value: Value, args: &Value) -> Value {
    let Some(fields) = args.get("fields").and_then(Value::as_array) else {
        return value;
    };
    if fields.is_empty() {
        return value;
    }
    let Some(obj) = value.as_object() else {
        return value;
    };
    Value::Object(
        fields
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|field| {
                obj.get(field)
                    .or_else(|| obj.get(&field.replace('_', "-")))
                    .cloned()
                    .map(|value| (field.to_owned(), value))
            })
            .collect(),
    )
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

async fn torrent_get(state: &AppState, args: &Value) -> Result<Value, String> {
    let table_format = args
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|format| format.eq_ignore_ascii_case("table"));
    let recently_active = args
        .get("ids")
        .and_then(Value::as_str)
        .is_some_and(|ids| ids.eq_ignore_ascii_case("recently-active"));
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
    let _lease = if state.engine.is_some() {
        Some(
            reserve_transmission_api_snapshot(
                state,
                estimate_transmission_torrent_get_snapshot_bytes(entries.len(), fields.len()),
            )
            .await?
            .ok_or_else(|| "api snapshot memory budget exhausted".to_owned())?,
        )
    } else {
        None
    };
    let mut metadata = std::collections::HashMap::new();
    let mut queue_positions = std::collections::HashMap::new();
    let mut limits_by_hash = state.torrent_limits.read().await.clone();
    if let Some(engine) = &state.engine {
        for entry in &entries {
            if let Ok(meta) = engine.torrent_metadata(entry.info_hash.clone()).await {
                metadata.insert(entry.info_hash.clone(), meta);
            }
            if let Ok(position) = engine.queue_priority(entry.info_hash.clone()).await {
                queue_positions.insert(entry.info_hash.clone(), position);
            }
            if let Ok(limits) = engine.torrent_limits(entry.info_hash.clone()).await {
                limits_by_hash.insert(entry.info_hash.clone(), limits);
            }
        }
    }
    let mut peers = std::collections::HashMap::new();
    let mut tracker_snapshots = std::collections::HashMap::new();
    let mut recheck_jobs = std::collections::HashMap::new();
    let mut webseed_snapshots = std::collections::HashMap::new();
    if let Some(engine) = &state.engine {
        if fields
            .iter()
            .any(|field| transmission_field_is_recheck_progress(field))
        {
            if let Ok(jobs) = engine.list_jobs().await {
                for entry in &entries {
                    if let Some(progress) = transmission_recheck_progress(&jobs, &entry.info_hash) {
                        recheck_jobs.insert(entry.info_hash.clone(), progress);
                    }
                }
            }
        }
        for entry in &entries {
            if fields
                .iter()
                .any(|field| transmission_field_needs_peers(field))
            {
                if let Ok(snapshot) = engine.torrent_peers(entry.info_hash.clone()).await {
                    peers.insert(entry.info_hash.clone(), snapshot);
                }
            }
            if fields
                .iter()
                .any(|field| transmission_field_needs_trackers(field))
            {
                if let Ok(snapshot) = engine.torrent_trackers(entry.info_hash.clone()).await {
                    tracker_snapshots.insert(entry.info_hash.clone(), snapshot);
                }
            }
            if fields
                .iter()
                .any(|field| transmission_field_needs_webseeds(field))
            {
                if let Ok(snapshot) = engine.torrent_webseeds(entry.info_hash.clone()).await {
                    webseed_snapshots.insert(entry.info_hash.clone(), snapshot);
                }
            }
        }
    }
    let torrent_groups = state.torrent_groups.read().await.clone();
    let sequential_from_piece = state.torrent_sequential_from_piece.read().await.clone();
    let groups = state.groups.read().await.clone();
    let torrents = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let meta = metadata.get(&entry.info_hash);
            let limits = limits_by_hash.get(&entry.info_hash);
            let group_name = torrent_groups
                .get(&entry.info_hash)
                .map(String::as_str)
                .unwrap_or("default");
            let group = groups.get(group_name);
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
                    "rateDownload" | "rate-download" => {
                        json!(transmission_peer_download_rate(peers.get(&entry.info_hash)))
                    }
                    "rateUpload" | "rate-upload" => {
                        json!(transmission_peer_upload_rate(peers.get(&entry.info_hash)))
                    }
                    "downloadLimit" | "download-limit" => json!(limits
                        .and_then(|limits| limits.download_limit)
                        .map(bytes_to_transmission_kib)
                        .unwrap_or(0)),
                    "downloadLimited" | "download-limited" => {
                        json!(limits.and_then(|limits| limits.download_limit).is_some())
                    }
                    "uploadLimit" | "upload-limit" => json!(limits
                        .and_then(|limits| limits.upload_limit)
                        .map(bytes_to_transmission_kib)
                        .unwrap_or(0)),
                    "uploadLimited" | "upload-limited" => {
                        json!(limits.and_then(|limits| limits.upload_limit).is_some())
                    }
                    "status" => json!(transmission_status(entry.state.as_str())),
                    "downloadDir" | "download-dir" => json!(entry.save_path),
                    "labels" => json!(entry.tags),
                    "error" => json!(0),
                    "errorString" | "error-string" => {
                        json!(entry.error_message.clone().unwrap_or_default())
                    }
                    "eta" => json!(transmission_eta(
                        entry.amount_left,
                        transmission_peer_download_rate(peers.get(&entry.info_hash))
                    )),
                    "etaIdle" | "eta-idle" => json!(transmission_eta(
                        entry.amount_left,
                        transmission_peer_download_rate(peers.get(&entry.info_hash))
                    )),
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
                    "recheckProgress" | "recheck-progress" => {
                        json!(recheck_jobs.get(&entry.info_hash).copied().unwrap_or(0.0))
                    }
                    "seedRatioLimit" | "seed-ratio-limit" => json!(limits
                        .and_then(|limits| limits.seed_ratio_limit)
                        .unwrap_or(-1.0)),
                    "seedRatioMode" | "seed-ratio-mode" => {
                        json!(
                            if limits.and_then(|limits| limits.seed_ratio_limit).is_some() {
                                1
                            } else {
                                0
                            }
                        )
                    }
                    "seedIdleLimit" | "seed-idle-limit" => json!(limits
                        .and_then(|limits| limits.seed_idle_limit)
                        .unwrap_or(0)),
                    "seedIdleMode" | "seed-idle-mode" => {
                        json!(
                            if limits.and_then(|limits| limits.seed_idle_limit).is_some() {
                                1
                            } else {
                                0
                            }
                        )
                    }
                    "addedDate" | "added-date" => json!(entry.added_at),
                    "activityDate" | "activity-date" => json!(entry.added_at),
                    "doneDate" | "done-date" => json!(entry.completed_at.unwrap_or(0)),
                    "startDate" | "start-date" => json!(entry.added_at),
                    "dateCreated" | "date-created" => json!(meta
                        .and_then(|meta| meta.creation_date)
                        .unwrap_or(entry.added_at as i64)),
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
                    "trackers" => json!(transmission_trackers(
                        meta,
                        tracker_snapshots.get(&entry.info_hash)
                    )),
                    "trackerStats" | "tracker-stats" => json!(transmission_tracker_stats(
                        meta,
                        tracker_snapshots.get(&entry.info_hash)
                    )),
                    "files" => json!(transmission_files(entry, meta)),
                    "fileStats" | "file-stats" => json!(transmission_file_stats(entry, meta)),
                    "priorities" => json!(transmission_file_priorities(meta)),
                    "wanted" => json!(transmission_file_wanted(meta)),
                    "comment" => json!(meta.and_then(|meta| meta.comment.as_deref()).unwrap_or("")),
                    "creator" => json!(meta
                        .and_then(|meta| meta.created_by.as_deref())
                        .unwrap_or("")),
                    "primaryMimeType" | "primary-mime-type" => {
                        json!(transmission_primary_mime_type(entry, meta))
                    }
                    "pieceCount" | "piece-count" => json!(meta.map(|m| m.piece_count).unwrap_or(0)),
                    "pieceSize" | "piece-size" => json!(meta.map(|m| m.piece_length).unwrap_or(0)),
                    "pieces" => json!(transmission_pieces(meta)),
                    "haveUnchecked" | "have-unchecked" => {
                        json!(transmission_have_unchecked(entry, meta))
                    }
                    "haveValid" | "have-valid" => {
                        json!(transmission_have_valid(entry, meta))
                    }
                    "desiredAvailable" | "desired-available" => {
                        json!(transmission_desired_available(entry, meta))
                    }
                    "corruptEver" | "corrupt-ever" => json!(0),
                    "manualAnnounceTime" | "manual-announce-time" => json!(0),
                    "maxConnectedPeers" | "max-connected-peers" => json!(limits
                        .and_then(|limits| limits.max_connections)
                        .unwrap_or(0)),
                    "webseeds" => json!(meta.map(|m| m.webseeds.clone()).unwrap_or_default()),
                    "webseedsSendingToUs" | "webseeds-sending-to-us" => {
                        json!(transmission_webseeds_sending_to_us(
                            webseed_snapshots.get(&entry.info_hash)
                        ))
                    }
                    "webseedsEx" | "webseeds-ex" => json!(transmission_webseeds_ex(
                        meta,
                        webseed_snapshots.get(&entry.info_hash)
                    )),
                    "bandwidthPriority" | "bandwidth-priority" => json!(0),
                    "honorsSessionLimits" | "honors-session-limits" => json!(group
                        .map(|group| group.honors_session_limits)
                        .unwrap_or(true)),
                    "group" => json!(group_name),
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
                    "secondsDownloading" | "seconds-downloading" => {
                        json!(transmission_seconds_downloading(entry, unix_now_secs()))
                    }
                    "secondsSeeding" | "seconds-seeding" => {
                        json!(transmission_seconds_seeding(entry, unix_now_secs()))
                    }
                    "sequentialDownload" | "sequential-download" => {
                        json!(limits
                            .map(|limits| limits.sequential_download)
                            .unwrap_or(false))
                    }
                    "sequentialDownloadFromPiece" | "sequential-download-from-piece" => {
                        json!(limits
                            .and_then(|limits| limits.sequential_download_from_piece)
                            .or_else(|| sequential_from_piece.get(&entry.info_hash).copied())
                            .unwrap_or(0))
                    }
                    _ => Value::Null,
                };
                obj.insert(field.clone(), value);
            }
            Value::Object(obj)
        })
        .collect::<Vec<_>>();
    let mut response = serde_json::Map::new();
    if table_format {
        let rows = torrents
            .iter()
            .map(|torrent| {
                let obj = torrent.as_object();
                Value::Array(
                    fields
                        .iter()
                        .map(|field| {
                            obj.and_then(|obj| obj.get(field))
                                .cloned()
                                .unwrap_or(Value::Null)
                        })
                        .collect(),
                )
            })
            .collect::<Vec<_>>();
        response.insert("fields".to_owned(), json!(fields));
        response.insert("torrents".to_owned(), json!(rows));
    } else {
        response.insert("torrents".to_owned(), json!(torrents));
    }
    if recently_active {
        response.insert("removed".to_owned(), json!([]));
    }
    Ok(Value::Object(response))
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
            | "rateDownload"
            | "rate-download"
            | "rateUpload"
            | "rate-upload"
    )
}

fn transmission_field_needs_trackers(field: &str) -> bool {
    let field = field.replace('_', "-");
    matches!(
        field.as_str(),
        "trackers" | "trackerStats" | "tracker-stats"
    )
}

fn transmission_field_needs_webseeds(field: &str) -> bool {
    let field = field.replace('_', "-");
    matches!(
        field.as_str(),
        "webseedsSendingToUs" | "webseeds-sending-to-us" | "webseedsEx" | "webseeds-ex"
    )
}

fn transmission_field_is_recheck_progress(field: &str) -> bool {
    let field = field.replace('_', "-");
    matches!(field.as_str(), "recheckProgress" | "recheck-progress")
}

fn transmission_recheck_progress(jobs: &[EngineJob], info_hash: &str) -> Option<f64> {
    jobs.iter()
        .filter(|job| {
            job.kind == "recheck_torrent"
                && job.affected_torrents.iter().any(|hash| hash == info_hash)
                && !matches!(
                    job.state.as_str(),
                    "completed" | "failed" | "cancelled" | "canceled"
                )
        })
        .max_by_key(|job| job.updated_at)
        .map(|job| {
            if job.total <= 0 {
                0.0
            } else {
                (job.done as f64 / job.total as f64).clamp(0.0, 1.0)
            }
        })
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

fn transmission_peer_download_rate(peers: Option<&Vec<EnginePeerSnapshot>>) -> i64 {
    peers
        .map(|peers| {
            peers
                .iter()
                .fold(0_i64, |sum, peer| sum.saturating_add(peer.download_rate))
        })
        .unwrap_or(0)
}

fn transmission_peer_upload_rate(peers: Option<&Vec<EnginePeerSnapshot>>) -> i64 {
    peers
        .map(|peers| {
            peers
                .iter()
                .fold(0_i64, |sum, peer| sum.saturating_add(peer.upload_rate))
        })
        .unwrap_or(0)
}

fn transmission_eta(amount_left: u64, download_rate: i64) -> i64 {
    if amount_left == 0 {
        return 0;
    }
    if download_rate <= 0 {
        return -1;
    }
    ((amount_left as f64) / (download_rate as f64)).ceil() as i64
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn transmission_seconds_downloading(entry: &rt_session::TorrentEntry, now: u64) -> u64 {
    if entry.added_at == 0 {
        return 0;
    }
    let end = entry.completed_at.unwrap_or(now);
    end.saturating_sub(entry.added_at)
}

fn transmission_seconds_seeding(entry: &rt_session::TorrentEntry, now: u64) -> u64 {
    entry
        .completed_at
        .map(|completed| now.saturating_sub(completed))
        .unwrap_or(0)
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

fn transmission_primary_mime_type(
    entry: &rt_session::TorrentEntry,
    meta: Option<&EngineTorrentMetadata>,
) -> String {
    let path = meta
        .and_then(|meta| {
            meta.files
                .iter()
                .max_by_key(|file| file.length)
                .map(|file| file.path.as_str())
        })
        .unwrap_or(entry.name.as_str());
    mime_type_from_path(path).to_owned()
}

fn mime_type_from_path(path: &str) -> &'static str {
    let Some(ext) = path.rsplit('.').next().map(str::to_ascii_lowercase) else {
        return "";
    };
    if ext == path.to_ascii_lowercase() {
        return "";
    }
    match ext.as_str() {
        "avi" => "video/x-msvideo",
        "flac" => "audio/flac",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "m4a" => "audio/mp4",
        "m4v" | "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "ogv" => "video/ogg",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "srt" => "application/x-subrip",
        "txt" => "text/plain",
        "wav" => "audio/wav",
        "webm" => "video/webm",
        "zip" => "application/zip",
        _ => "",
    }
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

fn transmission_pieces(meta: Option<&EngineTorrentMetadata>) -> String {
    let Some(meta) = meta else {
        return String::new();
    };
    let mut bytes = vec![0_u8; meta.piece_states.len().div_ceil(8)];
    for (idx, state) in meta.piece_states.iter().enumerate() {
        if matches!(state, EnginePieceState::Complete) {
            bytes[idx / 8] |= 0x80 >> (idx % 8);
        }
    }
    general_purpose::STANDARD.encode(bytes)
}

fn transmission_have_valid(
    entry: &rt_session::TorrentEntry,
    meta: Option<&EngineTorrentMetadata>,
) -> u64 {
    let Some(meta) = meta else {
        return entry.total_length.saturating_sub(entry.amount_left);
    };
    let complete_pieces = meta
        .piece_states
        .iter()
        .filter(|state| matches!(state, EnginePieceState::Complete))
        .count() as u64;
    complete_pieces
        .saturating_mul(meta.piece_length)
        .min(entry.total_length)
}

fn transmission_have_unchecked(
    entry: &rt_session::TorrentEntry,
    meta: Option<&EngineTorrentMetadata>,
) -> u64 {
    let Some(meta) = meta else {
        return 0;
    };
    let bytes = meta
        .piece_states
        .iter()
        .filter(|state| matches!(state, EnginePieceState::Partial))
        .count() as u64
        * meta.piece_length;
    bytes.min(entry.total_length)
}

fn transmission_desired_available(
    entry: &rt_session::TorrentEntry,
    meta: Option<&EngineTorrentMetadata>,
) -> u64 {
    let Some(meta) = meta else {
        return 0;
    };
    let bytes = meta
        .piece_states
        .iter()
        .filter(|state| {
            matches!(
                state,
                EnginePieceState::Complete | EnginePieceState::Partial
            )
        })
        .count() as u64
        * meta.piece_length;
    bytes.min(entry.total_length)
}

fn transmission_webseeds_sending_to_us(webseeds: Option<&Vec<EngineWebseedSnapshot>>) -> usize {
    webseeds
        .map(|webseeds| {
            webseeds
                .iter()
                .filter(|webseed| webseed.is_downloading)
                .count()
        })
        .unwrap_or(0)
}

fn transmission_webseeds_ex(
    meta: Option<&EngineTorrentMetadata>,
    webseeds: Option<&Vec<EngineWebseedSnapshot>>,
) -> Vec<Value> {
    if let Some(webseeds) = webseeds {
        return webseeds
            .iter()
            .map(|webseed| {
                json!({
                    "url": webseed.url,
                    "is_downloading": webseed.is_downloading,
                    "download_bytes_per_second": webseed.download_rate.max(0),
                })
            })
            .collect();
    }
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

fn transmission_trackers(
    meta: Option<&EngineTorrentMetadata>,
    trackers: Option<&Vec<EngineTrackerSnapshot>>,
) -> Vec<Value> {
    if let Some(trackers) = trackers {
        return trackers
            .iter()
            .map(|tracker| {
                json!({
                    "id": tracker.id,
                    "announce": tracker.announce,
                    "scrape": "",
                    "tier": tracker.tier,
                })
            })
            .collect();
    }
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

fn transmission_tracker_stats(
    meta: Option<&EngineTorrentMetadata>,
    trackers: Option<&Vec<EngineTrackerSnapshot>>,
) -> Vec<Value> {
    if let Some(trackers) = trackers {
        return trackers.iter().map(transmission_tracker_stat).collect();
    }
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

fn transmission_tracker_stat(tracker: &EngineTrackerSnapshot) -> Value {
    let failure = tracker.failure_reason.clone().unwrap_or_default();
    let warning = tracker.warning_message.clone().unwrap_or_default();
    let last_result = if !failure.is_empty() {
        failure
    } else {
        warning
    };
    let last_announce_succeeded = tracker.status == "working" || tracker.last_success_at.is_some();
    json!({
        "id": tracker.id,
        "announce": tracker.announce,
        "host": tracker_host(&tracker.announce),
        "tier": tracker.tier,
        "lastAnnounceSucceeded": last_announce_succeeded,
        "lastAnnounceTime": tracker.last_announce_at.unwrap_or(0),
        "lastAnnounceResult": last_result,
        "nextAnnounceTime": tracker.next_announce_at.unwrap_or(0),
        "lastScrapeSucceeded": tracker.seeders.is_some() || tracker.leechers.is_some() || tracker.completed.is_some(),
        "lastScrapeTime": tracker.last_announce_at.unwrap_or(0),
        "lastScrapeResult": "",
        "nextScrapeTime": tracker.next_announce_at.unwrap_or(0),
        "seederCount": tracker.seeders.unwrap_or(-1),
        "leecherCount": tracker.leechers.unwrap_or(-1),
        "downloadCount": tracker.completed.unwrap_or(-1),
        "hasAnnounced": tracker.last_announce_at.is_some(),
        "hasScraped": tracker.seeders.is_some() || tracker.leechers.is_some() || tracker.completed.is_some(),
    })
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

async fn transmission_torrent_limits(state: &AppState, hash: &str) -> Option<EngineTorrentLimits> {
    if let Some(engine) = &state.engine {
        if let Ok(limits) = engine.torrent_limits(hash.to_owned()).await {
            return Some(limits);
        }
    }
    state.torrent_limits.read().await.get(hash).cloned()
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
    let mut session = state.session.write().await;
    set_string_arg(args, "download-dir", &mut session.download_dir);
    set_string_value_arg(args, "incomplete-dir", &mut session.incomplete_dir);
    set_string_value_arg(
        args,
        "preferred-transport",
        &mut session.preferred_transport,
    );
    set_string_value_arg(args, "blocklist-url", &mut session.blocklist_url);
    set_i64_arg(args, "blocklist-size", &mut session.blocklist_size, 0);
    set_string_value_arg(
        args,
        "script-torrent-added-filename",
        &mut session.script_torrent_added_filename,
    );
    set_string_value_arg(
        args,
        "script-torrent-done-filename",
        &mut session.script_torrent_done_filename,
    );
    set_string_value_arg(
        args,
        "script-torrent-done-seeding-filename",
        &mut session.script_torrent_done_seeding_filename,
    );
    set_bool_arg(
        args,
        "incomplete-dir-enabled",
        &mut session.incomplete_dir_enabled,
    );
    set_bool_arg(
        args,
        "rename-partial-files",
        &mut session.rename_partial_files,
    );
    set_bool_arg(
        args,
        "start-added-torrents",
        &mut session.start_added_torrents,
    );
    set_bool_arg(
        args,
        "trash-original-torrent-files",
        &mut session.trash_original_torrent_files,
    );
    set_bool_arg(
        args,
        "alt-speed-time-enabled",
        &mut session.alt_speed_time_enabled,
    );
    set_i64_arg(
        args,
        "alt-speed-time-begin",
        &mut session.alt_speed_time_begin,
        0,
    );
    set_i64_arg(
        args,
        "alt-speed-time-end",
        &mut session.alt_speed_time_end,
        0,
    );
    set_i64_arg(
        args,
        "alt-speed-time-day",
        &mut session.alt_speed_time_day,
        0,
    );
    set_bool_arg(
        args,
        "download-queue-enabled",
        &mut session.download_queue_enabled,
    );
    set_i64_arg(
        args,
        "download-queue-size",
        &mut session.download_queue_size,
        0,
    );
    set_bool_arg(args, "seed-queue-enabled", &mut session.seed_queue_enabled);
    set_i64_arg(args, "seed-queue-size", &mut session.seed_queue_size, 0);
    set_i64_arg(args, "peer-limit-global", &mut session.peer_limit_global, 0);
    set_i64_arg(
        args,
        "peer-limit-per-torrent",
        &mut session.peer_limit_per_torrent,
        0,
    );
    set_i64_arg(args, "peer-port", &mut session.peer_port, 0);
    set_bool_arg(
        args,
        "port-forwarding-enabled",
        &mut session.port_forwarding_enabled,
    );
    set_bool_arg(
        args,
        "rpc-authentication-required",
        &mut session.rpc_authentication_required,
    );
    set_bool_arg(
        args,
        "rpc-whitelist-enabled",
        &mut session.rpc_whitelist_enabled,
    );
    set_string_value_arg(args, "rpc-username", &mut session.rpc_username);
    set_string_value_arg(args, "rpc-bind-address", &mut session.rpc_bind_address);
    // TNG-022: dht-enabled/pex-enabled used to only mutate `session`
    // (process-memory, no DB backing) below -- session-get echoed it
    // straight back, so a client toggling DHT off and reading it back saw
    // a convincing "yes, off" even though the swarm's actual DHT/PEX state
    // never changed. Mirrors the already-working qBittorrent-compat
    // equivalent (`app_set_preferences`): read current engine state,
    // apply only the fields this request actually specified, write back.
    if let Some(engine) = &state.engine {
        let dht_request = transmission_bool_arg(args, "dht-enabled");
        let pex_request = transmission_bool_arg(args, "pex-enabled");
        if dht_request.is_some() || pex_request.is_some() {
            let mut features = engine.network_features().await.unwrap_or_default();
            if let Some(value) = dht_request {
                features.dht = value;
            }
            if let Some(value) = pex_request {
                features.pex = value;
            }
            let _ = engine.update_network_features(features).await;
        }
    }
    set_bool_arg(args, "dht-enabled", &mut session.dht_enabled);
    set_bool_arg(args, "pex-enabled", &mut session.pex_enabled);
    set_bool_arg(args, "lpd-enabled", &mut session.lpd_enabled);
    set_bool_arg(args, "utp-enabled", &mut session.utp_enabled);
    set_bool_arg(args, "blocklist-enabled", &mut session.blocklist_enabled);
    set_bool_arg(
        args,
        "script-torrent-added-enabled",
        &mut session.script_torrent_added_enabled,
    );
    set_bool_arg(
        args,
        "script-torrent-done-enabled",
        &mut session.script_torrent_done_enabled,
    );
    set_bool_arg(
        args,
        "script-torrent-done-seeding-enabled",
        &mut session.script_torrent_done_seeding_enabled,
    );
    if let Some(value) = transmission_f64_arg(args, "seedRatioLimit") {
        session.seed_ratio_limit = value;
    }
    set_bool_arg(args, "seedRatioLimited", &mut session.seed_ratio_limited);
    set_i64_arg(
        args,
        "idle-seeding-limit",
        &mut session.idle_seeding_limit,
        0,
    );
    set_bool_arg(
        args,
        "idle-seeding-limit-enabled",
        &mut session.idle_seeding_limit_enabled,
    );
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

fn transmission_f64_arg(args: &Value, key: &str) -> Option<f64> {
    args.get(key)
        .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
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

fn set_bool_arg(args: &Value, key: &str, target: &mut bool) {
    if let Some(value) = transmission_bool_arg(args, key) {
        *target = value;
    }
}

fn set_i64_arg(args: &Value, key: &str, target: &mut i64, min: i64) {
    if let Some(value) = transmission_i64_arg(args, key) {
        *target = value.max(min);
    }
}

fn set_string_arg(args: &Value, key: &str, target: &mut Option<String>) {
    if let Some(value) = args.get(key).and_then(Value::as_str) {
        *target = Some(value.to_owned());
    }
}

fn set_string_value_arg(args: &Value, key: &str, target: &mut String) {
    if let Some(value) = args.get(key).and_then(Value::as_str) {
        *target = value.to_owned();
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
    use rt_engine::{EnginePieceState, EngineTorrentFile, EngineTrackerSnapshot};
    use rt_session::TorrentEntry;
    use tower::ServiceExt;

    #[tokio::test]
    async fn transmission_router_enforces_configured_token() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut state = AppState::new(Arc::clone(&registry));
        state.api_tokens = Arc::new(vec!["secret".to_owned()]);
        let app = build_transmission_router(state);

        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .body(Body::from(r#"{"method":"session-get"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let allowed = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("authorization", "Bearer secret")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"method":"session-get"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // A correctly-authenticated request without the Transmission
        // session-id header still gets the RPC layer's own 409 CSRF
        // challenge -- auth and the Transmission session-id dance are
        // independent checks, and auth must run first (see the 401 case
        // above) without masking or short-circuiting the second one.
        assert_eq!(allowed.status(), StatusCode::CONFLICT);
    }

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
    fn transmission_tracker_stats_project_persisted_engine_state() {
        let trackers = vec![EngineTrackerSnapshot {
            id: 7,
            tier: 2,
            announce: "https://tracker.example/announce".to_owned(),
            status: "warning".to_owned(),
            last_announce_at: Some(100),
            next_announce_at: Some(200),
            last_success_at: Some(90),
            failure_reason: None,
            warning_message: Some("tracker warning".to_owned()),
            seeders: Some(11),
            leechers: Some(22),
            completed: Some(33),
        }];

        let stats = transmission_tracker_stats(None, Some(&trackers));
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0]["id"], 7);
        assert_eq!(stats[0]["tier"], 2);
        assert_eq!(stats[0]["host"], "tracker.example");
        assert_eq!(stats[0]["lastAnnounceSucceeded"], true);
        assert_eq!(stats[0]["lastAnnounceTime"], 100);
        assert_eq!(stats[0]["nextAnnounceTime"], 200);
        assert_eq!(stats[0]["lastAnnounceResult"], "tracker warning");
        assert_eq!(stats[0]["seederCount"], 11);
        assert_eq!(stats[0]["leecherCount"], 22);
        assert_eq!(stats[0]["downloadCount"], 33);
        assert_eq!(stats[0]["hasAnnounced"], true);
        assert_eq!(stats[0]["hasScraped"], true);
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
                "alt-speed-time-enabled",
                "alt-speed-time-begin",
                "alt-speed-time-end",
                "alt-speed-time-day",
                "download-queue-enabled",
                "download-queue-size",
                "seed-queue-enabled",
                "seed-queue-size",
                "queue-stalled-enabled",
                "queue-stalled-minutes",
                "peer-limit-global",
                "peer-limit-per-torrent",
                "preferred-transport",
                "script-torrent-added-enabled",
                "script-torrent-added-filename",
                "script-torrent-done-enabled",
                "script-torrent-done-filename",
                "script-torrent-done-seeding-enabled",
                "script-torrent-done-seeding-filename",
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
            comment: None,
            created_by: None,
            creation_date: None,
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
                    path: "two.mkv".into(),
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
        assert_eq!(
            transmission_primary_mime_type(&entry, Some(&meta)),
            "video/x-matroska"
        );
        assert_eq!(transmission_pieces(Some(&meta)), "gA==");
        assert_eq!(transmission_have_valid(&entry, Some(&meta)), 100);
        assert_eq!(transmission_have_unchecked(&entry, Some(&meta)), 100);
        assert_eq!(transmission_desired_available(&entry, Some(&meta)), 200);
    }

    #[test]
    fn transmission_lifecycle_seconds_project_registry_timestamps() {
        let mut active = TorrentEntry::new("a".repeat(40), "active".into(), "/data".into());
        active.added_at = 1_000;
        active.completed_at = None;
        assert_eq!(transmission_seconds_downloading(&active, 1_090), 90);
        assert_eq!(transmission_seconds_seeding(&active, 1_090), 0);

        let mut seeding = TorrentEntry::new("b".repeat(40), "seed".into(), "/data".into());
        seeding.added_at = 1_000;
        seeding.completed_at = Some(1_075);
        assert_eq!(transmission_seconds_downloading(&seeding, 1_150), 75);
        assert_eq!(transmission_seconds_seeding(&seeding, 1_150), 75);
    }

    #[test]
    fn transmission_webseed_activity_projects_engine_snapshots() {
        let webseeds = vec![
            EngineWebseedSnapshot {
                url: "https://seed.example/one.bin".to_owned(),
                is_downloading: true,
                download_rate: 16_384,
                failures: 0,
            },
            EngineWebseedSnapshot {
                url: "https://mirror.example/one.bin".to_owned(),
                is_downloading: false,
                download_rate: 0,
                failures: 2,
            },
        ];

        assert_eq!(transmission_webseeds_sending_to_us(Some(&webseeds)), 1);
        let projected = transmission_webseeds_ex(None, Some(&webseeds));
        assert_eq!(projected[0]["url"], "https://seed.example/one.bin");
        assert_eq!(projected[0]["is_downloading"], true);
        assert_eq!(projected[0]["download_bytes_per_second"], 16_384);
        assert!(transmission_field_needs_webseeds("webseeds_ex"));
    }

    #[test]
    fn transmission_peer_rates_sum_native_snapshots() {
        let peers = vec![
            EnginePeerSnapshot {
                addr: "127.0.0.1:6881".parse().unwrap(),
                client: "peer-a".to_owned(),
                choked: false,
                upload_choked: false,
                interested: true,
                pieces: 1,
                pieces_total: 2,
                progress: 0.5,
                download_rate: 1_000,
                upload_rate: 2_000,
                downloaded: 10,
                uploaded: 20,
            },
            EnginePeerSnapshot {
                addr: "127.0.0.2:6881".parse().unwrap(),
                client: "peer-b".to_owned(),
                choked: false,
                upload_choked: false,
                interested: true,
                pieces: 2,
                pieces_total: 2,
                progress: 1.0,
                download_rate: 3_000,
                upload_rate: 4_000,
                downloaded: 30,
                uploaded: 40,
            },
        ];
        assert_eq!(transmission_peer_download_rate(Some(&peers)), 4_000);
        assert_eq!(transmission_peer_upload_rate(Some(&peers)), 6_000);
        assert_eq!(transmission_peer_download_rate(None), 0);
        assert_eq!(transmission_peer_upload_rate(None), 0);
        assert_eq!(transmission_eta(8_001, 4_000), 3);
        assert_eq!(transmission_eta(0, 4_000), 0);
        assert_eq!(transmission_eta(8_001, 0), -1);
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
    async fn transmission_session_set_persists_broad_compat_settings_without_engine() {
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
                        r#"{"method":"session-set","arguments":{
                            "download-dir":"/media",
                            "incomplete-dir":"/partial",
                            "incomplete-dir-enabled":true,
                            "rename-partial-files":true,
                            "start-added-torrents":false,
                            "trash-original-torrent-files":true,
                            "alt-speed-time-enabled":true,
                            "alt-speed-time-begin":60,
                            "alt-speed-time-end":120,
                            "alt-speed-time-day":62,
                            "download-queue-enabled":true,
                            "download-queue-size":4,
                            "seed-queue-enabled":true,
                            "seed-queue-size":8,
                            "peer-limit-global":200,
                            "peer-limit-per-torrent":50,
                            "peer-port":51413,
                            "port-forwarding-enabled":true,
                            "rpc-authentication-required":true,
                            "rpc-whitelist-enabled":true,
                            "rpc-username":"operator",
                            "rpc-bind-address":"127.0.0.1",
                            "dht-enabled":false,
                            "pex-enabled":false,
                            "lpd-enabled":true,
                            "utp-enabled":false,
                            "preferred-transport":"utp",
                            "blocklist-enabled":true,
                            "blocklist-size":42,
                            "blocklist-url":"https://example.invalid/blocklist.gz",
                            "script-torrent-added-enabled":true,
                            "script-torrent-added-filename":"/hooks/add.sh",
                            "script-torrent-done-enabled":true,
                            "script-torrent-done-filename":"/hooks/done.sh",
                            "script-torrent-done-seeding-enabled":true,
                            "script-torrent-done-seeding-filename":"/hooks/seed.sh",
                            "seedRatioLimit":2.5,
                            "seedRatioLimited":true,
                            "idle-seeding-limit":1440,
                            "idle-seeding-limit-enabled":true
                        }}"#,
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
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let args = &body["arguments"];
        assert_eq!(args["download-dir"], "/media");
        assert_eq!(args["incomplete-dir"], "/partial");
        assert_eq!(args["incomplete-dir-enabled"], true);
        assert_eq!(args["rename-partial-files"], true);
        assert_eq!(args["start-added-torrents"], false);
        assert_eq!(args["trash-original-torrent-files"], true);
        assert_eq!(args["alt-speed-time-enabled"], true);
        assert_eq!(args["alt-speed-time-begin"], 60);
        assert_eq!(args["alt-speed-time-end"], 120);
        assert_eq!(args["alt-speed-time-day"], 62);
        assert_eq!(args["download-queue-enabled"], true);
        assert_eq!(args["download-queue-size"], 4);
        assert_eq!(args["seed-queue-enabled"], true);
        assert_eq!(args["seed-queue-size"], 8);
        assert_eq!(args["peer-limit-global"], 200);
        assert_eq!(args["peer-limit-per-torrent"], 50);
        assert_eq!(args["peer-port"], 51413);
        assert_eq!(args["port-forwarding-enabled"], true);
        assert_eq!(args["rpc-authentication-required"], true);
        assert_eq!(args["rpc-whitelist-enabled"], true);
        assert_eq!(args["rpc-username"], "operator");
        assert_eq!(args["rpc-bind-address"], "127.0.0.1");
        assert_eq!(args["dht-enabled"], false);
        assert_eq!(args["pex-enabled"], false);
        assert_eq!(args["lpd-enabled"], true);
        assert_eq!(args["utp-enabled"], false);
        assert_eq!(args["preferred-transport"], "utp");
        assert_eq!(args["blocklist-enabled"], true);
        assert_eq!(args["blocklist-size"], 42);
        assert_eq!(
            args["blocklist-url"],
            "https://example.invalid/blocklist.gz"
        );
        assert_eq!(args["script-torrent-added-enabled"], true);
        assert_eq!(args["script-torrent-added-filename"], "/hooks/add.sh");
        assert_eq!(args["script-torrent-done-enabled"], true);
        assert_eq!(args["script-torrent-done-filename"], "/hooks/done.sh");
        assert_eq!(args["script-torrent-done-seeding-enabled"], true);
        assert_eq!(
            args["script-torrent-done-seeding-filename"],
            "/hooks/seed.sh"
        );
        assert_eq!(args["seedRatioLimit"], 2.5);
        assert_eq!(args["seedRatioLimited"], true);
        assert_eq!(args["idle-seeding-limit"], 1440);
        assert_eq!(args["idle-seeding-limit-enabled"], true);
    }

    #[tokio::test]
    async fn transmission_session_access_control_projects_session_security_state() {
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
                        r#"{"method":"session-set","arguments":{"blocklist-enabled":true,"rpc-authentication-required":true,"rpc-whitelist-enabled":true,"rpc-username":"operator","rpc-bind-address":"127.0.0.1"}}"#,
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
                    .body(Body::from(r#"{"method":"session-access-control"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let args = &body["arguments"];
        assert_eq!(args["blocklist-enabled"], true);
        assert_eq!(args["rpc-authentication-required"], true);
        assert_eq!(args["rpc-whitelist-enabled"], true);
        assert_eq!(args["rpc-username"], "operator");
        assert_eq!(args["rpc-bind-address"], "127.0.0.1");
    }

    #[tokio::test]
    async fn transmission_group_methods_roundtrip_compat_state() {
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
                        r#"{"method":"group-set","arguments":{
                            "group":"archive",
                            "honors-session-limits":false,
                            "speed-limit-down-enabled":true,
                            "speed-limit-down":2048,
                            "speed-limit-up-enabled":true,
                            "speed-limit-up":1024
                        }}"#,
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
                        r#"{"method":"group-get","arguments":{"group":"archive"}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let group = &body["arguments"]["groups"][0];
        assert_eq!(group["name"], "archive");
        assert_eq!(group["honors-session-limits"], false);
        assert_eq!(group["speed-limit-down-enabled"], true);
        assert_eq!(group["speed-limit-down"], 2048);
        assert_eq!(group["speed-limit-up-enabled"], true);
        assert_eq!(group["speed-limit-up"], 1024);
    }

    #[tokio::test]
    async fn transmission_torrent_group_assignment_roundtrips_in_torrent_get() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            reg.add(TorrentEntry::new(
                "b".repeat(40),
                "beta".into(),
                "/downloads".into(),
            ))
            .unwrap();
        }
        let app = build_transmission_router(AppState::new(registry));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"group-set","arguments":{"group":"archive","honors-session-limits":false}}"#,
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
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"torrent-set","arguments":{"ids":["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],"group":"archive"}}"#,
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
                        r#"{"method":"torrent-get","arguments":{"fields":["group","honorsSessionLimits"]}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let torrent = &body["arguments"]["torrents"][0];
        assert_eq!(torrent["group"], "archive");
        assert_eq!(torrent["honorsSessionLimits"], false);
    }

    #[tokio::test]
    async fn transmission_sequential_from_piece_roundtrips_in_torrent_get() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            reg.add(TorrentEntry::new(
                "c".repeat(40),
                "gamma".into(),
                "/downloads".into(),
            ))
            .unwrap();
        }
        let app = build_transmission_router(AppState::new(registry));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"torrent-set","arguments":{"ids":["cccccccccccccccccccccccccccccccccccccccc"],"sequentialDownloadFromPiece":123}}"#,
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
                        r#"{"method":"torrent-get","arguments":{"fields":["sequentialDownloadFromPiece"]}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["arguments"]["torrents"][0]["sequentialDownloadFromPiece"],
            123
        );
    }

    #[tokio::test]
    async fn transmission_notification_subscriptions_roundtrip_state() {
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
                        r#"{"method":"session-subscribe","arguments":{"fields":["torrent-added","torrent-removed"]}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["arguments"]["subscriptions"],
            json!(["torrent-added", "torrent-removed"])
        );

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"session-unsubscribe","arguments":{"fields":["torrent-added"]}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["arguments"]["subscriptions"],
            json!(["torrent-removed"])
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"method":"session-get","arguments":{"fields":["notification-subscriptions"]}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["arguments"]["notification-subscriptions"],
            json!(["torrent-removed"])
        );
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
    async fn transmission_json_rpc_20_uses_params_and_direct_result() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("f".repeat(40), "foxtrot".into(), "/data".into());
            entry.total_length = 80;
            entry.amount_left = 20;
            reg.add(entry).unwrap();
        }
        let app = build_transmission_router(AppState::new(registry));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","method":"torrent_get","params":{"fields":["hash_string","name","percent_done","left_until_done"]},"id":7}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 7);
        assert!(body.get("arguments").is_none());
        assert_eq!(body["result"]["torrents"][0]["hash_string"], "f".repeat(40));
        assert_eq!(body["result"]["torrents"][0]["name"], "foxtrot");
        assert_eq!(body["result"]["torrents"][0]["percent_done"], 0.75);
        assert_eq!(body["result"]["torrents"][0]["left_until_done"], 20);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","method":"no_such_method","params":{},"id":"bad"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], "bad");
        assert_eq!(body["error"]["code"], -32601);
        assert_eq!(body["error"]["message"], "method name not recognized");
    }

    #[tokio::test]
    async fn transmission_json_rpc_20_batch_requests_are_supported() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("1".repeat(40), "one".into(), "/data".into());
            entry.total_length = 10;
            entry.amount_left = 0;
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
                        r#"[
                            {"jsonrpc":"2.0","method":"session_get","params":{"fields":["version"]},"id":1},
                            {"jsonrpc":"2.0","method":"torrent_get","params":{"fields":["name","percent_done"]},"id":2}
                        ]"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body[0]["jsonrpc"], "2.0");
        assert_eq!(body[0]["id"], 1);
        assert_eq!(body[0]["result"]["version"], "TorrentNG");
        assert_eq!(body[1]["jsonrpc"], "2.0");
        assert_eq!(body[1]["id"], 2);
        assert_eq!(body[1]["result"]["torrents"][0]["name"], "one");
        assert_eq!(body[1]["result"]["torrents"][0]["percent_done"], 1.0);
    }

    #[tokio::test]
    async fn transmission_batches_are_bounded_before_response_allocation() {
        let app =
            build_transmission_router(AppState::new(Arc::new(RwLock::new(SessionRegistry::new()))));
        let requests = vec![
            json!({
                "jsonrpc": "2.0",
                "method": "session_get",
                "params": {},
                "id": 1
            });
            MAX_TRANSMISSION_BATCH_REQUESTS + 1
        ];
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(serde_json::to_vec(&requests).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn transmission_torrent_get_supports_table_format_and_recently_active() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("e".repeat(40), "echo".into(), "/data".into());
            entry.total_length = 200;
            entry.amount_left = 50;
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
                        r#"{"method":"torrent-get","arguments":{"ids":"recently-active","format":"table","fields":["id","hashString","name","percentDone","leftUntilDone"]}}"#,
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
            body["arguments"]["fields"],
            json!(["id", "hashString", "name", "percentDone", "leftUntilDone"])
        );
        assert_eq!(body["arguments"]["torrents"][0][0], 1);
        assert_eq!(body["arguments"]["torrents"][0][1], "e".repeat(40));
        assert_eq!(body["arguments"]["torrents"][0][2], "echo");
        assert_eq!(body["arguments"]["torrents"][0][3], 0.75);
        assert_eq!(body["arguments"]["torrents"][0][4], 50);
        assert_eq!(body["arguments"]["removed"], json!([]));
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
    async fn transmission_torrent_set_limits_roundtrip_without_engine() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            reg.add(TorrentEntry::new(
                "e".repeat(40),
                "epsilon".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        let hash = "e".repeat(40);
        let app = build_transmission_router(AppState::new(registry));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transmission/rpc")
                    .header("content-type", "application/json")
                    .header("x-transmission-session-id", SESSION_ID)
                    .body(Body::from(format!(
                        r#"{{"method":"torrent-set","arguments":{{"ids":["{hash}"],"download-limit":128,"download-limited":true,"upload-limit":64,"upload-limited":true,"peer-limit":42,"seed-ratio-limit":2.25,"seed-ratio-mode":1,"seed-idle-limit":90,"seed-idle-mode":1,"sequential-download":true}}}}"#
                    )))
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
                    .body(Body::from(format!(
                        r#"{{"method":"torrent-get","arguments":{{"ids":["{hash}"],"fields":["downloadLimit","downloadLimited","uploadLimit","uploadLimited","maxConnectedPeers","seedRatioLimit","seedRatioMode","seedIdleLimit","seedIdleMode","sequentialDownload"]}}}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let torrent = &body["arguments"]["torrents"][0];
        assert_eq!(torrent["downloadLimit"], 128);
        assert_eq!(torrent["downloadLimited"], true);
        assert_eq!(torrent["uploadLimit"], 64);
        assert_eq!(torrent["uploadLimited"], true);
        assert_eq!(torrent["maxConnectedPeers"], 42);
        assert_eq!(torrent["seedRatioLimit"], 2.25);
        assert_eq!(torrent["seedRatioMode"], 1);
        assert_eq!(torrent["seedIdleLimit"], 90);
        assert_eq!(torrent["seedIdleMode"], 1);
        assert_eq!(torrent["sequentialDownload"], true);
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
            (
                "session-subscribe",
                r#"{"fields":["torrent-added","torrent-removed"]}"#,
            ),
            ("session-unsubscribe", r#"{"fields":["torrent-added"]}"#),
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

    #[test]
    fn transmission_recheck_progress_projects_active_engine_job() {
        let jobs = vec![
            EngineJob {
                job_id: "old".to_owned(),
                kind: "recheck_torrent".to_owned(),
                state: "running".to_owned(),
                dry_run: false,
                affected_torrents: vec!["a".repeat(40)],
                total: 100,
                done: 20,
                checkpoint: 0,
                byte_offset: None,
                verified_bytes: 0,
                error: None,
                created_at: 1,
                started_at: Some(1),
                updated_at: 10,
                finished_at: None,
            },
            EngineJob {
                job_id: "new".to_owned(),
                kind: "recheck_torrent".to_owned(),
                state: "paused".to_owned(),
                dry_run: false,
                affected_torrents: vec!["a".repeat(40)],
                total: 100,
                done: 75,
                checkpoint: 0,
                byte_offset: None,
                verified_bytes: 0,
                error: None,
                created_at: 2,
                started_at: Some(2),
                updated_at: 20,
                finished_at: None,
            },
            EngineJob {
                job_id: "done".to_owned(),
                kind: "recheck_torrent".to_owned(),
                state: "completed".to_owned(),
                dry_run: false,
                affected_torrents: vec!["b".repeat(40)],
                total: 100,
                done: 100,
                checkpoint: 0,
                byte_offset: None,
                verified_bytes: 0,
                error: None,
                created_at: 3,
                started_at: Some(3),
                updated_at: 30,
                finished_at: Some(30),
            },
        ];

        assert_eq!(
            transmission_recheck_progress(&jobs, &"a".repeat(40)),
            Some(0.75)
        );
        assert_eq!(transmission_recheck_progress(&jobs, &"b".repeat(40)), None);
        assert!(transmission_field_is_recheck_progress("recheck_progress"));
    }

    #[test]
    fn transmission_api_snapshot_estimates_scale_with_torrent_and_field_count() {
        assert_eq!(
            estimate_transmission_torrent_get_snapshot_bytes(0, 0),
            16 * 1024
        );
        assert_eq!(
            estimate_transmission_torrent_get_snapshot_bytes(10, 0),
            16 * 1024 + 10 * (1024 + 384)
        );
        assert!(
            estimate_transmission_torrent_get_snapshot_bytes(10, 20)
                > estimate_transmission_torrent_get_snapshot_bytes(10, 1)
        );
    }
}
