#![recursion_limit = "256"]

use std::{net::SocketAddr, sync::Arc};

use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
use base64::{engine::general_purpose, Engine as _};
use rt_engine::{EngineHandle, EngineTorrentLimits, EngineTorrentMetadata};
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_session::SessionRegistry;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;

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
        .with_state(state)
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
            "version": "rtorrentNG",
            "libtorrent": "native",
        })),
        "daemon.get_method_list" => Ok(json!(supported_methods())),
        "daemon.shutdown" => Ok(json!(true)),
        "web.connected" => Ok(json!(true)),
        "web.add_host" => Ok(json!("rtorrentNG")),
        "web.edit_host" | "web.remove_host" => Ok(json!(true)),
        "web.get_config" => Ok(deluge_web_config()),
        "web.get_host_status" => Ok(json!(["rtorrentNG", "127.0.0.1", 0, "Online"])),
        "web.get_hosts" => Ok(json!([["rtorrentNG", "127.0.0.1", 0, "rtorrentNG"]])),
        "web.connect" | "web.disconnect" | "web.start_daemon" | "web.stop_daemon" => {
            Ok(json!(true))
        }
        "web.download_torrent_from_url" => Ok(json!("")),
        "web.add_torrents" => web_add_torrents(state, params).await,
        "web.get_events" => web_events(state).await,
        "web.get_plugins" => Ok(json!(deluge_plugins())),
        "web.get_plugin_info" => Ok(plugin_info(params.first().and_then(Value::as_str))),
        "web.upload_plugin" | "web.update_config" | "web.save_config" => Ok(json!(true)),
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
        "core.get_num_connections" => Ok(json!(0)),
        "core.get_download_rate" => Ok(json!(0.0)),
        "core.get_upload_rate" => Ok(json!(0.0)),
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
            for hash in string_list(params.first()) {
                if let Some(engine) = &state.engine {
                    let _ = engine.pause_torrent(hash).await;
                }
            }
            Ok(json!(true))
        }
        "core.resume_torrent" => {
            for hash in string_list(params.first()) {
                if let Some(engine) = &state.engine {
                    let _ = engine.resume_torrent(hash).await;
                }
            }
            Ok(json!(true))
        }
        "core.force_recheck" => {
            for hash in string_list(params.first()) {
                if let Some(engine) = &state.engine {
                    let _ = engine.recheck_torrent(hash).await;
                }
            }
            Ok(json!(true))
        }
        "core.queue_top"
        | "core.queue_up"
        | "core.queue_down"
        | "core.queue_bottom"
        | "core.create_torrent"
        | "core.upload_plugin"
        | "core.rescan_plugins" => Ok(json!(true)),
        "core.set_torrent_prioritize_first_last" => set_prioritize_first_last(state, params).await,
        "core.set_torrent_file_priorities" => set_file_priorities(state, params).await,
        "core.set_torrent_trackers" => set_trackers(state, params).await,
        "core.connect_peer" => connect_peer(state, params).await,
        "core.rename_files" => rename_files(state, params).await,
        "core.rename_folder" => rename_folder(state, params).await,
        "core.move_storage" => move_storage(state, params).await,
        "core.get_torrent_file_status" => {
            if let Some(hash) = params.first().and_then(Value::as_str) {
                torrent_files(state, hash).await
            } else {
                Ok(json!([]))
            }
        }
        "core.remove_torrent" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "missing torrent id".to_owned())?;
            let remove_data = params.get(1).and_then(Value::as_bool).unwrap_or(false);
            if let Some(engine) = &state.engine {
                let _ = engine.remove_torrent(hash.to_owned(), remove_data).await;
            }
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
        "label.add" => Ok(json!(true)),
        "label.remove" => Ok(json!(true)),
        "label.set_options" => Ok(json!(true)),
        "label.set_torrent" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "missing torrent id".to_owned())?;
            let label = params.get(1).and_then(Value::as_str).unwrap_or_default();
            set_label(state, hash, label).await?;
            Ok(json!(true))
        }
        "core.get_free_space" => Ok(json!(0)),
        "core.set_config" => Ok(json!(true)),
        "core.get_listen_port" => Ok(json!(0)),
        "core.get_external_ip" => Ok(json!("")),
        "core.get_path_size" => Ok(json!(0)),
        "core.get_cache_status" => cache_status(state).await,
        "core.get_config" => Ok(deluge_config()),
        "core.get_config_values" => Ok(deluge_config_values(params.first())),
        "core.get_config_value" => Ok(deluge_config_value(params.first())),
        "core.get_enabled_plugins" => Ok(json!(deluge_plugins())),
        "core.enable_plugin" | "core.disable_plugin" => Ok(json!(true)),
        "core.get_available_plugins" => Ok(json!(deluge_plugins())),
        "core.get_libtorrent_version" => Ok(json!("native")),
        "notifications.get_handled_events" => Ok(json!(notification_events())),
        "notifications.get_subscriptions" => Ok(notification_subscriptions()),
        "notifications.set_config" | "notifications.add_subscription" => Ok(json!(true)),
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

