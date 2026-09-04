#![recursion_limit = "256"]

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
};

use axum::{
    body::{to_bytes, Body},
    extract::{DefaultBodyLimit, State},
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose, Engine as _};
use rt_api_model::{
    csrf_request_allowed, request_fingerprint, session_cookie_value, valid_idempotency_key,
    CachedResponse, IdempotencyClaim, IdempotencyStore, MAX_IDEMPOTENCY_BODY_BYTES,
};
use rt_engine::{
    EngineHandle, EnginePeerSnapshot, EngineTorrentLimits, EngineTorrentMetadata,
    EngineTrackerSnapshot, QueueMove,
};
use rt_metainfo::parse_magnet;
use rt_metrics::{MemoryClass, MemoryLease};
use rt_session::SessionRegistry;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    sync::{Notify, RwLock},
    task::JoinSet,
};

// Deluge's compatibility API has no offset/cursor contract. Keep its legacy
// full-list calls bounded rather than allowing one client request to turn
// into an unbounded response and one live-engine query per torrent.
const MAX_LEGACY_FULL_LIST_ENTRIES: usize = 10_000;
const DELUGE_RUNTIME_PROJECTION_CONCURRENCY: usize = 64;

struct DelugeRuntimeProjection {
    info_hash: String,
    metadata: Option<EngineTorrentMetadata>,
    peers: Option<Vec<EnginePeerSnapshot>>,
    trackers: Option<Vec<EngineTrackerSnapshot>>,
    limits: Option<EngineTorrentLimits>,
}

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub api_tokens: Arc<Vec<String>>,
    pub shutdown: Option<Arc<Notify>>,
    pub torrent_options: Arc<RwLock<HashMap<String, EngineTorrentLimits>>>,
    pub move_completed_options: Arc<RwLock<HashMap<String, DelugeMoveCompletedOptions>>>,
    pub url_downloads: Arc<RwLock<HashMap<String, String>>>,
    pub next_url_download_id: Arc<RwLock<u64>>,
    pub enabled_plugins: Arc<RwLock<HashSet<String>>>,
    pub plugin_configs: Arc<RwLock<HashMap<String, Value>>>,
    pub execute_commands: Arc<RwLock<Vec<Value>>>,
    pub(crate) idempotency: Arc<IdempotencyStore>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelugeMoveCompletedOptions {
    pub enabled: bool,
    pub path: String,
}

impl AppState {
    pub fn new(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        Self {
            registry,
            engine: None,
            api_tokens: Arc::new(Vec::new()),
            shutdown: None,
            torrent_options: Arc::new(RwLock::new(HashMap::new())),
            move_completed_options: Arc::new(RwLock::new(HashMap::new())),
            url_downloads: Arc::new(RwLock::new(HashMap::new())),
            next_url_download_id: Arc::new(RwLock::new(1)),
            enabled_plugins: Arc::new(RwLock::new(default_enabled_plugins())),
            plugin_configs: Arc::new(RwLock::new(HashMap::new())),
            execute_commands: Arc::new(RwLock::new(Vec::new())),
            idempotency: IdempotencyStore::new(),
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
            shutdown: None,
            torrent_options: Arc::new(RwLock::new(HashMap::new())),
            move_completed_options: Arc::new(RwLock::new(HashMap::new())),
            url_downloads: Arc::new(RwLock::new(HashMap::new())),
            next_url_download_id: Arc::new(RwLock::new(1)),
            enabled_plugins: Arc::new(RwLock::new(default_enabled_plugins())),
            plugin_configs: Arc::new(RwLock::new(HashMap::new())),
            execute_commands: Arc::new(RwLock::new(Vec::new())),
            idempotency: IdempotencyStore::new(),
        }
    }
}

async fn reserve_deluge_api_snapshot(
    state: &AppState,
    bytes: u64,
) -> Result<Option<MemoryLease>, String> {
    let Some(engine) = &state.engine else {
        return Ok(None);
    };
    engine.reserve_memory(MemoryClass::ApiSnapshot, bytes).await
}

fn estimate_deluge_torrents_snapshot_bytes(torrent_count: usize) -> u64 {
    (torrent_count as u64).saturating_mul(3072)
}

fn estimate_deluge_update_ui_snapshot_bytes(torrent_count: usize) -> u64 {
    32 * 1024 + (torrent_count as u64).saturating_mul(4096)
}

fn estimate_deluge_torrent_detail_snapshot_bytes() -> u64 {
    64 * 1024
}

fn deluge_engine(state: &AppState) -> Result<&EngineHandle, String> {
    state
        .engine
        .as_ref()
        .ok_or_else(|| "native engine is unavailable; mutation was not applied".to_owned())
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Vec<Value>,
}

pub fn build_deluge_router(state: AppState) -> Router {
    Router::new()
        .route("/json", post(json_rpc))
        .route("/deluge/json", post(json_rpc))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            deluge_auth_guard,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            deluge_idempotency_guard,
        ))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(state)
}

async fn deluge_idempotency_guard(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(key) = req
        .headers()
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
    else {
        return next.run(req).await;
    };
    if !valid_idempotency_key(&key) {
        return (StatusCode::BAD_REQUEST, "invalid Idempotency-Key").into_response();
    }

    let (mut parts, body) = req.into_parts();
    let body = match to_bytes(body, MAX_IDEMPOTENCY_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let fingerprint =
        request_fingerprint(parts.method.as_str(), &parts.uri.to_string(), body.as_ref());
    parts.headers.remove(header::CONTENT_LENGTH);
    let req = Request::from_parts(parts, Body::from(body.to_vec()));

    loop {
        match state.idempotency.claim(&key, fingerprint) {
            IdempotencyClaim::Execute => break,
            IdempotencyClaim::Wait(notify) => notify.notified().await,
            IdempotencyClaim::Replay(cached) => return replay_idempotent_response(cached),
            IdempotencyClaim::Conflict => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Idempotency-Key was already used for a different request",
                )
                    .into_response();
            }
            IdempotencyClaim::Saturated => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "idempotency store is saturated; retry later",
                )
                    .into_response();
            }
        }
    }

    let mut execution = state.idempotency.execution_guard(&key, fingerprint);
    let response = next.run(req).await;
    let (parts, body) = response.into_parts();
    let body = match to_bytes(body, MAX_IDEMPOTENCY_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "mutation response exceeded the idempotency response limit",
            )
                .into_response();
        }
    };
    let response = Response::from_parts(parts.clone(), Body::from(body.clone()));
    if parts.status.is_success() {
        let headers = parts
            .headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
            .collect();
        execution.complete(CachedResponse {
            status: parts.status.as_u16(),
            headers,
            body: body.to_vec(),
        });
    } else {
        execution.abandon();
    }
    response
}

fn replay_idempotent_response(cached: CachedResponse) -> Response {
    let mut response = Response::new(Body::from(cached.body));
    *response.status_mut() = StatusCode::from_u16(cached.status).unwrap_or(StatusCode::OK);
    for (name, value) in cached.headers {
        let Ok(name) = axum::http::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(value) = axum::http::HeaderValue::from_bytes(&value) else {
            continue;
        };
        response.headers_mut().append(name, value);
    }
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("idempotency-replayed"),
        axum::http::HeaderValue::from_static("true"),
    );
    response
}

async fn deluge_auth_guard(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if state.api_tokens.is_empty() {
        return next.run(req).await;
    }
    if request_bearer_token(&req)
        .is_some_and(|token| state.api_tokens.iter().any(|allowed| allowed == &token))
    {
        return next.run(req).await;
    }
    if session_cookie_value(req.headers(), &["tng_session", "SID"])
        .is_some_and(|token| state.api_tokens.iter().any(|allowed| allowed == &token))
    {
        if deluge_is_mutating(&req) && !csrf_request_allowed(req.headers()) {
            return (StatusCode::FORBIDDEN, "cross-site cookie mutation rejected").into_response();
        }
        return next.run(req).await;
    }

    let (parts, body) = req.into_parts();
    let body = match to_bytes(body, 1024 * 1024).await {
        Ok(body) => body,
        Err(_) => {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
    };
    let login_token = serde_json::from_slice::<JsonRpcRequest>(&body)
        .ok()
        .filter(|request| request.method == "auth.login")
        .and_then(|request| {
            request
                .params
                .first()
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        });
    let req = Request::from_parts(parts, Body::from(body));
    if login_token.is_some_and(|token| state.api_tokens.iter().any(|allowed| allowed == &token)) {
        return next.run(req).await;
    }

    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":{"message":"authentication required","code":1}}"#,
    )
        .into_response()
}

fn request_bearer_token(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
}

fn deluge_is_mutating(req: &Request<Body>) -> bool {
    matches!(
        *req.method(),
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    )
}

pub async fn json_rpc(
    State(state): State<AppState>,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let result = dispatch(&state, &req.method, &req.params).await;
    let payload = match result {
        Ok(result) => json!({
            "id": req.id,
            "result": result,
            "error": null,
        }),
        Err(message) => json!({
            "id": req.id,
            "result": null,
            "error": {
                "message": message,
                "code": 1,
            },
        }),
    };
    Json(payload)
}

async fn dispatch(state: &AppState, method: &str, params: &[Value]) -> Result<Value, String> {
    match method {
        "auth.login" => Ok(json!(true)),
        "auth.check_session" => Ok(json!(true)),
        "daemon.login" => Ok(json!(true)),
        "daemon.info" => Ok(json!({
            "version": "TorrentNG",
            "libtorrent": "native",
        })),
        "daemon.get_method_list" => Ok(json!(supported_methods())),
        "daemon.shutdown" => {
            if let Some(shutdown) = &state.shutdown {
                // Retain the permit when the daemon has not reached its
                // graceful-shutdown select yet; a client request must not be
                // lost during startup.
                shutdown.notify_one();
            }
            Ok(json!(true))
        }
        "web.connected" => Ok(json!(state
            .engine
            .as_ref()
            .is_some_and(EngineHandle::is_alive))),
        "web.add_host" => Ok(json!("TorrentNG")),
        "web.edit_host" | "web.remove_host" => Err(
            "unsupported Deluge method: host configuration is owned by torrentngd".to_owned(),
        ),
        "web.get_config" => Ok(deluge_web_config()),
        "web.get_host_status" => Ok(json!([
            "TorrentNG",
            "127.0.0.1",
            0,
            if state
                .engine
                .as_ref()
                .is_some_and(EngineHandle::is_alive)
            {
                "Online"
            } else {
                "Offline"
            }
        ])),
        "web.get_hosts" => Ok(json!([["TorrentNG", "127.0.0.1", 0, "TorrentNG"]])),
        "web.connect" | "web.disconnect" => Err(
            "unsupported Deluge method: daemon connection state is owned by torrentngd".to_owned(),
        ),
        "web.start_daemon" | "web.stop_daemon" => Err(
            "unsupported Deluge method: daemon lifecycle is owned by torrentngd".to_owned(),
        ),
        "web.download_torrent_from_url" => web_download_torrent_from_url(state, params).await,
        "web.add_torrents" => web_add_torrents(state, params).await,
        "web.get_events" => web_events(state).await,
        "web.get_plugins" => Ok(json!(deluge_plugins())),
        "web.get_plugin_info" => Ok(plugin_info(params.first().and_then(Value::as_str))),
        "web.upload_plugin" | "web.update_config" | "web.save_config" => Err(
            "unsupported Deluge method: plugin and WebUI configuration writes are not implemented"
                .to_owned(),
        ),
        "web.get_torrent_files" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "missing torrent id".to_owned())?;
            torrent_files(state, hash).await
        }
        "web.update_ui" => update_ui(state, params).await,
        "core.get_session_status" => session_status(state).await,
        "core.get_stats" => session_status(state).await,
        "core.get_num_connections" => deluge_session_connections(state)
            .await
            .map(|value| json!(value)),
        "core.get_download_rate" => deluge_session_download_rate(state)
            .await
            .map(|value| json!(value)),
        "core.get_upload_rate" => deluge_session_upload_rate(state)
            .await
            .map(|value| json!(value)),
        "core.get_filter_tree" => filter_tree(state).await,
        "core.get_session_state" => session_state(state).await,
        "core.get_torrents_status" => torrents_status(state, params).await,
        "core.get_torrent_status" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "missing torrent id".to_owned())?;
            torrent_status(state, hash, params.get(1)).await
        }
        "core.pause_torrent" => {
            let hashes = canonical_torrent_hashes(state, params.first(), "torrent ids").await?;
            let engine = deluge_engine(state)?;
            for hash in hashes {
                engine.pause_torrent(hash).await?;
            }
            Ok(json!(true))
        }
        "core.resume_torrent" => {
            let hashes = canonical_torrent_hashes(state, params.first(), "torrent ids").await?;
            let engine = deluge_engine(state)?;
            for hash in hashes {
                engine.resume_torrent(hash).await?;
            }
            Ok(json!(true))
        }
        "core.force_recheck" => {
            let hashes = canonical_torrent_hashes(state, params.first(), "torrent ids").await?;
            let engine = deluge_engine(state)?;
            for hash in hashes {
                engine.recheck_torrent(hash).await?;
            }
            Ok(json!(true))
        }
        "core.queue_top" => deluge_queue(state, params, QueueMove::Top).await,
        "core.queue_up" => deluge_queue(state, params, QueueMove::Up).await,
        "core.queue_down" => deluge_queue(state, params, QueueMove::Down).await,
        "core.queue_bottom" => deluge_queue(state, params, QueueMove::Bottom).await,
        "core.create_torrent" | "core.upload_plugin" | "core.rescan_plugins" => Err(
            "unsupported Deluge method: native engine does not provide this operation".to_owned(),
        ),
        "core.set_torrent_prioritize_first_last" => set_prioritize_first_last(state, params).await,
        "core.set_torrent_file_priorities" => set_file_priorities(state, params).await,
        "core.set_torrent_trackers" => set_trackers(state, params).await,
        "core.connect_peer" => connect_peer(state, params).await,
        "core.rename_files" => rename_files(state, params).await,
        "core.rename_folder" => rename_folder(state, params).await,
        "core.move_storage" => move_storage(state, params).await,
        "core.get_torrent_file_status" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|hash| !hash.is_empty())
                .ok_or_else(|| "missing torrent id".to_owned())?;
            torrent_files(state, hash).await
        }
        "core.remove_torrent" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|hash| !hash.is_empty())
                .ok_or_else(|| "missing torrent id".to_owned())?;
            let remove_data = params
                .get(1)
                .map(|value| {
                    deluge_bool(Some(value))
                        .ok_or_else(|| "remove-data must be a boolean".to_owned())
                })
                .transpose()?
                .unwrap_or(false);
            let hash = canonical_torrent_hash(state, hash).await?;
            deluge_engine(state)?
                .remove_torrent(hash, remove_data)
                .await?;
            Ok(json!(true))
        }
        "core.add_torrent_magnet" => {
            let uri = params
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "missing magnet URI".to_owned())?;
            add_magnet(state, uri, params.get(1)).await
        }
        "core.add_torrent_file" => {
            let data = params
                .get(1)
                .and_then(Value::as_str)
                .ok_or_else(|| "missing torrent data".to_owned())?;
            add_torrent_file(state, data, params.get(2)).await
        }
        "core.set_torrent_options" => set_torrent_options(state, params).await,
        "label.get_labels" => labels(state).await,
        "label.add" => add_label(state, params).await,
        "label.remove" => remove_label(state, params).await,
        "label.set_options" => Err(
            "unsupported Deluge method: Label options have no native engine equivalent".to_owned(),
        ),
        "label.set_torrent" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|hash| !hash.is_empty())
                .ok_or_else(|| "missing torrent id".to_owned())?;
            let label = params
                .get(1)
                .and_then(Value::as_str)
                .ok_or_else(|| "missing label".to_owned())?;
            set_label(state, hash, label).await?;
            Ok(json!(true))
        }
        "core.get_free_space" => deluge_free_space(state).await,
        "core.set_config" => Err(
            "unsupported Deluge method: core configuration is not writable through the native API"
                .to_owned(),
        ),
        "core.get_listen_port" => {
            Err("listen-port probing is not exposed by the native compatibility API".to_owned())
        }
        "core.get_external_ip" => Err(
            "unsupported Deluge method: external IP discovery is not exposed by the native API"
                .to_owned(),
        ),
        "core.get_path_size" => Err(
            "unsupported Deluge method: arbitrary filesystem size probes are not exposed by the native API"
                .to_owned(),
        ),
        "core.get_cache_status" => cache_status(state).await,
        "core.get_config" => deluge_config(state).await,
        "core.get_config_values" => deluge_config_values(state, params.first()).await,
        "core.get_config_value" => deluge_config_value(state, params.first()).await,
        "core.get_enabled_plugins" => enabled_plugins(state).await,
        "core.enable_plugin" => set_plugin_enabled(state, params, true),
        "core.disable_plugin" => set_plugin_enabled(state, params, false),
        "core.get_available_plugins" => Ok(json!(deluge_plugins())),
        "core.get_libtorrent_version" => Ok(json!("native")),
        "blocklist.get_config" => plugin_config(state, "blocklist", blocklist_config()).await,
        "blocklist.set_config" => set_plugin_config(state, "blocklist", params),
        "blocklist.get_status" | "blocklist.check_import" => blocklist_status(state).await,
        "blocklist.import" => Err(
            "unsupported Deluge method: blocklist import is not implemented by the native engine"
                .to_owned(),
        ),
        "autoadd.get_config" => plugin_config(state, "autoadd", autoadd_config()).await,
        "autoadd.set_config" => set_plugin_config(state, "autoadd", params),
        "autoadd.enable" => set_plugin_config(state, "autoadd", params),
        "autoadd.disable" => set_plugin_config(state, "autoadd", params),
        "execute.get_commands" => execute_commands(state).await,
        "execute.save_command" => save_execute_command(state, params),
        "execute.remove_command" => remove_execute_command(state, params),
        "scheduler.get_config" => plugin_config(state, "scheduler", scheduler_config()).await,
        "scheduler.set_config" => set_plugin_config(state, "scheduler", params),
        "extractor.get_config" => plugin_config(state, "extractor", extractor_config()).await,
        "extractor.set_config" => set_plugin_config(state, "extractor", params),
        "notifications.get_handled_events" => Ok(json!(notification_events())),
        "notifications.get_subscriptions" => Ok(notification_subscriptions()),
        "notifications.set_config" | "notifications.add_subscription" => Err(
            "unsupported Deluge method: notifications are not configured by the native daemon"
                .to_owned(),
        ),
        _ => Err(format!("unsupported method {method}")),
    }
}

fn supported_methods() -> Vec<&'static str> {
    vec![
        "auth.login",
        "auth.check_session",
        "daemon.login",
        "daemon.info",
        "daemon.get_method_list",
        "daemon.shutdown",
        "web.connected",
        "web.add_host",
        "web.edit_host",
        "web.remove_host",
        "web.get_config",
        "web.update_ui",
        "web.get_events",
        "web.get_hosts",
        "web.get_host_status",
        "web.connect",
        "web.disconnect",
        "web.start_daemon",
        "web.stop_daemon",
        "web.download_torrent_from_url",
        "web.add_torrents",
        "web.get_plugins",
        "web.get_plugin_info",
        "web.upload_plugin",
        "web.update_config",
        "web.save_config",
        "web.get_torrent_files",
        "core.get_torrents_status",
        "core.get_torrent_status",
        "core.get_torrent_file_status",
        "core.get_session_state",
        "core.get_session_status",
        "core.get_stats",
        "core.get_num_connections",
        "core.get_download_rate",
        "core.get_upload_rate",
        "core.get_filter_tree",
        "core.pause_torrent",
        "core.resume_torrent",
        "core.force_recheck",
        "core.queue_top",
        "core.queue_up",
        "core.queue_down",
        "core.queue_bottom",
        "core.remove_torrent",
        "core.add_torrent_magnet",
        "core.add_torrent_file",
        "core.set_torrent_options",
        "core.set_torrent_file_priorities",
        "core.set_torrent_trackers",
        "core.set_torrent_prioritize_first_last",
        "core.connect_peer",
        "core.rename_files",
        "core.rename_folder",
        "core.move_storage",
        "core.get_config",
        "core.get_config_values",
        "core.get_config_value",
        "core.set_config",
        "core.get_free_space",
        "core.get_listen_port",
        "core.get_external_ip",
        "core.get_path_size",
        "core.get_cache_status",
        "core.get_enabled_plugins",
        "core.enable_plugin",
        "core.disable_plugin",
        "core.get_available_plugins",
        "core.get_libtorrent_version",
        "core.create_torrent",
        "core.upload_plugin",
        "core.rescan_plugins",
        "blocklist.get_config",
        "blocklist.set_config",
        "blocklist.get_status",
        "blocklist.check_import",
        "blocklist.import",
        "autoadd.get_config",
        "autoadd.set_config",
        "autoadd.enable",
        "autoadd.disable",
        "execute.get_commands",
        "execute.save_command",
        "execute.remove_command",
        "scheduler.get_config",
        "scheduler.set_config",
        "extractor.get_config",
        "extractor.set_config",
        "label.get_labels",
        "label.add",
        "label.remove",
        "label.set_options",
        "label.set_torrent",
        "notifications.get_handled_events",
        "notifications.get_subscriptions",
        "notifications.set_config",
        "notifications.add_subscription",
    ]
}

fn default_deluge_config() -> Value {
    json!({
        "download_location": "/downloads",
        "move_completed": false,
        "move_completed_path": "/downloads",
        "copy_torrent_file": false,
        "torrentfiles_location": "/downloads",
        "autoadd_enable": false,
        "autoadd_location": "/watch",
        "max_download_speed": -1.0,
        "max_upload_speed": -1.0,
        "max_connections_global": -1,
        "max_upload_slots_global": -1,
        "max_active_limit": -1,
        "max_active_downloading": -1,
        "max_active_seeding": -1,
        "queue_new_to_top": false,
        "ignore_limits_on_local_network": true,
        "share_ratio_limit": -1.0,
        "seed_time_ratio_limit": -1.0,
        "seed_time_limit": -1,
        "stop_seed_at_ratio": false,
        "stop_seed_ratio": 2.0,
        "remove_seed_at_ratio": false,
        "listen_ports": [0, 0],
        "random_port": true,
        "dht": true,
        "upnp": false,
        "natpmp": false,
        "utpex": true,
        "lsd": false,
        "enc_in_policy": 1,
        "enc_out_policy": 1,
        "enc_level": 2,
    })
}

async fn deluge_config(state: &AppState) -> Result<Value, String> {
    let mut config = default_deluge_config();
    let (dht, pex) = if let Some(engine) = &state.engine {
        let features = engine.network_features().await?;
        (features.dht, features.pex)
    } else {
        (false, false)
    };
    config["dht"] = Value::Bool(dht);
    config["utpex"] = Value::Bool(pex);
    Ok(config)
}

async fn deluge_config_values(state: &AppState, keys: Option<&Value>) -> Result<Value, String> {
    let config = deluge_config(state).await?;
    let Some(value) = keys else {
        return Ok(config);
    };
    let Some(keys) = value.as_array() else {
        return Err("Deluge config keys must be an array of strings".to_owned());
    };
    let mut out = serde_json::Map::new();
    for (index, value) in keys.iter().enumerate() {
        let key = value
            .as_str()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| format!("Deluge config keys[{index}] must be a non-empty string"))?;
        out.insert(
            key.to_owned(),
            config.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    Ok(Value::Object(out))
}

async fn deluge_config_value(state: &AppState, key: Option<&Value>) -> Result<Value, String> {
    let key = key
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| "Deluge config key must be a non-empty string".to_owned())?;
    Ok(deluge_config(state)
        .await?
        .get(key)
        .cloned()
        .unwrap_or(Value::Null))
}

fn deluge_web_config() -> Value {
    json!({
        "base": "/",
        "pwd_salt": "",
        "pwd_sha1": "",
        "sessions": {},
        "session_timeout": 3600,
        "default_daemon": "TorrentNG",
        "sidebar_show_zero": false,
        "sidebar_multiple_filters": true,
        "show_session_speed": false,
        "theme": "gray",
        "first_login": false,
    })
}

async fn web_add_torrents(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let Some(torrents) = params.first().and_then(Value::as_array) else {
        return Err("missing torrent list".to_owned());
    };
    let mut results = Vec::new();
    for torrent in torrents {
        let options = torrent.get("options").or_else(|| torrent.get("params"));
        let path = torrent
            .get("path")
            .or_else(|| torrent.get("url"))
            .or_else(|| torrent.get("filename"))
            .or_else(|| torrent.get("file"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = if path.starts_with("magnet:") {
            add_magnet(state, path, options).await
        } else if let Some(url) = state.url_downloads.write().await.remove(path) {
            if url.starts_with("magnet:") {
                add_magnet(state, &url, options).await
            } else {
                Ok(json!({
                    "url": url,
                    "downloaded": false,
                    "reason": "server-side URL fetch is disabled; token preserves Deluge WebUI flow",
                }))
            }
        } else if path.starts_with("http://") || path.starts_with("https://") {
            Ok(json!({
                "url": path,
                "downloaded": false,
                "reason": "server-side URL fetch is disabled; pass web.download_torrent_from_url token to preserve flow",
            }))
        } else if let Some(data) = torrent
            .get("data")
            .or_else(|| torrent.get("torrent"))
            .or_else(|| torrent.get("metainfo"))
            .or_else(|| torrent.get("filedata"))
            .or_else(|| torrent.get("content"))
            .and_then(Value::as_str)
        {
            add_torrent_file(state, data, options).await
        } else if !path.trim().is_empty() {
            add_torrent_path(state, path, options).await
        } else {
            Err("torrent item requires a magnet, embedded metainfo, URL token, or path".to_owned())
        };
        let success = result.as_ref().is_ok_and(|value| {
            value
                .get("downloaded")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        });
        results.push(json!({
            "path": path,
            "success": success,
            "result": result.unwrap_or(Value::Null),
        }));
    }
    Ok(Value::Array(results))
}

async fn web_download_torrent_from_url(
    state: &AppState,
    params: &[Value],
) -> Result<Value, String> {
    let url = params
        .first()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing URL".to_owned())?;
    if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("magnet:")) {
        return Err("unsupported torrent URL scheme".to_owned());
    }

    let mut next = state.next_url_download_id.write().await;
    let token = format!("torrentng-url-download-{}.torrent", *next);
    *next = next.saturating_add(1);
    state
        .url_downloads
        .write()
        .await
        .insert(token.clone(), url.to_owned());
    Ok(json!(token))
}

async fn move_storage(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let location = params
        .get(1)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|location| !location.is_empty())
        .ok_or_else(|| "missing storage path".to_owned())?;
    let hashes = canonical_torrent_hashes(state, params.first(), "torrent ids").await?;
    let engine = deluge_engine(state)?;
    for hash in hashes {
        engine
            .update_torrent_fields(hash, None, Some(std::path::PathBuf::from(location)))
            .await?;
    }
    Ok(json!(true))
}

async fn deluge_queue(
    state: &AppState,
    params: &[Value],
    queue_move: QueueMove,
) -> Result<Value, String> {
    let hashes = canonical_torrent_hashes(state, params.first(), "torrent ids").await?;
    deluge_engine(state)?
        .update_queue_order(hashes, queue_move)
        .await?;
    Ok(json!(true))
}

async fn deluge_free_space(state: &AppState) -> Result<Value, String> {
    let roots = deluge_engine(state)?.list_storage_roots().await?;
    roots
        .into_iter()
        .filter(|root| root.ok)
        .map(|root| root.available_bytes)
        .max()
        .map(Value::from)
        .ok_or_else(|| "no healthy storage root is available for a free-space probe".to_owned())
}

async fn session_state(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    let snapshot = reg.snapshot();
    Ok(json!(snapshot
        .iter()
        .map(|entry| entry.info_hash.clone())
        .collect::<Vec<_>>()))
}