fn deluge_config() -> Value {
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

fn deluge_config_values(keys: Option<&Value>) -> Value {
    let config = deluge_config();
    let Some(keys) = keys.and_then(Value::as_array) else {
        return config;
    };
    let mut out = serde_json::Map::new();
    for key in keys.iter().filter_map(Value::as_str) {
        out.insert(
            key.to_owned(),
            config.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(out)
}

fn deluge_config_value(key: Option<&Value>) -> Value {
    let Some(key) = key.and_then(Value::as_str) else {
        return Value::Null;
    };
    deluge_config().get(key).cloned().unwrap_or(Value::Null)
}

fn deluge_web_config() -> Value {
    json!({
        "base": "/",
        "pwd_salt": "",
        "pwd_sha1": "",
        "sessions": {},
        "session_timeout": 3600,
        "default_daemon": "rtorrentNG",
        "sidebar_show_zero": false,
        "sidebar_multiple_filters": true,
        "show_session_speed": false,
        "theme": "gray",
        "first_login": false,
    })
}

async fn web_add_torrents(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let Some(torrents) = params.first().and_then(Value::as_array) else {
        return Ok(json!(true));
    };
    if state.engine.is_none() {
        return Ok(json!(true));
    }
    let mut results = Vec::new();
    for torrent in torrents {
        let options = torrent.get("options").or_else(|| torrent.get("params"));
        let path = torrent
            .get("path")
            .or_else(|| torrent.get("url"))
            .or_else(|| torrent.get("filename"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = if path.starts_with("magnet:") {
            add_magnet(state, path, options).await
        } else if let Some(data) = torrent
            .get("data")
            .or_else(|| torrent.get("torrent"))
            .or_else(|| torrent.get("metainfo"))
            .and_then(Value::as_str)
        {
            add_torrent_file(state, data, options).await
        } else {
            Ok(json!(true))
        };
        results.push(json!({
            "path": path,
            "success": result.is_ok(),
            "result": result.unwrap_or(Value::Null),
        }));
    }
    Ok(Value::Array(results))
}

async fn move_storage(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let location = params
        .get(1)
        .and_then(Value::as_str)
        .ok_or_else(|| "missing storage path".to_owned())?;
    for hash in string_list(params.first()) {
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
    Ok(json!(true))
}

async fn session_state(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    Ok(json!(reg
        .iter()
        .map(|entry| entry.info_hash.clone())
        .collect::<Vec<_>>()))
}

async fn session_status(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    let torrent_count = reg.iter().count();
    let total_payload_download = reg.iter().fold(0_u64, |acc, entry| {
        acc.saturating_add(entry.stats.downloaded)
    });
    let total_payload_upload = reg
        .iter()
        .fold(0_u64, |acc, entry| acc.saturating_add(entry.stats.uploaded));
    Ok(json!({
        "payload_download_rate": 0.0,
        "payload_upload_rate": 0.0,
        "download_rate": 0.0,
        "upload_rate": 0.0,
        "num_connections": 0,
        "total_payload_download": total_payload_download,
        "total_payload_upload": total_payload_upload,
        "num_torrents": torrent_count,
    }))
}

async fn cache_status(state: &AppState) -> Result<Value, String> {
    let (num_torrents, total_done, total_left) = {
        let reg = state.registry.read().await;
        reg.iter()
            .fold((0_u64, 0_u64, 0_u64), |(count, done, left), entry| {
                (
                    count + 1,
                    done.saturating_add(entry.total_length.saturating_sub(entry.amount_left)),
                    left.saturating_add(entry.amount_left),
                )
            })
    };
    let jobs_active = if let Some(engine) = &state.engine {
        engine
            .stats()
            .await
            .map(|stats| stats.jobs_active)
            .unwrap_or(0)
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
    Ok(json!(reg
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
    vec!["Label", "Notifications"]
}

fn plugin_info(name: Option<&str>) -> Value {
    let name = name.unwrap_or_default();
    match name {
        "Label" | "label" => json!({
            "name": "Label",
            "version": "rtorrentNG",
            "author": "rtorrentNG",
            "description": "Category and label compatibility backed by native torrent labels.",
            "enabled": true,
        }),
        "Notifications" | "notifications" => json!({
            "name": "Notifications",
            "version": "rtorrentNG",
            "author": "rtorrentNG",
            "description": "Native session event notification compatibility.",
            "enabled": true,
        }),
        _ => json!({}),
    }
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
    let mut labels = std::collections::BTreeMap::<String, usize>::new();
    let mut states = std::collections::BTreeMap::<String, usize>::new();
    for entry in reg.iter() {
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

async fn update_ui(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let wanted_fields = deluge_requested_fields(params.first());
    let entries = {
        let reg = state.registry.read().await;
        reg.iter().cloned().collect::<Vec<_>>()
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
        .map(|entry| {
            (
                entry.info_hash.clone(),
                filter_deluge_torrent_fields(
                    deluge_torrent(entry, metadata.get(&entry.info_hash)),
                    &wanted_fields,
                ),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Ok(json!({
        "connected": true,
        "torrents": torrents,
        "filters": deluge_filters_from_entries(&entries),
        "stats": {
            "download_rate": 0.0,
            "upload_rate": 0.0,
            "num_connections": 0,
            "dht_nodes": 0,
            "has_incoming_connections": true,
            "free_space": 0,
        }
    }))
}

fn deluge_filters_from_entries(entries: &[rt_session::TorrentEntry]) -> Value {
    let mut states = std::collections::BTreeMap::<String, usize>::new();
    for entry in entries {
        *states
            .entry(deluge_state(entry.state.as_str()).to_owned())
            .or_default() += 1;
    }
    let mut state_filters = vec![json!(["All", entries.len()])];
    state_filters.extend(
        states
            .into_iter()
            .map(|(state, count)| json!([state, count]))
            .collect::<Vec<_>>(),
    );
    json!({
        "state": state_filters,
        "label": labels_from_entries(entries),
    })
}

async fn labels(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    Ok(json!(reg
        .iter()
        .filter_map(|entry| entry.category.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()))
}

async fn set_label(state: &AppState, hash: &str, label: &str) -> Result<(), String> {
    let label = label.trim();
    let category = if label.is_empty() {
        None
    } else {
        Some(label.to_owned())
    };
    if let Some(engine) = &state.engine {
        engine
            .update_torrent_labels(hash.to_owned(), Some(category), Vec::new(), Vec::new())
            .await?;
        return Ok(());
    }
    let mut reg = state.registry.write().await;
    let entry = reg
        .get_mut(hash)
        .ok_or_else(|| format!("torrent {hash} not found"))?;
    entry.category = category;
    Ok(())
}

async fn torrents_status(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let filter = params.first();
    let wanted_fields = deluge_requested_fields(params.get(1));
    let entries = {
        let reg = state.registry.read().await;
        reg.iter().cloned().collect::<Vec<_>>()
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
        .filter(|entry| deluge_torrent_matches_filter(entry, filter))
        .map(|entry| {
            (
                entry.info_hash.clone(),
                filter_deluge_torrent_fields(
                    deluge_torrent(entry, metadata.get(&entry.info_hash)),
                    &wanted_fields,
                ),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Ok(Value::Object(torrents))
}

fn deluge_torrent_matches_filter(entry: &rt_session::TorrentEntry, filter: Option<&Value>) -> bool {
    let Some(filter) = filter.and_then(Value::as_object) else {
        return true;
    };
    for (key, value) in filter {
        match key.as_str() {
            "id" | "ids" | "hash" | "hashes" => {
                let values = string_list(Some(value));
                if !values.is_empty() && !values.iter().any(|hash| hash == &entry.info_hash) {
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
                    && !values
                        .iter()
                        .any(|state| deluge_state(entry.state.as_str()).eq_ignore_ascii_case(state))
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

async fn torrent_status(
    state: &AppState,
    hash: &str,
    fields: Option<&Value>,
) -> Result<Value, String> {
    let entry = {
        let reg = state.registry.read().await;
        reg.get(hash)
            .cloned()
            .ok_or_else(|| format!("torrent {hash} not found"))?
    };
    let meta = if let Some(engine) = &state.engine {
        engine.torrent_metadata(hash.to_owned()).await.ok()
    } else {
        None
    };
    let wanted_fields = deluge_requested_fields(fields);
    Ok(filter_deluge_torrent_fields(
        deluge_torrent(&entry, meta.as_ref()),
        &wanted_fields,
    ))
}

fn deluge_requested_fields(value: Option<&Value>) -> Option<std::collections::BTreeSet<String>> {
    let fields = value?.as_array()?;
    if fields.is_empty() {
        return None;
    }
    Some(
        fields
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    )
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

async fn torrent_files(state: &AppState, hash: &str) -> Result<Value, String> {
    if let Some(engine) = &state.engine {
        if let Ok(meta) = engine.torrent_metadata(hash.to_owned()).await {
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
    }
    Ok(json!([]))
}

fn deluge_torrent(entry: &rt_session::TorrentEntry, meta: Option<&EngineTorrentMetadata>) -> Value {
    let progress = if entry.total_length == 0 {
        0.0
    } else {
        entry.total_length.saturating_sub(entry.amount_left) as f64 * 100.0
            / entry.total_length as f64
    };
    let tracker = meta
        .and_then(|meta| meta.trackers.first())
        .cloned()
        .unwrap_or_default();
    json!({
        "hash": entry.info_hash,
        "name": entry.name,
        "state": deluge_state(entry.state.as_str()),
        "progress": progress,
        "total_size": entry.total_length,
        "total_done": entry.total_length.saturating_sub(entry.amount_left),
        "download_payload_rate": 0,
        "upload_payload_rate": 0,
        "ratio": entry.stats.ratio(),
        "save_path": entry.save_path,
        "label": entry.category.clone().unwrap_or_default(),
        "tags": entry.tags,
        "is_finished": entry.completed_at.is_some(),
        "eta": 0,
        "num_peers": 0,
        "num_seeds": 0,
        "total_peers": 0,
        "total_seeds": 0,
        "num_files": meta.map(|meta| meta.files.len()).unwrap_or(0),
        "num_pieces": meta.map(|meta| meta.piece_count).unwrap_or(0),
        "piece_length": meta.map(|meta| meta.piece_length).unwrap_or(0),
        "distributed_copies": 0.0,
        "seeds_peers_ratio": 0.0,
        "max_download_speed": -1.0,
        "max_upload_speed": -1.0,
        "is_auto_managed": false,
        "stop_at_ratio": false,
        "stop_ratio": 0.0,
        "remove_at_ratio": false,
        "prioritize_first_last": false,
        "sequential_download": false,
        "super_seeding": false,
        "move_on_completed": false,
        "move_on_completed_path": "",
        "time_added": entry.added_at,
        "completed_time": entry.completed_at.unwrap_or(0),
        "active_time": 0,
        "seeding_time": 0,
        "finished_time": 0,
        "all_time_download": entry.stats.downloaded,
        "total_uploaded": entry.stats.uploaded,
        "total_payload_upload": entry.stats.uploaded,
        "total_payload_download": entry.stats.downloaded,
        "next_announce": 0,
        "private": meta.map(|meta| meta.is_private).unwrap_or(false),
        "owner": "localclient",
        "shared": false,
        "tracker_host": tracker_host(&tracker),
        "tracker_status": "",
        "tracker": tracker,
        "comment": "",
        "message": "",
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

fn labels_from_entries<'a>(
    entries: impl IntoIterator<Item = &'a rt_session::TorrentEntry>,
) -> Vec<Value> {
    let mut labels = std::collections::BTreeMap::<String, usize>::new();
    for entry in entries {
        if let Some(label) = &entry.category {
            *labels.entry(label.clone()).or_default() += 1;
        }
    }
    labels
        .into_iter()
        .map(|(label, count)| json!([label, count]))
        .collect()
}

async fn add_magnet(state: &AppState, uri: &str, options: Option<&Value>) -> Result<Value, String> {
    let Some(engine) = &state.engine else {
        return Err("engine unavailable".to_owned());
    };
    let magnet = parse_magnet(uri).map_err(|e| e.to_string())?;
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
    let Some(engine) = &state.engine else {
        return Err("engine unavailable".to_owned());
    };
    let raw = general_purpose::STANDARD
        .decode(data)
        .map_err(|e| e.to_string())?;
    let meta = parse_torrent(&raw).map_err(|e| e.to_string())?;
    let save_path = options
        .and_then(|value| value.get("download_location"))
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    let hash = engine
        .add_torrent_with_labels(meta, save_path, false, None, Vec::new())
        .await?;
    Ok(json!(hash))
}

async fn set_torrent_options(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let hashes = hashes_from_param(params.first());
    let Some(options) = params.get(1).and_then(Value::as_object) else {
        return Ok(json!(true));
    };
    let Some(engine) = &state.engine else {
        return Ok(json!(true));
    };
    for hash in hashes {
        let mut limits = engine
            .torrent_limits(hash.clone())
            .await
            .unwrap_or_else(|_| EngineTorrentLimits::default());
        apply_deluge_options(&mut limits, options);
        engine.update_torrent_limits(hash, limits).await?;
    }
    Ok(json!(true))
}

async fn set_prioritize_first_last(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let hashes = hashes_from_param(params.first());
    let Some(enabled) = deluge_bool(params.get(1)) else {
        return Ok(json!(true));
    };
    let Some(engine) = &state.engine else {
        return Ok(json!(true));
    };
    for hash in hashes {
        let mut limits = engine
            .torrent_limits(hash.clone())
            .await
            .unwrap_or_else(|_| EngineTorrentLimits::default());
        limits.first_last_piece_prio = enabled;
        engine.update_torrent_limits(hash, limits).await?;
    }
    Ok(json!(true))
}

async fn set_file_priorities(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let Some(hash) = params.first().and_then(Value::as_str) else {
        return Ok(json!(true));
    };
    let updates = deluge_file_priority_updates(params.get(1), params.get(2));
    if updates.is_empty() {
        return Ok(json!(true));
    }
    let Some(engine) = &state.engine else {
        return Ok(json!(true));
    };
    for (file_ids, priority) in updates {
        engine
            .update_file_priorities(hash.to_owned(), file_ids, priority)
            .await?;
    }
    Ok(json!(true))
}

async fn set_trackers(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let Some(hash) = params.first().and_then(Value::as_str) else {
        return Ok(json!(true));
    };
    let trackers = deluge_trackers_arg(params.get(1));
    let Some(engine) = &state.engine else {
        return Ok(json!(true));
    };
    engine
        .update_torrent_trackers(hash.to_owned(), trackers)
        .await?;
    Ok(json!(true))
}

async fn connect_peer(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let Some(hash) = params.first().and_then(Value::as_str) else {
        return Ok(json!(true));
    };
    let peer = if let Some(addr) = params.get(1).and_then(deluge_peer_addr_arg) {
        Some(addr)
    } else {
        deluge_peer_host_port(params.get(1), params.get(2))
    };
    let Some(peer) = peer else {
        return Ok(json!(true));
    };
    let Some(engine) = &state.engine else {
        return Ok(json!(true));
    };
    engine.add_peers(hash.to_owned(), vec![peer]).await?;
    Ok(json!(true))
}

async fn rename_files(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let Some(hash) = params.first().and_then(Value::as_str) else {
        return Ok(json!(true));
    };
    let renames = deluge_rename_file_args(params.get(1));
    let Some(engine) = &state.engine else {
        return Ok(json!(true));
    };
    for (file_id, new_path) in renames {
        engine
            .rename_file_path(hash.to_owned(), file_id, new_path)
            .await?;
    }
    Ok(json!(true))
}

async fn rename_folder(state: &AppState, params: &[Value]) -> Result<Value, String> {
    let Some(hash) = params.first().and_then(Value::as_str) else {
        return Ok(json!(true));
    };
    let Some(old_path) = params.get(1).and_then(Value::as_str) else {
        return Ok(json!(true));
    };
    let Some(new_path) = params.get(2).and_then(Value::as_str) else {
        return Ok(json!(true));
    };
    let Some(engine) = &state.engine else {
        return Ok(json!(true));
    };
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
        .get("auto_managed")
        .and_then(|value| deluge_bool(Some(value)))
    {
        limits.auto_management = value;
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

fn deluge_bool(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Number(value)) => Some(value.as_i64().unwrap_or_default() != 0),
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn deluge_speed_limit(value: Option<&Value>) -> Option<Option<i64>> {
    let kib = match value {
        Some(Value::Number(value)) => value.as_f64()?,
        Some(Value::String(value)) => value.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if kib <= 0.0 {
        Some(None)
    } else {
        Some(Some((kib * 1024.0) as i64))
    }
}

fn deluge_file_priority_updates(
    ids_or_priorities: Option<&Value>,
    priority: Option<&Value>,
) -> Vec<(Vec<u32>, i64)> {
    if let Some(priority) = priority.and_then(Value::as_i64) {
        let ids = deluge_file_ids(ids_or_priorities);
        if ids.is_empty() {
            return Vec::new();
        }
        return vec![(ids, deluge_file_priority(priority))];
    }
    let Some(priorities) = ids_or_priorities.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut skipped = Vec::new();
    let mut normal = Vec::new();
    let mut high = Vec::new();
    for (idx, value) in priorities.iter().enumerate() {
        let Some(priority) = value.as_i64() else {
            continue;
        };
        match deluge_file_priority(priority) {
            0 => skipped.push(idx as u32),
            2 => high.push(idx as u32),
            _ => normal.push(idx as u32),
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
    updates
}

fn deluge_file_ids(value: Option<&Value>) -> Vec<u32> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_u64().map(|value| value as u32))
                .collect()
        })
        .unwrap_or_default()
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

fn deluge_trackers_arg(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let mut trackers = Vec::new();
    collect_deluge_trackers(value, &mut trackers);
    normalize_deluge_trackers(trackers)
}

fn collect_deluge_trackers(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(value) => out.push(value.to_owned()),
        Value::Array(values) => {
            for value in values {
                collect_deluge_trackers(value, out);
            }
        }
        Value::Object(obj) => {
            if let Some(url) = obj
                .get("url")
                .or_else(|| obj.get("announce"))
                .and_then(Value::as_str)
            {
                out.push(url.to_owned());
            }
        }
        _ => {}
    }
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

fn deluge_peer_addr_arg(value: &Value) -> Option<SocketAddr> {
    match value {
        Value::String(value) => value.trim().parse().ok(),
        Value::Array(values) => deluge_peer_host_port(values.first(), values.get(1)),
        Value::Object(obj) => {
            deluge_peer_host_port(obj.get("ip").or_else(|| obj.get("host")), obj.get("port"))
        }
        _ => None,
    }
}

fn deluge_peer_host_port(host: Option<&Value>, port: Option<&Value>) -> Option<SocketAddr> {
    let host = host.and_then(Value::as_str)?.trim();
    let port = match port {
        Some(Value::Number(value)) => value.as_u64()? as u16,
        Some(Value::String(value)) => value.trim().parse().ok()?,
        _ => return None,
    };
    format!("{host}:{port}").parse().ok()
}

fn deluge_rename_file_args(value: Option<&Value>) -> Vec<(u32, String)> {
    value
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(deluge_rename_file_arg).collect())
        .unwrap_or_default()
}

fn deluge_rename_file_arg(value: &Value) -> Option<(u32, String)> {
    match value {
        Value::Array(values) => {
            let id = values.first()?.as_u64()? as u32;
            let path = values.get(1)?.as_str()?.to_owned();
            Some((id, path))
        }
        Value::Object(obj) => {
            let id = obj
                .get("index")
                .or_else(|| obj.get("id"))
                .or_else(|| obj.get("file_id"))?
                .as_u64()? as u32;
            let path = obj
                .get("path")
                .or_else(|| obj.get("name"))
                .or_else(|| obj.get("new_path"))?
                .as_str()?
                .to_owned();
            Some((id, path))
        }
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use rt_session::TorrentEntry;
    use tower::ServiceExt;

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
            entry.category = Some("movies".into());
            entry.tags = vec!["hd".into()];
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
                r#"["rtorrentNG","127.0.0.1",58846,"localclient",""]"#,
            ),
            ("web.remove_host", r#"["rtorrentNG"]"#),
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
            ("notifications.get_subscriptions", "TorrentAddedEvent"),
        ] {
            let params = if method == "web.get_plugin_info" {
                r#"["Label"]"#
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
    async fn deluge_label_methods_update_registry() {
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
        assert_eq!(
            registry
                .read()
                .await
                .get(&"b".repeat(40))
                .unwrap()
                .category
                .as_deref(),
            Some("movies")
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
        assert_eq!(body["result"], json!(["movies"]));
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
            vec![(vec![0], 0), (vec![1], 1), (vec![2, 3], 2)]
        );
        assert_eq!(
            deluge_file_priority_updates(Some(&json!([2, 4])), Some(&json!(0))),
            vec![(vec![2, 4], 0)]
        );
        assert_eq!(
            deluge_trackers_arg(Some(&json!([
                {"url":" udp://tracker.example/announce "},
                {"announce":"http://tracker.example/announce"},
                "udp://tracker.example/announce"
            ]))),
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
            ]))),
            vec![(1, "new/a.bin".to_owned()), (2, "new/b.bin".to_owned())]
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
        assert!(!limits.auto_management);
        assert_eq!(limits.download_limit, Some(10_752));
        assert_eq!(limits.upload_limit, None);
        assert_eq!(limits.seed_ratio_limit, Some(1.25));

        apply_deluge_options(
            &mut limits,
            json!({"stop_at_ratio": false}).as_object().unwrap(),
        );
        assert_eq!(limits.seed_ratio_limit, None);
    }
}