async fn session_status(state: &AppState) -> Result<Value, String> {
    let (torrent_count, total_payload_download, total_payload_upload) = {
        let reg = state.registry.read().await;
        let stats = reg.stats();
        (
            stats.torrents_total,
            stats.bytes_downloaded,
            stats.bytes_uploaded,
        )
    };
    let (download_rate, upload_rate, paused_count, connected_peers) =
        if let Some(engine) = &state.engine {
            let stats = engine.stats().await?;
            (
                stats.download_rate,
                stats.upload_rate,
                stats.torrents_paused,
                stats.connected_peers,
            )
        } else {
            let reg = state.registry.read().await;
            let stats = reg.stats();
            (0, 0, stats.torrents_paused + stats.torrents_stopped, 0)
        };
    Ok(json!({
        "payload_download_rate": download_rate,
        "payload_upload_rate": upload_rate,
        "download_rate": download_rate,
        "upload_rate": upload_rate,
        "num_connections": connected_peers,
        "total_payload_download": total_payload_download,
        "total_payload_upload": total_payload_upload,
        "num_torrents": torrent_count,
        "num_paused": paused_count,
    }))
}

async fn deluge_session_download_rate(state: &AppState) -> Result<i64, String> {
    let Some(engine) = &state.engine else {
        return Ok(0);
    };
    Ok(engine.stats().await?.download_rate)
}

async fn deluge_session_upload_rate(state: &AppState) -> Result<i64, String> {
    let Some(engine) = &state.engine else {
        return Ok(0);
    };
    Ok(engine.stats().await?.upload_rate)
}

async fn deluge_session_connections(state: &AppState) -> Result<u64, String> {
    let Some(engine) = &state.engine else {
        return Ok(0);
    };
    Ok(engine.stats().await?.connected_peers)
}

async fn cache_status(state: &AppState) -> Result<Value, String> {
    let (num_torrents, total_done, total_left) = {
        let reg = state.registry.read().await;
        let stats = reg.stats();
        (
            stats.torrents_total,
            stats.bytes_total.saturating_sub(stats.bytes_left),
            stats.bytes_left,
        )
    };
    let jobs_active = if let Some(engine) = &state.engine {
        engine.stats().await?.jobs_active
    } else {
        0
    };
    Ok(json!({
        "blocks_read": 0,
        "blocks_written": 0,
        "cache_size": total_done.saturating_add(total_left),
        "read_cache_hits": 0,
        "read_cache_size": total_done,
        "total_used_buffers": num_torrents,
        "write_cache_size": total_left,
        "queued_jobs": jobs_active,
    }))
}

async fn web_events(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    let snapshot = reg.snapshot();
    Ok(json!(snapshot
        .iter()
        .map(|entry| {
            json!({
                "event": "TorrentStateChangedEvent",
                "value": [entry.info_hash, deluge_state(entry.state.as_str())],
            })
        })
        .collect::<Vec<_>>()))
}

fn deluge_plugins() -> Vec<&'static str> {
    vec![
        "AutoAdd",
        "Blocklist",
        "Execute",
        "Extractor",
        "Label",
        "Notifications",
        "Scheduler",
    ]
}

fn default_enabled_plugins() -> HashSet<String> {
    ["Label", "Notifications"]
        .into_iter()
        .map(str::to_owned)
        .collect::<HashSet<_>>()
}

fn canonical_plugin_name(name: &str) -> Option<&'static str> {
    match name {
        "AutoAdd" | "autoadd" => Some("AutoAdd"),
        "Blocklist" | "blocklist" => Some("Blocklist"),
        "Execute" | "execute" => Some("Execute"),
        "Extractor" | "extractor" => Some("Extractor"),
        "Label" | "label" => Some("Label"),
        "Notifications" | "notifications" => Some("Notifications"),
        "Scheduler" | "scheduler" => Some("Scheduler"),
        _ => None,
    }
}

async fn enabled_plugins(state: &AppState) -> Result<Value, String> {
    let mut plugins = state
        .enabled_plugins
        .read()
        .await
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    plugins.sort();
    Ok(json!(plugins))
}

fn set_plugin_enabled(_state: &AppState, params: &[Value], enabled: bool) -> Result<Value, String> {
    let name = params
        .first()
        .and_then(Value::as_str)
        .and_then(canonical_plugin_name)
        .ok_or_else(|| "missing plugin name".to_owned())?;
    let operation = if enabled { "enable" } else { "disable" };
    Err(format!(
        "unsupported Deluge method: cannot {operation} {name}; plugin lifecycle is not runtime-backed"
    ))
}

fn plugin_info(name: Option<&str>) -> Value {
    let name = name.unwrap_or_default();
    match name {
        "AutoAdd" | "autoadd" => json!({
            "name": "AutoAdd",
            "version": "TorrentNG",
            "author": "TorrentNG",
            "description": "Watch-directory compatibility configuration; server-side watch execution is disabled unless native automation owns it.",
            "enabled": false,
        }),
        "Blocklist" | "blocklist" => json!({
            "name": "Blocklist",
            "version": "TorrentNG",
            "author": "TorrentNG",
            "description": "Blocklist compatibility configuration with zero-entry status until a native blocklist backend is configured.",
            "enabled": false,
        }),
        "Execute" | "execute" => json!({
            "name": "Execute",
            "version": "TorrentNG",
            "author": "TorrentNG",
            "description": "Execute plugin compatibility surface; arbitrary command execution is intentionally not performed by the facade.",
            "enabled": false,
        }),
        "Extractor" | "extractor" => json!({
            "name": "Extractor",
            "version": "TorrentNG",
            "author": "TorrentNG",
            "description": "Extractor plugin compatibility configuration; archive extraction is left to explicit operator workflows.",
            "enabled": false,
        }),
        "Label" | "label" => json!({
            "name": "Label",
            "version": "TorrentNG",
            "author": "TorrentNG",
            "description": "Category and label compatibility backed by native torrent labels.",
            "enabled": true,
        }),
        "Notifications" | "notifications" => json!({
            "name": "Notifications",
            "version": "TorrentNG",
            "author": "TorrentNG",
            "description": "Native session event notification compatibility.",
            "enabled": true,
        }),
        "Scheduler" | "scheduler" => json!({
            "name": "Scheduler",
            "version": "TorrentNG",
            "author": "TorrentNG",
            "description": "Scheduler plugin compatibility configuration; native limits remain controlled by TorrentNG settings.",
            "enabled": false,
        }),
        _ => json!({}),
    }
}

fn blocklist_config() -> Value {
    json!({
        "enabled": false,
        "url": "",
        "load_on_start": false,
        "check_after_days": 4,
        "list_size": 0,
        "last_update": 0,
    })
}

fn autoadd_config() -> Value {
    json!({
        "enabled": false,
        "watchdirs": {},
        "next_id": 1,
    })
}

fn scheduler_config() -> Value {
    json!({
        "low_down": -1.0,
        "low_up": -1.0,
        "high_down": -1.0,
        "high_up": -1.0,
        "button_state": [[0, 0, 0, 0, 0, 0, 0, 0]],
    })
}

fn extractor_config() -> Value {
    json!({
        "enabled": false,
        "extract_path": "",
        "use_name_folder": true,
    })
}

async fn blocklist_status(state: &AppState) -> Result<Value, String> {
    let config = plugin_config_value(state, "blocklist", blocklist_config()).await;
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let url = config
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(json!({
        "state": if enabled { "Idle" } else { "Disabled" },
        "message": if enabled && !url.is_empty() { "Blocklist configured" } else { "No native blocklist configured" },
        "num_blocked": config.get("list_size").and_then(Value::as_i64).unwrap_or(0).max(0),
        "file_progress": 0.0,
        "file_type": "",
        "up_to_date": true,
    }))
}

async fn plugin_config(state: &AppState, key: &str, default: Value) -> Result<Value, String> {
    Ok(plugin_config_value(state, key, default).await)
}

async fn plugin_config_value(state: &AppState, key: &str, default: Value) -> Value {
    state
        .plugin_configs
        .read()
        .await
        .get(key)
        .cloned()
        .unwrap_or(default)
}

async fn execute_commands(state: &AppState) -> Result<Value, String> {
    Ok(Value::Array(state.execute_commands.read().await.clone()))
}

fn set_plugin_config(_state: &AppState, key: &str, _params: &[Value]) -> Result<Value, String> {
    Err(format!(
        "unsupported Deluge method: {key} plugin configuration is not runtime-backed"
    ))
}

fn save_execute_command(_state: &AppState, _params: &[Value]) -> Result<Value, String> {
    Err(
        "unsupported Deluge method: Execute command registration is not runtime-backed and was not applied"
            .to_owned(),
    )
}

fn remove_execute_command(_state: &AppState, _params: &[Value]) -> Result<Value, String> {
    Err(
        "unsupported Deluge method: Execute command removal is not runtime-backed and was not applied"
            .to_owned(),
    )
}

fn notification_events() -> Vec<&'static str> {
    vec![
        "TorrentAddedEvent",
        "TorrentRemovedEvent",
        "TorrentStateChangedEvent",
        "TorrentFinishedEvent",
    ]
}

fn notification_subscriptions() -> Value {
    notification_events()
        .into_iter()
        .map(|event| (event.to_owned(), json!([])))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

async fn filter_tree(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    let snapshot = reg.snapshot();
    let mut labels = std::collections::BTreeMap::<String, usize>::new();
    let mut states = std::collections::BTreeMap::<String, usize>::new();
    for entry in snapshot.iter() {
        *labels
            .entry(entry.category.clone().unwrap_or_default())
            .or_default() += 1;
        *states
            .entry(deluge_state(entry.state.as_str()).to_owned())
            .or_default() += 1;
    }
    Ok(json!({
        "label": labels.into_iter().map(|(label, count)| json!([label, count])).collect::<Vec<_>>(),
        "state": states.into_iter().map(|(state, count)| json!([state, count])).collect::<Vec<_>>(),
    }))
}

/// Load the optional live fields for a legacy full-list response with a
/// bounded number of in-flight actor requests. Deluge has no page or snapshot
/// contract, so the response still has a hard row cap; it must not also pay
/// one serial round trip per torrent and turn one slow actor into a full-list
/// outage.
async fn load_deluge_runtime_projections(
    engine: &EngineHandle,
    hashes: &[String],
    need_metadata: bool,
    need_peers: bool,
    need_limits: bool,
    need_trackers: bool,
) -> Result<Vec<DelugeRuntimeProjection>, String> {
    if !need_metadata && !need_peers && !need_limits && !need_trackers {
        return Ok(Vec::new());
    }

    let mut projections = Vec::with_capacity(hashes.len());
    for batch in hashes.chunks(DELUGE_RUNTIME_PROJECTION_CONCURRENCY) {
        let mut tasks = JoinSet::new();
        for info_hash in batch {
            let engine = engine.clone();
            let info_hash = info_hash.clone();
            tasks.spawn(async move {
                let metadata = if need_metadata {
                    Some(engine.torrent_metadata(info_hash.clone()).await?)
                } else {
                    None
                };
                let peers = if need_peers {
                    Some(engine.torrent_peers(info_hash.clone()).await?)
                } else {
                    None
                };
                let limits = if need_limits {
                    Some(engine.torrent_limits(info_hash.clone()).await?)
                } else {
                    None
                };
                let trackers = if need_trackers {
                    Some(engine.torrent_trackers(info_hash.clone()).await?)
                } else {
                    None
                };
                Ok::<_, String>(DelugeRuntimeProjection {
                    info_hash,
                    metadata,
                    peers,
                    trackers,
                    limits,
                })
            });
        }
        while let Some(result) = tasks.join_next().await {
            let projection =
                result.map_err(|error| format!("Deluge projection task failed: {error}"))??;
            projections.push(projection);
        }
    }
    Ok(projections)
}

fn merge_deluge_runtime_projections(
    projections: Vec<DelugeRuntimeProjection>,
    metadata: &mut std::collections::HashMap<String, EngineTorrentMetadata>,
    peers: &mut std::collections::HashMap<String, Vec<EnginePeerSnapshot>>,
    trackers: &mut std::collections::HashMap<String, Vec<EngineTrackerSnapshot>>,
    limits: &mut std::collections::HashMap<String, EngineTorrentLimits>,
) {
    for projection in projections {
        let info_hash = projection.info_hash;
        if let Some(value) = projection.metadata {
            metadata.insert(info_hash.clone(), value);
        }
        if let Some(value) = projection.peers {
            peers.insert(info_hash.clone(), value);
        }
        if let Some(value) = projection.trackers {
            trackers.insert(info_hash.clone(), value);
        }
        if let Some(value) = projection.limits {
            limits.insert(info_hash, value);
        }
    }
}

async fn update_ui(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let wanted_fields = deluge_requested_fields(params.first())?;
    let snapshot = {
        let reg = state.registry.read().await;
        reg.snapshot()
    };
    ensure_legacy_full_list_bound(snapshot.len(), "Deluge web.update_ui")?;
    let _lease = if state.engine.is_some() {
        Some(
            reserve_deluge_api_snapshot(
                state,
                estimate_deluge_update_ui_snapshot_bytes(snapshot.len()),
            )
            .await?
            .ok_or_else(|| "api snapshot memory budget exhausted".to_owned())?,
        )
    } else {
        None
    };
    let mut metadata = std::collections::HashMap::new();
    let mut peers = std::collections::HashMap::new();
    let mut trackers = std::collections::HashMap::new();
    let mut active_rechecks = HashSet::new();
    let mut limits_by_hash = state.torrent_options.read().await.clone();
    let move_completed_by_hash = state.move_completed_options.read().await.clone();
    let need_metadata = deluge_fields_need_metadata(&wanted_fields);
    let need_limits = deluge_fields_need_limits(&wanted_fields);
    if let Some(engine) = &state.engine {
        active_rechecks = deluge_active_recheck_hashes(engine).await?;
        let hashes = snapshot
            .iter()
            .map(|entry| entry.info_hash.clone())
            .collect::<Vec<_>>();
        let projections = load_deluge_runtime_projections(
            engine,
            &hashes,
            need_metadata,
            deluge_fields_need_peers(&wanted_fields),
            need_limits,
            deluge_fields_need_trackers(&wanted_fields),
        )
        .await?;
        merge_deluge_runtime_projections(
            projections,
            &mut metadata,
            &mut peers,
            &mut trackers,
            &mut limits_by_hash,
        );
    }
    let torrents = snapshot
        .iter()
        .map(|entry| {
            (
                entry.info_hash.clone(),
                filter_deluge_torrent_fields(
                    deluge_torrent(
                        entry,
                        metadata.get(&entry.info_hash),
                        peers.get(&entry.info_hash).map(Vec::as_slice),
                        trackers.get(&entry.info_hash).map(Vec::as_slice),
                        limits_by_hash.get(&entry.info_hash),
                        move_completed_by_hash.get(&entry.info_hash),
                        active_rechecks.contains(&entry.info_hash),
                    ),
                    &wanted_fields,
                ),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let runtime_stats = match &state.engine {
        Some(engine) => Some(engine.stats().await?),
        None => None,
    };
    let free_space = if let Some(engine) = &state.engine {
        engine
            .list_storage_roots()
            .await?
            .into_iter()
            .filter(|root| root.ok)
            .map(|root| root.available_bytes)
            .max()
            .map(Value::from)
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let connected = state.engine.as_ref().is_some_and(EngineHandle::is_alive);
    let incoming_connections = state
        .engine
        .as_ref()
        .is_some_and(EngineHandle::peer_listener_healthy);
    Ok(json!({
        "connected": connected,
        "torrents": torrents,
        "filters": deluge_filters_from_entries(snapshot.iter(), &active_rechecks),
        "stats": {
            "download_rate": runtime_stats.as_ref().map(|stats| stats.download_rate).unwrap_or(0),
            "upload_rate": runtime_stats.as_ref().map(|stats| stats.upload_rate).unwrap_or(0),
            "num_connections": runtime_stats.as_ref().map(|stats| stats.connected_peers).unwrap_or(0),
            "dht_nodes": runtime_stats.as_ref().map(|stats| stats.dht_routing_nodes).unwrap_or(0),
            "has_incoming_connections": incoming_connections,
            "free_space": free_space,
        }
    }))
}

fn deluge_filters_from_entries<'a>(
    entries: impl IntoIterator<Item = &'a rt_session::TorrentEntry>,
    active_rechecks: &HashSet<String>,
) -> Value {
    let mut states = std::collections::BTreeMap::<String, usize>::new();
    let mut labels = std::collections::BTreeMap::<String, usize>::new();
    let mut torrent_count = 0;
    for entry in entries {
        torrent_count += 1;
        *states
            .entry(deluge_state_with_recheck(
                entry.state.as_str(),
                active_rechecks.contains(&entry.info_hash),
            ))
            .or_default() += 1;
        if let Some(label) = &entry.category {
            *labels.entry(label.clone()).or_default() += 1;
        }
    }
    let mut state_filters = vec![json!(["All", torrent_count])];
    state_filters.extend(
        states
            .into_iter()
            .map(|(state, count)| json!([state, count]))
            .collect::<Vec<_>>(),
    );
    json!({
        "state": state_filters,
        "label": labels
            .into_iter()
            .map(|(label, count)| json!([label, count]))
            .collect::<Vec<_>>(),
    })
}

async fn labels(state: &AppState) -> Result<Value, String> {
    if let Some(engine) = &state.engine {
        return Ok(json!(engine
            .list_categories()
            .await?
            .into_iter()
            .map(|category| category.name)
            .collect::<Vec<_>>()));
    }
    let reg = state.registry.read().await;
    let snapshot = reg.snapshot();
    Ok(json!(snapshot
        .iter()
        .filter_map(|entry| entry.category.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()))
}

async fn add_label(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let name = params
        .first()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "missing label name".to_owned())?;
    if params
        .get(1)
        .and_then(Value::as_object)
        .is_some_and(|options| !options.is_empty())
    {
        return Err(
            "unsupported Deluge method: Label options have no native engine equivalent".to_owned(),
        );
    }
    deluge_engine(state)?
        .create_category(name.to_owned(), None)
        .await?;
    Ok(json!(true))
}

async fn remove_label(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let name = params
        .first()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "missing label name".to_owned())?;
    deluge_engine(state)?
        .remove_categories(vec![name.to_owned()])
        .await?;
    Ok(json!(true))
}

async fn set_label(state: &AppState, hash: &str, label: &str) -> Result<(), String> {
    let hash = canonical_torrent_hash(state, hash).await?;
    let label = label.trim();
    let category = if label.is_empty() {
        None
    } else {
        Some(label.to_owned())
    };
    deluge_engine(state)?
        .update_torrent_labels(hash, Some(category), Vec::new(), Vec::new())
        .await?;
    Ok(())
}

async fn torrents_status(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let filter = params.first();
    validate_deluge_status_filter(filter)?;
    let wanted_fields = deluge_requested_fields(params.get(1))?;
    let snapshot = {
        let reg = state.registry.read().await;
        reg.snapshot()
    };
    let active_rechecks = if let Some(engine) = &state.engine {
        deluge_active_recheck_hashes(engine).await?
    } else {
        HashSet::new()
    };
    let entries = snapshot
        .iter()
        .filter(|entry| {
            deluge_torrent_matches_filter(entry, filter, active_rechecks.contains(&entry.info_hash))
        })
        .collect::<Vec<_>>();
    ensure_legacy_full_list_bound(entries.len(), "Deluge core.get_torrents_status")?;
    let _lease = if state.engine.is_some() {
        Some(
            reserve_deluge_api_snapshot(
                state,
                estimate_deluge_torrents_snapshot_bytes(entries.len()),
            )
            .await?
            .ok_or_else(|| "api snapshot memory budget exhausted".to_owned())?,
        )
    } else {
        None
    };
    let mut metadata = std::collections::HashMap::new();
    let mut peers = std::collections::HashMap::new();
    let mut trackers = std::collections::HashMap::new();
    let mut limits_by_hash = state.torrent_options.read().await.clone();
    let move_completed_by_hash = state.move_completed_options.read().await.clone();
    let need_metadata = deluge_fields_need_metadata(&wanted_fields);
    let need_limits = deluge_fields_need_limits(&wanted_fields);
    if let Some(engine) = &state.engine {
        let hashes = entries
            .iter()
            .map(|entry| entry.info_hash.clone())
            .collect::<Vec<_>>();
        let projections = load_deluge_runtime_projections(
            engine,
            &hashes,
            need_metadata,
            deluge_fields_need_peers(&wanted_fields),
            need_limits,
            deluge_fields_need_trackers(&wanted_fields),
        )
        .await?;
        merge_deluge_runtime_projections(
            projections,
            &mut metadata,
            &mut peers,
            &mut trackers,
            &mut limits_by_hash,
        );
    }
    let torrents = entries
        .iter()
        .map(|entry| {
            (
                entry.info_hash.clone(),
                filter_deluge_torrent_fields(
                    deluge_torrent(
                        entry,
                        metadata.get(&entry.info_hash),
                        peers.get(&entry.info_hash).map(Vec::as_slice),
                        trackers.get(&entry.info_hash).map(Vec::as_slice),
                        limits_by_hash.get(&entry.info_hash),
                        move_completed_by_hash.get(&entry.info_hash),
                        active_rechecks.contains(&entry.info_hash),
                    ),
                    &wanted_fields,
                ),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Ok(Value::Object(torrents))
}

fn ensure_legacy_full_list_bound(count: usize, endpoint: &str) -> Result<(), String> {
    if count > MAX_LEGACY_FULL_LIST_ENTRIES {
        return Err(format!(
            "{endpoint} full-list response has {count} torrents; maximum is {MAX_LEGACY_FULL_LIST_ENTRIES}; use the native paged API"
        ));
    }
    Ok(())
}

fn deluge_torrent_matches_filter(
    entry: &rt_session::TorrentEntry,
    filter: Option<&Value>,
    active_recheck: bool,
) -> bool {
    let Some(filter) = filter.and_then(Value::as_object) else {
        return true;
    };
    for (key, value) in filter {
        match key.as_str() {
            "id" | "ids" | "hash" | "hashes" => {
                let values = string_list(Some(value));
                if !values.is_empty()
                    && !values
                        .iter()
                        .any(|hash| hash.eq_ignore_ascii_case(&entry.info_hash))
                {
                    return false;
                }
            }
            "label" => {
                let values = string_list(Some(value));
                if !values.is_empty()
                    && !values
                        .iter()
                        .any(|label| entry.category.as_deref().unwrap_or_default() == label)
                {
                    return false;
                }
            }
            "state" => {
                let values = string_list(Some(value));
                if !values.is_empty()
                    && !values.iter().any(|state| {
                        deluge_state_with_recheck(entry.state.as_str(), active_recheck)
                            .eq_ignore_ascii_case(state)
                    })
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

fn validate_deluge_status_filter(value: Option<&Value>) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let Some(filter) = value.as_object() else {
        return Err("Deluge torrent status filter must be an object".to_owned());
    };
    for (key, value) in filter {
        if !matches!(
            key.as_str(),
            "id" | "ids" | "hash" | "hashes" | "label" | "state"
        ) {
            continue;
        }
        let values = value
            .as_array()
            .ok_or_else(|| format!("Deluge status filter {key} must be an array"))?;
        for (index, item) in values.iter().enumerate() {
            if item
                .as_str()
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .is_none()
            {
                return Err(format!(
                    "Deluge status filter {key}[{index}] must be a non-empty string"
                ));
            }
        }
    }
    Ok(())
}

async fn torrent_status(
    state: &AppState,
    hash: &str,
    fields: Option<&Value>,
) -> Result<Value, String> {
    let entry = {
        let reg = state.registry.read().await;
        reg.get(hash)
            .ok_or_else(|| format!("torrent {hash} not found"))?
    };
    let hash = entry.info_hash.clone();
    let _lease = if state.engine.is_some() {
        Some(
            reserve_deluge_api_snapshot(state, estimate_deluge_torrent_detail_snapshot_bytes())
                .await?
                .ok_or_else(|| "api snapshot memory budget exhausted".to_owned())?,
        )
    } else {
        None
    };
    let meta = if let Some(engine) = &state.engine {
        Some(engine.torrent_metadata(hash.clone()).await?)
    } else {
        None
    };
    let peers = if let Some(engine) = &state.engine {
        Some(engine.torrent_peers(hash.clone()).await?)
    } else {
        None
    };
    let trackers = if let Some(engine) = &state.engine {
        Some(engine.torrent_trackers(hash.clone()).await?)
    } else {
        None
    };
    let active_recheck = if let Some(engine) = &state.engine {
        deluge_active_recheck_hashes(engine).await?.contains(&hash)
    } else {
        false
    };
    let limits = deluge_torrent_limits(state, &hash).await?;
    let move_completed = state
        .move_completed_options
        .read()
        .await
        .get(&hash)
        .cloned();
    let wanted_fields = deluge_requested_fields(fields)?;
    Ok(filter_deluge_torrent_fields(
        deluge_torrent(
            &entry,
            meta.as_ref(),
            peers.as_deref(),
            trackers.as_deref(),
            limits.as_ref(),
            move_completed.as_ref(),
            active_recheck,
        ),
        &wanted_fields,
    ))
}

fn deluge_requested_fields(
    value: Option<&Value>,
) -> Result<Option<std::collections::BTreeSet<String>>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(fields) = value.as_array() else {
        return Err("Deluge requested fields must be an array".to_owned());
    };
    if fields.is_empty() {
        return Ok(None);
    }
    let mut requested = std::collections::BTreeSet::new();
    for field in fields {
        let field = field
            .as_str()
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .ok_or_else(|| "Deluge requested fields must contain non-empty strings".to_owned())?;
        requested.insert(field.to_owned());
    }
    Ok(Some(requested))
}

fn filter_deluge_torrent_fields(
    torrent: Value,
    fields: &Option<std::collections::BTreeSet<String>>,
) -> Value {
    let Some(fields) = fields else {
        return torrent;
    };
    let Some(obj) = torrent.as_object() else {
        return torrent;
    };
    Value::Object(
        fields
            .iter()
            .filter_map(|field| obj.get(field).cloned().map(|value| (field.clone(), value)))
            .collect(),
    )
}

fn deluge_fields_need_peers(fields: &Option<std::collections::BTreeSet<String>>) -> bool {
    let Some(fields) = fields else {
        return true;
    };
    fields.iter().any(|field| {
        matches!(
            field.as_str(),
            "download_payload_rate"
                | "upload_payload_rate"
                | "eta"
                | "num_peers"
                | "num_seeds"
                | "total_peers"
                | "total_seeds"
                | "distributed_copies"
                | "seeds_peers_ratio"
        )
    })
}

fn deluge_fields_need_metadata(fields: &Option<std::collections::BTreeSet<String>>) -> bool {
    let Some(fields) = fields else {
        return true;
    };
    fields.iter().any(|field| {
        matches!(
            field.as_str(),
            "num_files"
                | "num_pieces"
                | "piece_length"
                | "private"
                | "comment"
                | "tracker"
                | "tracker_host"
                | "tracker_status"
                | "next_announce"
        )
    })
}

fn deluge_fields_need_limits(fields: &Option<std::collections::BTreeSet<String>>) -> bool {
    let Some(fields) = fields else {
        return true;
    };
    fields.iter().any(|field| {
        matches!(
            field.as_str(),
            "max_download_speed"
                | "max_upload_speed"
                | "is_auto_managed"
                | "stop_at_ratio"
                | "stop_ratio"
                | "prioritize_first_last"
                | "sequential_download"
                | "super_seeding"
        )
    })
}

fn deluge_fields_need_trackers(fields: &Option<std::collections::BTreeSet<String>>) -> bool {
    let Some(fields) = fields else {
        return true;
    };
    fields.iter().any(|field| {
        matches!(
            field.as_str(),
            "tracker" | "tracker_host" | "tracker_status" | "next_announce"
        )
    })
}

async fn deluge_active_recheck_hashes(engine: &EngineHandle) -> Result<HashSet<String>, String> {
    let jobs = engine.list_jobs().await?;
    Ok(jobs
        .into_iter()
        .filter(|job| {
            job.kind == "recheck_torrent"
                && !matches!(
                    job.state.as_str(),
                    "completed" | "failed" | "cancelled" | "canceled"
                )
        })
        .flat_map(|job| job.affected_torrents)
        .collect())
}

async fn torrent_files(state: &AppState, hash: &str) -> Result<Value, String> {
    let hash = canonical_torrent_hash(state, hash).await?;
    if let Some(engine) = &state.engine {
        let meta = engine.torrent_metadata(hash).await?;
        return Ok(json!(meta
            .files
            .into_iter()
            .map(|file| json!({
                "index": file.index,
                "path": file.path,
                "size": file.length,
                "offset": 0,
                "progress": 0.0,
                "priority": 1,
            }))
            .collect::<Vec<_>>()));
    }
    Ok(json!([]))
}

fn deluge_torrent(
    entry: &rt_session::TorrentEntry,
    meta: Option<&EngineTorrentMetadata>,
    peers: Option<&[EnginePeerSnapshot]>,
    trackers: Option<&[EngineTrackerSnapshot]>,
    limits: Option<&EngineTorrentLimits>,
    move_completed: Option<&DelugeMoveCompletedOptions>,
    active_recheck: bool,
) -> Value {
    let now = unix_now();
    let progress = if entry.total_length == 0 {
        0.0
    } else {
        entry.total_length.saturating_sub(entry.amount_left) as f64 * 100.0
            / entry.total_length as f64
    };
    let message = entry.error_message.clone().unwrap_or_default();
    let tracker_state = trackers.and_then(|trackers| trackers.first());
    let tracker = tracker_state
        .map(|tracker| tracker.announce.clone())
        .or_else(|| meta.and_then(|meta| meta.trackers.first()).cloned())
        .unwrap_or_default();
    let next_announce = tracker_state
        .and_then(|tracker| tracker.next_announce_at)
        .unwrap_or(0);
    let tracker_status = tracker_state
        .map(deluge_tracker_status)
        .unwrap_or_else(|| deluge_fallback_tracker_status(entry, &tracker));
    json!({
        "hash": entry.info_hash,
        "name": entry.name,
        "state": deluge_state_with_recheck(entry.state.as_str(), active_recheck),
        "progress": progress,
        "total_size": entry.total_length,
        "total_done": entry.total_length.saturating_sub(entry.amount_left),
        "download_payload_rate": deluge_peer_download_rate(peers),
        "upload_payload_rate": deluge_peer_upload_rate(peers),
        "ratio": entry.stats.ratio(),
        "save_path": entry.save_path,
        "label": entry.category.clone().unwrap_or_default(),
        "tags": entry.tags,
        "is_finished": entry.completed_at.is_some(),
        "eta": deluge_eta(entry.amount_left, deluge_peer_download_rate(peers)),
        "num_peers": deluge_leecher_count(peers),
        "num_seeds": deluge_seed_count(peers),
        "total_peers": peers.map(|peers| peers.len()).unwrap_or(0),
        "total_seeds": deluge_seed_count(peers),
        "num_files": meta.map(|meta| meta.files.len()).unwrap_or(0),
        "num_pieces": meta.map(|meta| meta.piece_count).unwrap_or(0),
        "piece_length": meta.map(|meta| meta.piece_length).unwrap_or(0),
        "distributed_copies": deluge_distributed_copies(peers),
        "seeds_peers_ratio": deluge_seeds_peers_ratio(peers),
        "max_download_speed": limits.and_then(|limits| limits.download_limit).map(bytes_to_deluge_kib).unwrap_or(-1.0),
        "max_upload_speed": limits.and_then(|limits| limits.upload_limit).map(bytes_to_deluge_kib).unwrap_or(-1.0),
        "is_auto_managed": limits.map(|limits| limits.auto_management).unwrap_or(false),
        "stop_at_ratio": limits.and_then(|limits| limits.seed_ratio_limit).is_some(),
        "stop_ratio": limits.and_then(|limits| limits.seed_ratio_limit).unwrap_or(0.0),
        "remove_at_ratio": false,
        "prioritize_first_last": limits.map(|limits| limits.first_last_piece_prio).unwrap_or(false),
        "sequential_download": limits.map(|limits| limits.sequential_download).unwrap_or(false),
        "super_seeding": limits.map(|limits| limits.super_seeding).unwrap_or(false),
        "move_on_completed": move_completed.map(|options| options.enabled).unwrap_or(false),
        "move_on_completed_path": move_completed.map(|options| options.path.as_str()).unwrap_or(""),
        "time_added": entry.added_at,
        "completed_time": entry.completed_at.unwrap_or(0),
        "active_time": now.saturating_sub(deluge_i64(entry.added_at)),
        "seeding_time": entry
            .completed_at
            .map(|completed| now.saturating_sub(deluge_i64(completed)))
            .unwrap_or(0),
        "finished_time": entry.completed_at.unwrap_or(0),
        "all_time_download": entry.stats.downloaded,
        "total_uploaded": entry.stats.uploaded,
        "total_payload_upload": entry.stats.uploaded,
        "total_payload_download": entry.stats.downloaded,
        "next_announce": next_announce,
        "private": meta.map(|meta| meta.is_private).unwrap_or(false),
        "owner": "localclient",
        "shared": false,
        "tracker_host": tracker_host(&tracker),
        "tracker_status": tracker_status,
        "tracker": tracker,
        "comment": meta
            .and_then(|meta| meta.comment.as_deref())
            .unwrap_or(""),
        "message": message,
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

fn deluge_tracker_status(tracker: &EngineTrackerSnapshot) -> String {
    if let Some(reason) = tracker
        .failure_reason
        .as_deref()
        .filter(|reason| !reason.is_empty())
    {
        return format!("Error: {reason}");
    }
    if let Some(warning) = tracker
        .warning_message
        .as_deref()
        .filter(|warning| !warning.is_empty())
    {
        return format!("Warning: {warning}");
    }
    match tracker.status.as_str() {
        "working" => "Announce OK".to_owned(),
        "error" => "Error".to_owned(),
        "warning" => "Warning".to_owned(),
        "pending" => "Announce pending".to_owned(),
        "" => String::new(),
        status => status.to_owned(),
    }
}

fn deluge_fallback_tracker_status(entry: &rt_session::TorrentEntry, tracker: &str) -> String {
    let message = entry.error_message.clone().unwrap_or_default();
    if entry.state.as_str() == "error" && !message.is_empty() {
        format!("Error: {message}")
    } else if tracker.is_empty() {
        String::new()
    } else {
        "Announce OK".to_owned()
    }
}

fn deluge_peer_download_rate(peers: Option<&[EnginePeerSnapshot]>) -> i64 {
    peers
        .map(|peers| {
            peers
                .iter()
                .fold(0_i64, |sum, peer| sum.saturating_add(peer.download_rate))
        })
        .unwrap_or(0)
}

fn deluge_peer_upload_rate(peers: Option<&[EnginePeerSnapshot]>) -> i64 {
    peers
        .map(|peers| {
            peers
                .iter()
                .fold(0_i64, |sum, peer| sum.saturating_add(peer.upload_rate))
        })
        .unwrap_or(0)
}

fn deluge_seed_count(peers: Option<&[EnginePeerSnapshot]>) -> usize {
    peers
        .map(|peers| peers.iter().filter(|peer| peer.progress >= 1.0).count())
        .unwrap_or(0)
}

fn deluge_leecher_count(peers: Option<&[EnginePeerSnapshot]>) -> usize {
    peers
        .map(|peers| peers.iter().filter(|peer| peer.progress < 1.0).count())
        .unwrap_or(0)
}

fn deluge_eta(amount_left: u64, download_rate: i64) -> i64 {
    if amount_left == 0 {
        0
    } else if download_rate > 0 {
        i64::try_from(amount_left / download_rate as u64).unwrap_or(i64::MAX)
    } else {
        -1
    }
}

fn deluge_distributed_copies(peers: Option<&[EnginePeerSnapshot]>) -> f64 {
    let Some(peers) = peers else {
        return 0.0;
    };
    peers
        .iter()
        .map(|peer| peer.progress.clamp(0.0, 1.0))
        .fold(0.0_f64, f64::max)
}

fn deluge_seeds_peers_ratio(peers: Option<&[EnginePeerSnapshot]>) -> f64 {
    let seeds = deluge_seed_count(peers);
    let leechers = deluge_leecher_count(peers);
    if leechers == 0 {
        seeds as f64
    } else {
        seeds as f64 / leechers as f64
    }
}

async fn add_magnet(state: &AppState, uri: &str, options: Option<&Value>) -> Result<Value, String> {
    let magnet = parse_magnet(uri).map_err(|e| e.to_string())?;
    let engine = deluge_engine(state)?;
    let save_path = options
        .and_then(|value| value.get("download_location"))
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    let hash = engine
        .add_magnet_with_labels(magnet, save_path, false, None, Vec::new())
        .await?;
    Ok(json!(hash))
}

async fn add_torrent_file(
    state: &AppState,
    data: &str,
    options: Option<&Value>,
) -> Result<Value, String> {
    let raw = decode_deluge_torrent_data(data)?;
    let engine = deluge_engine(state)?;
    let save_path = options
        .and_then(|value| value.get("download_location"))
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    let hash = engine
        .add_torrent_raw_with_labels(raw, save_path, false, None, Vec::new())
        .await?;
    Ok(json!(hash))
}

async fn add_torrent_path(
    _state: &AppState,
    _path: &str,
    _options: Option<&Value>,
) -> Result<Value, String> {
    Err("path-based torrent loads are unsupported at this API boundary; use embedded base64 metainfo or a magnet URI".to_owned())
}

fn decode_deluge_torrent_data(data: &str) -> Result<Vec<u8>, String> {
    let payload = data
        .split_once(',')
        .filter(|(prefix, _)| prefix.contains(";base64"))
        .map(|(_, payload)| payload)
        .unwrap_or(data)
        .trim();
    general_purpose::STANDARD
        .decode(payload)
        .or_else(|_| general_purpose::URL_SAFE.decode(payload))
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(payload))
        .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(payload))
        .map_err(|e| e.to_string())
}

async fn set_torrent_options(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let hashes = canonical_hashes_from_param(state, params.first()).await?;
    let Some(options) = params.get(1).and_then(Value::as_object) else {
        return Err("missing torrent options".to_owned());
    };
    if !hashes.is_empty() && state.engine.is_none() && !options.is_empty() {
        return Err("native engine is unavailable; torrent options were not applied".to_owned());
    }
    if state.engine.is_some()
        && [
            "auto_managed",
            "move_completed",
            "move_on_completed",
            "move_completed_path",
            "move_on_completed_path",
        ]
        .iter()
        .any(|key| options.contains_key(*key))
    {
        return Err(
            "auto_managed and move-on-completion options are not runtime-supported".to_owned(),
        );
    }
    validate_deluge_options(options)?;
    for hash in hashes {
        let mut limits = if let Some(engine) = &state.engine {
            engine.torrent_limits(hash.clone()).await?
        } else {
            state
                .torrent_options
                .read()
                .await
                .get(&hash)
                .cloned()
                .unwrap_or_default()
        };
        apply_deluge_options(&mut limits, options);
        if let Some(engine) = &state.engine {
            engine
                .update_torrent_limits(hash.clone(), limits.clone())
                .await?;
        }
        if let Some(move_completed) = deluge_move_completed_options(options) {
            state
                .move_completed_options
                .write()
                .await
                .insert(hash.clone(), move_completed);
        }
        state
            .torrent_options
            .write()
            .await
            .insert(hash.clone(), limits.clone());
    }
    Ok(json!(true))
}

fn deluge_move_completed_options(
    options: &serde_json::Map<String, Value>,
) -> Option<DelugeMoveCompletedOptions> {
    let enabled = options
        .get("move_completed")
        .or_else(|| options.get("move_on_completed"))
        .and_then(|value| deluge_bool(Some(value)));
    let path = options
        .get("move_completed_path")
        .or_else(|| options.get("move_on_completed_path"))
        .and_then(Value::as_str);
    if enabled.is_none() && path.is_none() {
        return None;
    }
    Some(DelugeMoveCompletedOptions {
        enabled: enabled.unwrap_or(false),
        path: path.unwrap_or_default().to_owned(),
    })
}

async fn set_prioritize_first_last(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let hashes = canonical_hashes_from_param(state, params.first()).await?;
    let Some(enabled) = deluge_bool(params.get(1)) else {
        return Err("missing prioritize-first-last value".to_owned());
    };
    if hashes.is_empty() {
        return Ok(json!(true));
    }
    let engine = deluge_engine(state)?;
    for hash in hashes {
        let mut limits = engine.torrent_limits(hash.clone()).await?;
        limits.first_last_piece_prio = enabled;
        engine
            .update_torrent_limits(hash.clone(), limits.clone())
            .await?;
        state.torrent_options.write().await.insert(hash, limits);
    }
    Ok(json!(true))
}

async fn set_file_priorities(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let hash = params
        .first()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| "missing torrent id".to_owned())?;
    let hash = canonical_torrent_hash(state, hash).await?;
    let updates = deluge_file_priority_updates(params.get(1), params.get(2))?;
    if updates.is_empty() {
        return Ok(json!(true));
    }
    let engine = deluge_engine(state)?;
    for (file_ids, priority) in updates {
        engine
            .update_file_priorities(hash.to_owned(), file_ids, priority)
            .await?;
    }
    Ok(json!(true))
}

async fn set_trackers(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let hash = params
        .first()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| "missing torrent id".to_owned())?;
    let hash = canonical_torrent_hash(state, hash).await?;
    let trackers = deluge_trackers_arg(params.get(1))?;
    let engine = deluge_engine(state)?;
    engine
        .update_torrent_trackers(hash.to_owned(), trackers)
        .await?;
    Ok(json!(true))
}

async fn connect_peer(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let hash = params
        .first()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| "missing torrent id".to_owned())?;
    let hash = canonical_torrent_hash(state, hash).await?;
    let peer = if params.len() >= 3 {
        deluge_peer_host_port(params.get(1), params.get(2))?
    } else {
        deluge_peer_addr_arg(
            params
                .get(1)
                .ok_or_else(|| "missing peer address".to_owned())?,
        )?
    };
    let engine = deluge_engine(state)?;
    engine.add_peers(hash.to_owned(), vec![peer]).await?;
    Ok(json!(true))
}

async fn rename_files(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let hash = params
        .first()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| "missing torrent id".to_owned())?;
    let hash = canonical_torrent_hash(state, hash).await?;
    let renames = deluge_rename_file_args(params.get(1))?;
    let engine = deluge_engine(state)?;
    for (file_id, new_path) in renames {
        engine
            .rename_file_path(hash.to_owned(), file_id, new_path)
            .await?;
    }
    Ok(json!(true))
}

async fn rename_folder(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let hash = params
        .first()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| "missing torrent id".to_owned())?;
    let hash = canonical_torrent_hash(state, hash).await?;
    let old_path = params
        .get(1)
        .and_then(Value::as_str)
        .ok_or_else(|| "missing old folder path".to_owned())?;
    let new_path = params
        .get(2)
        .and_then(Value::as_str)
        .ok_or_else(|| "missing new folder path".to_owned())?;
    let engine = deluge_engine(state)?;
    engine
        .rename_folder_path(hash.to_owned(), old_path.to_owned(), new_path.to_owned())
        .await?;
    Ok(json!(true))
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn strict_string_list(value: Option<&Value>, field: &str) -> Result<Vec<String>, String> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{field} must be an array of non-empty strings"))?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{field}[{index}] must be a non-empty string"))
        })
        .collect()
}

#[cfg(test)]
fn hashes_from_param(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(hash)) if !hash.trim().is_empty() => vec![hash.trim().to_owned()],
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|hash| !hash.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn strict_hashes_from_param(value: Option<&Value>) -> Result<Vec<String>, String> {
    match value {
        Some(Value::String(hash)) => {
            let hash = hash.trim();
            if hash.is_empty() {
                Err("torrent id must not be empty".to_owned())
            } else {
                Ok(vec![hash.to_owned()])
            }
        }
        Some(Value::Array(values)) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .as_str()
                    .map(str::trim)
                    .filter(|hash| !hash.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| format!("torrent ids[{index}] must be a non-empty string"))
            })
            .collect(),
        Some(_) => Err("torrent id must be a string or an array of strings".to_owned()),
        None => Err("missing torrent id".to_owned()),
    }
}

async fn canonical_torrent_hash(state: &AppState, hash: &str) -> Result<String, String> {
    let hash = hash.trim();
    if hash.is_empty() {
        return Err("torrent id must not be empty".to_owned());
    }
    let reg = state.registry.read().await;
    reg.get(hash)
        .map(|entry| entry.info_hash.clone())
        .ok_or_else(|| format!("torrent {hash} was not found"))
}

async fn canonical_torrent_hashes(
    state: &AppState,
    value: Option<&Value>,
    field: &str,
) -> Result<Vec<String>, String> {
    let hashes = strict_string_list(value, field)?;
    let reg = state.registry.read().await;
    hashes
        .into_iter()
        .map(|hash| {
            reg.get(&hash)
                .map(|entry| entry.info_hash.clone())
                .ok_or_else(|| format!("torrent {hash} was not found"))
        })
        .collect()
}

async fn canonical_hashes_from_param(
    state: &AppState,
    value: Option<&Value>,
) -> Result<Vec<String>, String> {
    let hashes = strict_hashes_from_param(value)?;
    let reg = state.registry.read().await;
    hashes
        .into_iter()
        .map(|hash| {
            reg.get(&hash)
                .map(|entry| entry.info_hash.clone())
                .ok_or_else(|| format!("torrent {hash} was not found"))
        })
        .collect()
}

fn apply_deluge_options(
    limits: &mut EngineTorrentLimits,
    options: &serde_json::Map<String, Value>,
) {
    if let Some(value) = options
        .get("prioritize_first_last")
        .and_then(|value| deluge_bool(Some(value)))
    {
        limits.first_last_piece_prio = value;
    }
    if let Some(value) = options
        .get("sequential_download")
        .and_then(|value| deluge_bool(Some(value)))
    {
        limits.sequential_download = value;
    }
    if let Some(value) = options
        .get("super_seeding")
        .and_then(|value| deluge_bool(Some(value)))
    {
        limits.super_seeding = value;
    }
    if let Some(value) = options
        .get("max_download_speed")
        .and_then(|value| deluge_speed_limit(Some(value)))
    {
        limits.download_limit = value;
    }
    if let Some(value) = options
        .get("max_upload_speed")
        .and_then(|value| deluge_speed_limit(Some(value)))
    {
        limits.upload_limit = value;
    }
    if matches!(
        options
            .get("stop_at_ratio")
            .and_then(|value| deluge_bool(Some(value))),
        Some(false)
    ) {
        limits.seed_ratio_limit = None;
    } else if let Some(value) = options.get("stop_ratio").and_then(Value::as_f64) {
        limits.seed_ratio_limit = Some(value);
    }
}

async fn deluge_torrent_limits(
    state: &AppState,
    hash: &str,
) -> Result<Option<EngineTorrentLimits>, String> {
    if let Some(engine) = &state.engine {
        return engine
            .torrent_limits(hash.to_owned())
            .await
            .map(Some)
            .map_err(|error| error.to_string());
    }
    Ok(state.torrent_options.read().await.get(hash).cloned())
}

fn bytes_to_deluge_kib(value: i64) -> f64 {
    value.max(0) as f64 / 1024.0
}

fn unix_now() -> i64 {
    deluge_i64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
}

fn deluge_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn deluge_bool(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Number(value)) => value.as_i64().map(|value| value != 0),
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn validate_deluge_options(options: &serde_json::Map<String, Value>) -> Result<(), String> {
    const BOOL_OPTIONS: &[&str] = &[
        "prioritize_first_last",
        "sequential_download",
        "super_seeding",
        "stop_at_ratio",
    ];
    const SUPPORTED_OPTIONS: &[&str] = &[
        "prioritize_first_last",
        "sequential_download",
        "super_seeding",
        "max_download_speed",
        "max_upload_speed",
        "stop_at_ratio",
        "stop_ratio",
    ];

    for key in BOOL_OPTIONS {
        if options.contains_key(*key) && deluge_bool(options.get(*key)).is_none() {
            return Err(format!("Deluge option {key} must be a boolean"));
        }
    }
    for key in ["max_download_speed", "max_upload_speed"] {
        if options.contains_key(key) && deluge_speed_limit(options.get(key)).is_none() {
            return Err(format!(
                "Deluge option {key} must be a finite speed in KiB/s within the supported range"
            ));
        }
    }
    if options.contains_key("stop_ratio")
        && options
            .get("stop_ratio")
            .and_then(Value::as_f64)
            .filter(|ratio| ratio.is_finite() && *ratio >= 0.0)
            .is_none()
    {
        return Err("Deluge option stop_ratio must be a finite non-negative number".to_owned());
    }
    if let Some(key) = options
        .keys()
        .find(|key| !SUPPORTED_OPTIONS.contains(&key.as_str()))
    {
        return Err(format!(
            "unsupported Deluge torrent option {key}; it was not applied"
        ));
    }
    Ok(())
}

fn deluge_speed_limit(value: Option<&Value>) -> Option<Option<i64>> {
    let kib = match value {
        Some(Value::Number(value)) => value.as_f64()?,
        Some(Value::String(value)) => value.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !kib.is_finite() {
        return None;
    }
    if kib <= 0.0 {
        Some(None)
    } else if kib > i64::MAX as f64 / 1024.0 {
        None
    } else {
        Some(Some((kib * 1024.0).round() as i64))
    }
}

fn deluge_file_priority_updates(
    ids_or_priorities: Option<&Value>,
    priority: Option<&Value>,
) -> Result<Vec<(Vec<u32>, i64)>, String> {
    if let Some(priority) = priority {
        let priority = priority
            .as_i64()
            .ok_or_else(|| "file priority must be an integer".to_owned())?;
        let ids = deluge_file_ids(ids_or_priorities)?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        return Ok(vec![(ids, deluge_file_priority(priority))]);
    }
    let Some(priorities) = ids_or_priorities.and_then(Value::as_array) else {
        return Err("file priorities must be an array".to_owned());
    };
    let mut skipped = Vec::new();
    let mut normal = Vec::new();
    let mut high = Vec::new();
    for (idx, value) in priorities.iter().enumerate() {
        let priority = value
            .as_i64()
            .ok_or_else(|| format!("file priority at index {idx} must be an integer"))?;
        let idx = u32::try_from(idx).map_err(|_| "too many file priorities".to_owned())?;
        match deluge_file_priority(priority) {
            0 => skipped.push(idx),
            2 => high.push(idx),
            _ => normal.push(idx),
        }
    }
    let mut updates = Vec::new();
    if !skipped.is_empty() {
        updates.push((skipped, 0));
    }
    if !normal.is_empty() {
        updates.push((normal, 1));
    }
    if !high.is_empty() {
        updates.push((high, 2));
    }
    Ok(updates)
}

fn deluge_file_ids(value: Option<&Value>) -> Result<Vec<u32>, String> {
    let Some(values) = value.and_then(Value::as_array) else {
        return Err("file ids must be an array".to_owned());
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let value = value.as_u64().ok_or_else(|| {
                format!("file id at index {index} must be a non-negative integer")
            })?;
            u32::try_from(value)
                .map_err(|_| format!("file id at index {index} exceeds the supported range"))
        })
        .collect()
}

fn deluge_file_priority(priority: i64) -> i64 {
    if priority <= 0 {
        0
    } else if priority >= 5 {
        2
    } else {
        1
    }
}

fn deluge_trackers_arg(value: Option<&Value>) -> Result<Vec<String>, String> {
    let value = value.ok_or_else(|| "missing tracker list".to_owned())?;
    let mut trackers = Vec::new();
    collect_deluge_trackers(value, &mut trackers)?;
    Ok(normalize_deluge_trackers(trackers))
}

fn collect_deluge_trackers(value: &Value, out: &mut Vec<String>) -> Result<(), String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => out.push(value.to_owned()),
        Value::String(_) => return Err("tracker URL must not be empty".to_owned()),
        Value::Array(values) => {
            for value in values {
                collect_deluge_trackers(value, out)?;
            }
        }
        Value::Object(obj) => {
            let Some(url) = obj
                .get("url")
                .or_else(|| obj.get("announce"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|url| !url.is_empty())
            else {
                return Err("tracker entry must contain a non-empty url".to_owned());
            };
            out.push(url.to_owned());
        }
        _ => return Err("tracker list contains an invalid entry".to_owned()),
    }
    Ok(())
}

fn normalize_deluge_trackers(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !out.iter().any(|existing| existing == value) {
            out.push(value.to_owned());
        }
    }
    out
}

fn deluge_peer_addr_arg(value: &Value) -> Result<SocketAddr, String> {
    match value {
        Value::String(value) => value
            .trim()
            .parse()
            .map_err(|_| "peer address must be a valid socket address".to_owned()),
        Value::Array(values) => deluge_peer_host_port(values.first(), values.get(1)),
        Value::Object(obj) => {
            deluge_peer_host_port(obj.get("ip").or_else(|| obj.get("host")), obj.get("port"))
        }
        _ => Err("peer address must be a socket address, host/port array, or object".to_owned()),
    }
}

fn deluge_peer_host_port(host: Option<&Value>, port: Option<&Value>) -> Result<SocketAddr, String> {
    let host = host
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "peer host is required".to_owned())?;
    let port = match port {
        Some(Value::Number(value)) => value
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| "peer port must be between 0 and 65535".to_owned())?,
        Some(Value::String(value)) => value
            .trim()
            .parse()
            .map_err(|_| "peer port must be between 0 and 65535".to_owned())?,
        _ => return Err("peer port is required".to_owned()),
    };
    format!("{host}:{port}")
        .parse()
        .map_err(|_| "peer host and port do not form a valid socket address".to_owned())
}

fn deluge_rename_file_args(value: Option<&Value>) -> Result<Vec<(u32, String)>, String> {
    let Some(values) = value.and_then(Value::as_array) else {
        return Err("file renames must be an array".to_owned());
    };
    values.iter().map(deluge_rename_file_arg).collect()
}

fn deluge_rename_file_arg(value: &Value) -> Result<(u32, String), String> {
    match value {
        Value::Array(values) => {
            let id = values
                .first()
                .and_then(Value::as_u64)
                .ok_or_else(|| "file rename id must be a non-negative integer".to_owned())
                .and_then(|id| {
                    u32::try_from(id)
                        .map_err(|_| "file rename id exceeds the supported range".to_owned())
                })?;
            let path = values
                .get(1)
                .and_then(Value::as_str)
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| "file rename path is required".to_owned())?
                .to_owned();
            Ok((id, path))
        }
        Value::Object(obj) => {
            let id_value = obj
                .get("index")
                .or_else(|| obj.get("id"))
                .or_else(|| obj.get("file_id"))
                .ok_or_else(|| "file rename id is required".to_owned())?;
            let id = id_value
                .as_u64()
                .ok_or_else(|| "file rename id must be a non-negative integer".to_owned())
                .and_then(|id| {
                    u32::try_from(id)
                        .map_err(|_| "file rename id exceeds the supported range".to_owned())
                })?;
            let path_value = obj
                .get("path")
                .or_else(|| obj.get("name"))
                .or_else(|| obj.get("new_path"))
                .ok_or_else(|| "file rename path is required".to_owned())?;
            let path = path_value
                .as_str()
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| "file rename path is required".to_owned())?
                .to_owned();
            Ok((id, path))
        }
        _ => Err("file rename must be an array or object".to_owned()),
    }
}

fn deluge_state(state: &str) -> &'static str {
    match state {
        "seeding" => "Seeding",
        "downloading" | "metadata_pending" => "Downloading",
        "checking" => "Checking",
        "paused" | "stopped" => "Paused",
        "error" => "Error",
        _ => "Queued",
    }
}

fn deluge_state_with_recheck(state: &str, active_recheck: bool) -> String {
    if active_recheck {
        "Checking".to_owned()
    } else {
        deluge_state(state).to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use rt_session::TorrentEntry;
    use tower::ServiceExt;

    #[tokio::test]
    async fn deluge_router_enforces_configured_token_and_preserves_login_body() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut state = AppState::new(Arc::clone(&registry));
        state.api_tokens = Arc::new(vec!["secret".to_owned()]);
        let app = build_deluge_router(state);

        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"id":1,"method":"daemon.info","params":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let login = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":1,"method":"auth.login","params":["secret"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let body = axum::body::to_bytes(login.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["result"], true);
    }

    #[tokio::test]
    async fn deluge_shutdown_notifies_daemon() {
        let notify = Arc::new(Notify::new());
        let mut state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
        state.shutdown = Some(Arc::clone(&notify));
        let notified = notify.notified();

        assert_eq!(
            dispatch(&state, "daemon.shutdown", &[]).await.unwrap(),
            true
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), notified)
            .await
            .expect("daemon shutdown notification was lost");
    }

    #[tokio::test]
    async fn deluge_idempotency_key_replays_mutation_and_rejects_reuse() {
        let app = build_deluge_router(AppState::new(Arc::new(RwLock::new(SessionRegistry::new()))));
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/json")
                .header("content-type", "application/json")
                .header("idempotency-key", "deluge-session-1")
                .body(Body::from(
                    r#"{"id":1,"method":"web.download_torrent_from_url","params":["magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]}"#,
                ))
                .unwrap()
        };
        let first = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let replay = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(
            replay.headers().get("idempotency-replayed").unwrap(),
            "true"
        );

        let conflict = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .header("idempotency-key", "deluge-session-1")
                    .body(Body::from(
                        r#"{"id":2,"method":"web.download_torrent_from_url","params":["magnet:?xt=urn:btih-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn deluge_update_ui_projects_registry() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data".into());
            entry.total_length = 100;
            entry.amount_left = 25;
            entry.category = Some("movies".into());
            reg.add(entry).unwrap();
        }
        let app = build_deluge_router(AppState::new(registry));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":1,"method":"web.update_ui","params":[[],{}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["error"].is_null());
        assert_eq!(
            body["result"]["torrents"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]["name"],
            "alpha"
        );
        assert_eq!(
            body["result"]["torrents"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]["progress"],
            75.0
        );
        assert_eq!(body["result"]["filters"]["state"][0], json!(["All", 1]));
        assert_eq!(body["result"]["filters"]["state"][1], json!(["Paused", 1]));
        assert_eq!(body["result"]["filters"]["label"][0], json!(["movies", 1]));
        assert_json_keys(
            &body["result"]["stats"],
            &[
                "download_rate",
                "upload_rate",
                "num_connections",
                "dht_nodes",
                "has_incoming_connections",
                "free_space",
            ],
        );
    }

    #[test]
    fn deluge_state_projects_active_recheck_as_checking() {
        assert_eq!(deluge_state_with_recheck("downloading", true), "Checking");
        assert_eq!(deluge_state_with_recheck("seeding", false), "Seeding");
    }

    #[tokio::test]
    async fn deluge_update_ui_honors_requested_fields() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("b".repeat(40), "bravo".into(), "/data".into());
            entry.total_length = 100;
            entry.amount_left = 10;
            reg.add(entry).unwrap();
        }
        let app = build_deluge_router(AppState::new(registry));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":1,"method":"web.update_ui","params":[["name","progress"],{}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let torrent = &body["result"]["torrents"]["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"];
        assert_eq!(torrent["name"], "bravo");
        assert_eq!(torrent["progress"], 90.0);
        assert_eq!(torrent.as_object().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn deluge_torrent_status_field_matrix_is_present() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data".into());
            entry.total_length = 100;
            entry.amount_left = 25;
            entry.added_at = 100;
            entry.completed_at = Some(200);
            entry.category = Some("movies".into());
            entry.tags = vec!["hd".into()];
            entry.set_error("disk full");
            entry.stats.add_download(75);
            entry.stats.add_upload(150);
            reg.add(entry).unwrap();
        }
        let app = build_deluge_router(AppState::new(registry));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"id":1,"method":"core.get_torrent_status","params":["{}",[]]}}"#,
                        "a".repeat(40)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 16384).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["error"].is_null(), "{:?}", body["error"]);
        assert!(body["result"]["active_time"].as_i64().unwrap() > 0);
        assert!(body["result"]["seeding_time"].as_i64().unwrap() > 0);
        assert_eq!(body["result"]["finished_time"], 200);
        assert_eq!(body["result"]["state"], "Error");
        assert_eq!(body["result"]["message"], "disk full");
        assert_eq!(body["result"]["tracker_status"], "Error: disk full");
        assert_json_keys(
            &body["result"],
            &[
                "hash",
                "name",
                "state",
                "progress",
                "total_size",
                "total_done",
                "download_payload_rate",
                "upload_payload_rate",
                "ratio",
                "save_path",
                "label",
                "tags",
                "is_finished",
                "eta",
                "num_peers",
                "num_seeds",
                "total_peers",
                "total_seeds",
                "num_files",
                "num_pieces",
                "piece_length",
                "distributed_copies",
                "seeds_peers_ratio",
                "max_download_speed",
                "max_upload_speed",
                "is_auto_managed",
                "stop_at_ratio",
                "stop_ratio",
                "remove_at_ratio",
                "prioritize_first_last",
                "sequential_download",
                "super_seeding",
                "move_on_completed",
                "move_on_completed_path",
                "time_added",
                "completed_time",
                "active_time",
                "seeding_time",
                "finished_time",
                "all_time_download",
                "total_uploaded",
                "total_payload_upload",
                "total_payload_download",
                "next_announce",
                "private",
                "owner",
                "shared",
                "tracker_host",
                "tracker_status",
                "tracker",
                "comment",
                "message",
            ],
        );
    }

    #[tokio::test]
    async fn deluge_torrent_status_honors_requested_fields() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data".into());
            entry.total_length = 100;
            entry.amount_left = 25;
            reg.add(entry).unwrap();
        }
        let app = build_deluge_router(AppState::new(registry));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"id":1,"method":"core.get_torrent_status","params":["{}",["name","progress"]]}}"#,
                        "a".repeat(40)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["result"]["name"], "alpha");
        assert_eq!(body["result"]["progress"], 75.0);
        assert_eq!(body["result"].as_object().unwrap().len(), 2);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":1,"method":"core.get_torrents_status","params":[{},["name","state"]]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let torrent = &body["result"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"];
        assert_eq!(torrent["name"], "alpha");
        assert_eq!(torrent["state"], "Paused");
        assert_eq!(torrent.as_object().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn deluge_torrents_status_honors_filter_dictionary() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut alpha = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data".into());
            alpha.category = Some("movies".into());
            reg.add(alpha).unwrap();
            let mut bravo = TorrentEntry::new("b".repeat(40), "bravo".into(), "/data".into());
            bravo.category = Some("tv".into());
            reg.add(bravo).unwrap();
        }
        let app = build_deluge_router(AppState::new(registry));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":1,"method":"core.get_torrents_status","params":[{"label":["movies"],"state":["Paused"]},["name","label","state"]]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["result"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"].is_object());
        assert!(body["result"]["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"].is_null());
        assert_eq!(
            body["result"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]["label"],
            "movies"
        );
        assert_eq!(
            body["result"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn deluge_peer_projection_uses_native_snapshots() {
        let seed = EnginePeerSnapshot {
            addr: "127.0.0.1:6881".parse().unwrap(),
            client: "seed".to_owned(),
            choked: false,
            upload_choked: false,
            interested: false,
            pieces: 10,
            pieces_total: 10,
            progress: 1.0,
            download_rate: 1_000,
            upload_rate: 2_000,
            downloaded: 10,
            uploaded: 20,
        };
        let leecher = EnginePeerSnapshot {
            addr: "127.0.0.2:6881".parse().unwrap(),
            client: "leecher".to_owned(),
            choked: false,
            upload_choked: false,
            interested: true,
            pieces: 3,
            pieces_total: 10,
            progress: 0.3,
            download_rate: 3_000,
            upload_rate: 4_000,
            downloaded: 30,
            uploaded: 40,
        };
        let peers = [seed, leecher];
        assert_eq!(deluge_peer_download_rate(Some(&peers)), 4_000);
        assert_eq!(deluge_peer_upload_rate(Some(&peers)), 6_000);
        assert_eq!(deluge_seed_count(Some(&peers)), 1);
        assert_eq!(deluge_leecher_count(Some(&peers)), 1);
        assert_eq!(
            deluge_eta(8_000, deluge_peer_download_rate(Some(&peers))),
            2
        );
        assert_eq!(deluge_distributed_copies(Some(&peers)), 1.0);
        assert_eq!(deluge_seeds_peers_ratio(Some(&peers)), 1.0);
    }

    #[test]
    fn deluge_tracker_projection_uses_persisted_engine_state() {
        let tracker = EngineTrackerSnapshot {
            id: 1,
            tier: 0,
            announce: "https://tracker.example/announce".to_owned(),
            status: "warning".to_owned(),
            last_announce_at: Some(100),
            next_announce_at: Some(200),
            last_success_at: Some(90),
            failure_reason: None,
            warning_message: Some("slow scrape".to_owned()),
            seeders: Some(3),
            leechers: Some(4),
            completed: Some(5),
        };
        assert_eq!(
            deluge_tracker_status(&tracker),
            "Warning: slow scrape".to_owned()
        );
        let mut entry = TorrentEntry::new("c".repeat(40), "charlie".into(), "/data".into());
        entry.total_length = 100;
        entry.amount_left = 50;
        let body = deluge_torrent(&entry, None, None, Some(&[tracker]), None, None, false);
        assert_eq!(body["tracker"], "https://tracker.example/announce");
        assert_eq!(body["tracker_host"], "tracker.example");
        assert_eq!(body["tracker_status"], "Warning: slow scrape");
        assert_eq!(body["next_announce"], 200);
    }

    #[test]
    fn deluge_projection_arguments_reject_malformed_filters_and_fields() {
        assert!(validate_deluge_status_filter(Some(&json!("all"))).is_err());
        assert!(validate_deluge_status_filter(Some(&json!({
            "state": ["Paused", 1]
        })))
        .is_err());
        assert!(deluge_requested_fields(Some(&json!("name"))).is_err());
        assert!(deluge_requested_fields(Some(&json!(["name", 1]))).is_err());
        assert!(deluge_requested_fields(Some(&json!(["name", "progress"]))).is_ok());
    }

    fn assert_json_keys(value: &Value, keys: &[&str]) {
        let obj = value.as_object().expect("expected JSON object");
        for key in keys {
            assert!(obj.contains_key(*key), "missing key {key} in {obj:?}");
        }
    }

    #[tokio::test]
    async fn deluge_auth_and_config_are_supported() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            reg.add(TorrentEntry::new(
                "a".repeat(40),
                "alpha".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        let app = build_deluge_router(AppState::new(registry));
        for (method, params) in deluge_method_matrix() {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/deluge/json")
                        .header("content-type", "application/json")
                        .body(Body::from(format!(
                            r#"{{"id":1,"method":"{method}","params":{params}}}"#
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(resp.status().is_success());
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            if let Some(message) = body["error"].get("message").and_then(Value::as_str) {
                assert!(
                    !message.starts_with("unsupported method"),
                    "{method} returned {:?}",
                    body["error"]
                );
            }
        }
    }

    #[tokio::test]
    async fn deluge_advertised_method_list_matches_probe_matrix() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let app = build_deluge_router(AppState::new(registry));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/deluge/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":1,"method":"daemon.get_method_list","params":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 16384).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let mut advertised = body["result"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        advertised.sort_unstable();
        let mut probed = deluge_method_matrix()
            .into_iter()
            .map(|(method, _)| method)
            .collect::<Vec<_>>();
        probed.sort_unstable();
        assert_eq!(advertised, probed);
    }

    fn deluge_method_matrix() -> Vec<(&'static str, &'static str)> {
        vec![
            ("auth.login", r#"[]"#),
            ("auth.check_session", r#"[]"#),
            ("daemon.login", r#"[]"#),
            ("daemon.info", r#"[]"#),
            ("daemon.get_method_list", r#"[]"#),
            ("daemon.shutdown", r#"[]"#),
            ("web.connected", r#"[]"#),
            ("web.add_host", r#"["127.0.0.1",58846,"localclient",""]"#),
            (
                "web.edit_host",
                r#"["TorrentNG","127.0.0.1",58846,"localclient",""]"#,
            ),
            ("web.remove_host", r#"["TorrentNG"]"#),
            ("web.get_config", r#"[]"#),
            ("web.update_ui", r#"[[],{}]"#),
            ("web.get_events", r#"[]"#),
            ("web.get_hosts", r#"[]"#),
            ("web.get_host_status", r#"[]"#),
            ("web.connect", r#"[]"#),
            ("web.disconnect", r#"[]"#),
            ("web.start_daemon", r#"[]"#),
            ("web.stop_daemon", r#"[]"#),
            (
                "web.download_torrent_from_url",
                r#"["https://example.invalid/test.torrent"]"#,
            ),
            ("web.add_torrents", r#"[[]]"#),
            ("web.get_plugins", r#"[]"#),
            ("web.get_plugin_info", r#"["Label"]"#),
            ("web.upload_plugin", r#"[]"#),
            ("web.update_config", r#"[{}]"#),
            ("web.save_config", r#"[]"#),
            (
                "web.get_torrent_files",
                r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]"#,
            ),
            ("core.get_torrents_status", r#"[{},[]]"#),
            (
                "core.get_torrent_status",
                r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",[]]"#,
            ),
            ("core.get_torrent_file_status", r#"[]"#),
            ("core.get_session_state", r#"[]"#),
            ("core.get_session_status", r#"[]"#),
            ("core.get_stats", r#"[]"#),
            ("core.get_num_connections", r#"[]"#),
            ("core.get_download_rate", r#"[]"#),
            ("core.get_upload_rate", r#"[]"#),
            ("core.get_filter_tree", r#"[]"#),
            ("core.pause_torrent", r#"[[]]"#),
            ("core.resume_torrent", r#"[[]]"#),
            ("core.force_recheck", r#"[[]]"#),
            ("core.queue_top", r#"[[]]"#),
            ("core.queue_up", r#"[[]]"#),
            ("core.queue_down", r#"[[]]"#),
            ("core.queue_bottom", r#"[[]]"#),
            (
                "core.remove_torrent",
                r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",false]"#,
            ),
            (
                "core.add_torrent_magnet",
                r#"["magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",{}]"#,
            ),
            ("core.add_torrent_file", r#"["test.torrent","",{}]"#),
            ("core.set_torrent_options", r#"[[],{}]"#),
            (
                "core.set_torrent_file_priorities",
                r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",[]]"#,
            ),
            (
                "core.set_torrent_trackers",
                r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",[]]"#,
            ),
            ("core.set_torrent_prioritize_first_last", r#"[[],false]"#),
            (
                "core.connect_peer",
                r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","127.0.0.1",6881]"#,
            ),
            (
                "core.rename_files",
                r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",[]]"#,
            ),
            (
                "core.rename_folder",
                r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","old","new"]"#,
            ),
            ("core.move_storage", r#"[[],"/tmp"]"#),
            ("core.get_config", r#"[]"#),
            ("core.get_config_values", r#"[["download_location"]]"#),
            ("core.get_config_value", r#"["download_location"]"#),
            ("core.set_config", r#"[{}]"#),
            ("core.get_free_space", r#"[]"#),
            ("core.get_listen_port", r#"[]"#),
            ("core.get_external_ip", r#"[]"#),
            ("core.get_path_size", r#"["/tmp"]"#),
            ("core.get_cache_status", r#"[]"#),
            ("core.get_enabled_plugins", r#"[]"#),
            ("core.enable_plugin", r#"["Label"]"#),
            ("core.disable_plugin", r#"["Label"]"#),
            ("core.get_available_plugins", r#"[]"#),
            ("core.get_libtorrent_version", r#"[]"#),
            ("core.create_torrent", r#"[]"#),
            ("core.upload_plugin", r#"[]"#),
            ("core.rescan_plugins", r#"[]"#),
            ("blocklist.get_config", r#"[]"#),
            ("blocklist.set_config", r#"[{}]"#),
            ("blocklist.get_status", r#"[]"#),
            ("blocklist.check_import", r#"[]"#),
            ("blocklist.import", r#"[]"#),
            ("autoadd.get_config", r#"[]"#),
            ("autoadd.set_config", r#"[{}]"#),
            ("autoadd.enable", r#"[1]"#),
            ("autoadd.disable", r#"[1]"#),
            ("execute.get_commands", r#"[]"#),
            ("execute.save_command", r#"["complete","echo done"]"#),
            ("execute.remove_command", r#"[1]"#),
            ("scheduler.get_config", r#"[]"#),
            ("scheduler.set_config", r#"[{}]"#),
            ("extractor.get_config", r#"[]"#),
            ("extractor.set_config", r#"[{}]"#),
            ("label.get_labels", r#"[]"#),
            ("label.add", r#"["test"]"#),
            ("label.remove", r#"["test"]"#),
            ("label.set_options", r#"["test",{}]"#),
            (
                "label.set_torrent",
                r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","test"]"#,
            ),
            ("notifications.get_handled_events", r#"[]"#),
            ("notifications.get_subscriptions", r#"[]"#),
            ("notifications.set_config", r#"[{}]"#),
            (
                "notifications.add_subscription",
                r#"["TorrentAddedEvent","email"]"#,
            ),
        ]
    }

    #[tokio::test]
    async fn deluge_plugin_cache_and_notification_shapes_are_structured() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("c".repeat(40), "cache".into(), "/data".into());
            entry.total_length = 100;
            entry.amount_left = 40;
            reg.add(entry).unwrap();
        }
        let app = build_deluge_router(AppState::new(registry));
        for (method, assertion_key) in [
            ("core.get_cache_status", "cache_size"),
            ("web.get_plugin_info", "name"),
            ("blocklist.get_status", "num_blocked"),
            ("autoadd.get_config", "watchdirs"),
            ("scheduler.get_config", "button_state"),
            ("extractor.get_config", "extract_path"),
            ("notifications.get_subscriptions", "TorrentAddedEvent"),
        ] {
            let params = if method == "web.get_plugin_info" {
                r#"["Blocklist"]"#
            } else {
                "[]"
            };
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/json")
                        .header("content-type", "application/json")
                        .body(Body::from(format!(
                            r#"{{"id":1,"method":"{method}","params":{params}}}"#
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert!(body["error"].is_null());
            assert!(!body["result"][assertion_key].is_null(), "{method}");
        }
    }

    #[tokio::test]
    async fn deluge_unsupported_plugin_writes_fail_closed() {
        let app = build_deluge_router(AppState::new(Arc::new(RwLock::new(SessionRegistry::new()))));

        for (id, method, params) in [
            (1, "blocklist.set_config", r#"[{"enabled":true}]"#),
            (2, "autoadd.set_config", r#"[{"watchdirs":{}}]"#),
            (3, "autoadd.enable", r#"[1]"#),
            (4, "autoadd.disable", r#"[1]"#),
            (5, "scheduler.set_config", r#"[{"low_down":10.0}]"#),
            (6, "extractor.set_config", r#"[{"enabled":true}]"#),
            (7, "execute.save_command", r#"["complete","echo done"]"#),
            (8, "execute.remove_command", r#"[1]"#),
            (9, "core.enable_plugin", r#"["Execute"]"#),
            (10, "core.disable_plugin", r#"["Execute"]"#),
        ] {
            let body = format!(r#"{{"id":{id},"method":"{method}","params":{params}}}"#);
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/json")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), axum::http::StatusCode::OK);
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert!(body["result"].is_null(), "{method}: {body:?}");
            assert!(
                body["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("not runtime-backed")),
                "{method}: {body:?}"
            );
        }

        for (method, expected) in [
            ("blocklist.get_config", blocklist_config()),
            ("autoadd.get_config", autoadd_config()),
            ("scheduler.get_config", scheduler_config()),
            ("extractor.get_config", extractor_config()),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/json")
                        .header("content-type", "application/json")
                        .body(Body::from(format!(
                            r#"{{"id":10,"method":"{method}","params":[]}}"#
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert!(body["error"].is_null(), "{method}: {body:?}");
            assert_eq!(body["result"], expected, "{method}");
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":13,"method":"core.get_enabled_plugins","params":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["result"], json!(["Label", "Notifications"]));

        let commands = dispatch(
            &AppState::new(Arc::new(RwLock::new(SessionRegistry::new()))),
            "execute.get_commands",
            &[],
        )
        .await
        .unwrap();
        assert_eq!(commands, json!([]));
    }

    #[tokio::test]
    async fn deluge_label_mutation_requires_native_engine() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            reg.add(TorrentEntry::new(
                "b".repeat(40),
                "beta".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        let app = build_deluge_router(AppState::new(Arc::clone(&registry)));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"id":1,"method":"label.set_torrent","params":["{}","movies"]}}"#,
                        "b".repeat(40)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["result"].is_null());
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("native engine is unavailable"));
        assert_eq!(
            registry.read().await.get(&"b".repeat(40)).unwrap().category,
            None
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":2,"method":"label.get_labels","params":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["result"], json!([]));
    }

    #[tokio::test]
    async fn deluge_file_probe_returns_array_shape() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            reg.add(TorrentEntry::new(
                "c".repeat(40),
                "gamma".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        let app = build_deluge_router(AppState::new(registry));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"id":1,"method":"web.get_torrent_files","params":["{}"]}}"#,
                        "c".repeat(40)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["error"].is_null());
        assert!(body["result"].as_array().is_some());
    }

    #[tokio::test]
    async fn deluge_web_add_torrents_reports_unavailable_engine_per_item() {
        let app = build_deluge_router(AppState::new(Arc::new(RwLock::new(SessionRegistry::new()))));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":1,"method":"web.add_torrents","params":[[
                            {"path":"magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","options":{"download_location":"/data"}},
                            {"path":"/tmp/uploaded.torrent","options":{}},
                            {"filename":"https://example.invalid/file.torrent","options":{}},
                            {"path":"inline.torrent","filedata":"ZHVtbXk=","params":{}}
                        ]]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["error"].is_null(), "{:?}", body["error"]);
        let results = body["result"].as_array().unwrap();
        assert_eq!(results.len(), 4);
        assert_eq!(results[0]["success"], false);
        assert_eq!(results[1]["success"], false);
        assert_eq!(results[2]["success"], false);
        assert_eq!(results[3]["success"], false);
        assert_eq!(
            results[0]["path"],
            "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(results[2]["path"], "https://example.invalid/file.torrent");
        assert_eq!(results[3]["path"], "inline.torrent");
    }

    #[tokio::test]
    async fn deluge_url_download_returns_stateful_safe_token() {
        let app = build_deluge_router(AppState::new(Arc::new(RwLock::new(SessionRegistry::new()))));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":1,"method":"web.download_torrent_from_url","params":["https://example.invalid/file.torrent"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["error"].is_null(), "{:?}", body["error"]);
        let token = body["result"].as_str().unwrap();
        assert!(token.starts_with("torrentng-url-download-"));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"id":2,"method":"web.add_torrents","params":[[{{"path":"{token}","options":{{}}}}]]}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["error"].is_null(), "{:?}", body["error"]);
        let result = &body["result"][0];
        assert_eq!(result["success"], false);
        assert_eq!(result["path"], token);
        assert_eq!(
            result["result"]["url"],
            "https://example.invalid/file.torrent"
        );
        assert_eq!(result["result"]["downloaded"], false);
    }

    #[tokio::test]
    async fn deluge_url_download_tokens_are_one_shot_and_report_engine_failure() {
        let app = build_deluge_router(AppState::new(Arc::new(RwLock::new(SessionRegistry::new()))));
        let magnet = "magnet:?xt=urn:btih:0123456789012345678901234567890123456789&dn=TokenMagnet";
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"id":1,"method":"web.download_torrent_from_url","params":["{magnet}"]}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let token = body["result"].as_str().unwrap();

        for id in [2, 3] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/json")
                        .header("content-type", "application/json")
                        .body(Body::from(format!(
                            r#"{{"id":{id},"method":"web.add_torrents","params":[[{{"path":"{token}","options":{{}}}}]]}}"#
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert!(body["error"].is_null(), "{:?}", body["error"]);
            let result = &body["result"][0];
            assert_eq!(result["success"], false);
            assert!(result["result"].is_null());
        }
    }

    #[tokio::test]
    async fn deluge_torrent_options_require_native_engine() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            reg.add(TorrentEntry::new(
                "d".repeat(40),
                "delta".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        let hash = "d".repeat(40);
        let app = build_deluge_router(AppState::new(registry));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"id":1,"method":"core.set_torrent_options","params":[["{hash}"],{{"max_download_speed":10.5,"max_upload_speed":4.0,"auto_managed":true,"stop_at_ratio":true,"stop_ratio":1.25,"sequential_download":true,"super_seeding":true,"prioritize_first_last":true,"move_completed":true,"move_completed_path":"/done"}}]}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["result"].is_null());
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("native engine is unavailable"));
    }

    #[test]
    fn deluge_mutator_parsers_accept_client_shapes() {
        assert_eq!(
            hashes_from_param(Some(&json!([" a ", "", "b"]))),
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert_eq!(deluge_file_priority(0), 0);
        assert_eq!(deluge_file_priority(1), 1);
        assert_eq!(deluge_file_priority(7), 2);
        assert_eq!(
            deluge_file_priority_updates(Some(&json!([0, 1, 5, 7])), None),
            Ok(vec![(vec![0], 0), (vec![1], 1), (vec![2, 3], 2)])
        );
        assert_eq!(
            deluge_file_priority_updates(Some(&json!([2, 4])), Some(&json!(0))),
            Ok(vec![(vec![2, 4], 0)])
        );
        assert_eq!(
            deluge_trackers_arg(Some(&json!([
                {"url":" udp://tracker.example/announce "},
                {"announce":"http://tracker.example/announce"},
                "udp://tracker.example/announce"
            ])))
            .unwrap(),
            vec![
                "udp://tracker.example/announce".to_owned(),
                "http://tracker.example/announce".to_owned()
            ]
        );
        assert_eq!(
            deluge_peer_addr_arg(&json!(["127.0.0.1", 6881])).unwrap(),
            "127.0.0.1:6881".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            deluge_rename_file_args(Some(&json!([
                [1, "new/a.bin"],
                {"index": 2, "path": "new/b.bin"}
            ])))
            .unwrap(),
            vec![(1, "new/a.bin".to_owned()), (2, "new/b.bin".to_owned())]
        );
        assert!(deluge_file_priority_updates(Some(&json!([u64::MAX])), Some(&json!(1))).is_err());
        assert!(deluge_peer_addr_arg(&json!(["127.0.0.1", 70_000])).is_err());
        assert!(deluge_rename_file_args(Some(&json!([[u64::MAX, "new"]]))).is_err());
        assert!(
            validate_deluge_options(json!({"max_download_speed": "NaN"}).as_object().unwrap())
                .is_err()
        );
        assert!(
            validate_deluge_options(json!({"unknown_option": true}).as_object().unwrap()).is_err()
        );
    }

    #[test]
    fn deluge_options_project_to_engine_limits() {
        let mut limits = EngineTorrentLimits::default();
        let options = json!({
            "prioritize_first_last": true,
            "sequential_download": "1",
            "super_seeding": 1,
            "auto_managed": false,
            "max_download_speed": 10.5,
            "max_upload_speed": "-1",
            "stop_at_ratio": true,
            "stop_ratio": 1.25
        });
        apply_deluge_options(&mut limits, options.as_object().unwrap());
        assert!(limits.first_last_piece_prio);
        assert!(limits.sequential_download);
        assert!(limits.super_seeding);
        assert_eq!(limits.download_limit, Some(10_752));
        assert_eq!(limits.upload_limit, None);
        assert_eq!(limits.seed_ratio_limit, Some(1.25));

        apply_deluge_options(
            &mut limits,
            json!({"stop_at_ratio": false}).as_object().unwrap(),
        );
        assert_eq!(limits.seed_ratio_limit, None);
    }

    #[test]
    fn deluge_torrent_data_decoder_accepts_data_urls_and_unpadded_base64() {
        assert_eq!(
            decode_deluge_torrent_data("data:application/x-bittorrent;base64,ZHVtbXk=").unwrap(),
            b"dummy"
        );
        assert_eq!(decode_deluge_torrent_data("ZHVtbXk").unwrap(), b"dummy");
    }

    #[test]
    fn deluge_api_snapshot_estimates_scale_with_torrent_count() {
        assert_eq!(estimate_deluge_torrents_snapshot_bytes(0), 0);
        assert_eq!(estimate_deluge_torrents_snapshot_bytes(10), 30_720);
        assert_eq!(estimate_deluge_update_ui_snapshot_bytes(0), 32 * 1024);
        assert_eq!(
            estimate_deluge_update_ui_snapshot_bytes(10),
            32 * 1024 + 40_960
        );
        assert_eq!(estimate_deluge_torrent_detail_snapshot_bytes(), 64 * 1024);
    }
}
