use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use base64::{engine::general_purpose, Engine as _};
use rt_engine::{
    EngineGlobalLimits, EngineHandle, EnginePeerSnapshot, EngineTorrentFile, EngineTorrentLimits,
    EngineTorrentMetadata, EngineTrackerSnapshot,
};
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_metrics::{MemoryClass, MemoryLease};
use rt_session::{SessionRegistry, TorrentEntry};
use serde_json::Value;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub session_path: String,
    pub network_port: i64,
    global_down_limit: Arc<RwLock<i64>>,
    global_up_limit: Arc<RwLock<i64>>,
    torrent_limits: Arc<RwLock<BTreeMap<String, EngineTorrentLimits>>>,
    custom: Arc<RwLock<BTreeMap<String, BTreeMap<String, RtValue>>>>,
    views: Arc<RwLock<BTreeSet<String>>>,
}

impl AppState {
    pub fn new(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        Self {
            registry,
            engine: None,
            session_path: String::new(),
            network_port: 0,
            global_down_limit: Arc::new(RwLock::new(0)),
            global_up_limit: Arc::new(RwLock::new(0)),
            torrent_limits: Arc::new(RwLock::new(BTreeMap::new())),
            custom: Arc::new(RwLock::new(BTreeMap::new())),
            views: Arc::new(RwLock::new(default_rtorrent_views())),
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        Self {
            engine: Some(engine),
            ..Self::new(registry)
        }
    }
}

async fn reserve_rtorrent_api_snapshot(
    state: &AppState,
    bytes: u64,
) -> Result<Option<MemoryLease>, String> {
    let Some(engine) = &state.engine else {
        return Ok(None);
    };
    engine
        .reserve_memory(MemoryClass::ApiSnapshot, bytes)
        .await?
        .map(Some)
        .ok_or_else(|| "api snapshot memory budget exhausted".to_owned())
}

fn estimate_rtorrent_multicall_snapshot_bytes(torrent_count: usize, command_count: usize) -> u64 {
    let commands = command_count.max(1) as u64;
    8 * 1024 + (torrent_count as u64).saturating_mul(512 + commands.saturating_mul(160))
}

#[derive(Debug, Clone, PartialEq)]
pub enum RtValue {
    Int(i64),
    Bool(bool),
    String(String),
    Array(Vec<RtValue>),
    Struct(BTreeMap<String, RtValue>),
    Nil,
}

impl RtValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            RtValue::String(value) => Some(value),
            _ => None,
        }
    }
}

pub fn supported_methods() -> &'static [&'static str] {
    &[
        "method.list",
        "system.client_version",
        "system.library_version",
        "system.time",
        "session.name",
        "session.path",
        "network.port_open",
        "network.port_random",
        "throttle.global_down.max_rate",
        "throttle.global_up.max_rate",
        "throttle.global_down.max_rate.set",
        "throttle.global_up.max_rate.set",
        "view.list",
        "view.add",
        "view.set",
        "view.size",
        "d.hash",
        "d.name",
        "d.base_path",
        "d.directory",
        "d.size_bytes",
        "d.left_bytes",
        "d.completed_bytes",
        "d.complete",
        "d.is_active",
        "d.state",
        "d.state_changed",
        "d.up.total",
        "d.down.total",
        "d.down.max_rate",
        "d.down.max_rate.set",
        "d.up.max_rate",
        "d.up.max_rate.set",
        "d.ratio",
        "d.custom",
        "d.custom.set",
        "d.multicall",
        "d.multicall2",
        "load.normal",
        "load.start",
        "load.raw",
        "load.raw_start",
        "d.erase",
        "d.pause",
        "d.resume",
        "d.stop",
        "d.start",
        "d.tracker_announce",
        "f.multicall",
        "t.multicall",
        "p.multicall",
    ]
}

pub async fn execute(
    state: &AppState,
    method: &str,
    params: &[RtValue],
) -> Result<RtValue, String> {
    match method {
        "method.list" => Ok(RtValue::Array(
            supported_methods()
                .iter()
                .map(|method| RtValue::String((*method).to_owned()))
                .collect(),
        )),
        "system.client_version" => Ok(RtValue::String("TorrentNG".to_owned())),
        "system.library_version" => Ok(RtValue::String("native".to_owned())),
        "system.time" => Ok(RtValue::Int(unix_now())),
        "session.name" => Ok(RtValue::String("TorrentNG".to_owned())),
        "session.path" => Ok(RtValue::String(state.session_path.clone())),
        "network.port_open" => Ok(RtValue::Int(state.network_port)),
        "network.port_random" => Ok(RtValue::Bool(false)),
        "throttle.global_down.max_rate" => Ok(RtValue::Int(global_down_limit(state).await)),
        "throttle.global_up.max_rate" => Ok(RtValue::Int(global_up_limit(state).await)),
        "throttle.global_down.max_rate.set" => set_global_limit(state, params, true).await,
        "throttle.global_up.max_rate.set" => set_global_limit(state, params, false).await,
        "view.list" => Ok(RtValue::Array(rtorrent_views(state).await)),
        "view.add" | "view.set" => rtorrent_view_add(state, params).await,
        "view.size" => Ok(RtValue::Int(view_size(state, params).await)),
        "d.multicall" | "d.multicall2" => d_multicall(state, params).await,
        "load.normal" | "load.start" | "load.raw" | "load.raw_start" => {
            load(state, method, params).await
        }
        "d.erase" => lifecycle(state, params, Lifecycle::Erase).await,
        "d.pause" | "d.stop" => lifecycle(state, params, Lifecycle::Pause).await,
        "d.resume" | "d.start" => lifecycle(state, params, Lifecycle::Resume).await,
        "d.tracker_announce" => Ok(RtValue::Int(0)),
        "f.multicall" => file_multicall(state, params).await,
        "t.multicall" => tracker_multicall(state, params).await,
        "p.multicall" => peer_multicall(state, params).await,
        _ if method.starts_with("d.") => d_read_or_write(state, method, params).await,
        _ => Err(format!("unsupported rTorrent XMLRPC method {method}")),
    }
}

pub async fn execute_xml(state: &AppState, request: &str) -> String {
    match parse_method_call(request) {
        Ok((method, params)) => match execute(state, &method, &params).await {
            Ok(value) => method_response(&value),
            Err(message) => fault_response(1, &message),
        },
        Err(message) => fault_response(1, &message),
    }
}

async fn d_read_or_write(
    state: &AppState,
    method: &str,
    params: &[RtValue],
) -> Result<RtValue, String> {
    let hash = params
        .first()
        .and_then(RtValue::as_str)
        .ok_or_else(|| format!("{method} requires info hash"))?;
    if method == "d.custom.set" {
        let key = params.get(1).and_then(RtValue::as_str).unwrap_or_default();
        let value = params
            .get(2)
            .cloned()
            .unwrap_or(RtValue::String(String::new()));
        state
            .custom
            .write()
            .await
            .entry(hash.to_owned())
            .or_default()
            .insert(key.to_owned(), value);
        return Ok(RtValue::Int(0));
    }
    if method == "d.down.max_rate.set" || method == "d.up.max_rate.set" {
        let value = params.get(1).and_then(rt_value_i64).unwrap_or(0).max(0);
        set_torrent_limit(state, hash, method == "d.down.max_rate.set", value).await?;
        return Ok(RtValue::Int(0));
    }
    let limits = torrent_limits(state, hash).await;
    let registry = state.registry.read().await;
    let entry = registry
        .get(hash)
        .ok_or_else(|| format!("torrent not found: {hash}"))?;
    Ok(project_download_field(
        entry,
        method,
        limits.as_ref(),
        state.custom.read().await.get(hash),
        params.get(1).and_then(RtValue::as_str),
    ))
}

fn project_download_field(
    entry: &TorrentEntry,
    method: &str,
    limits: Option<&EngineTorrentLimits>,
    custom: Option<&BTreeMap<String, RtValue>>,
    custom_key: Option<&str>,
) -> RtValue {
    match method {
        "d.hash" => RtValue::String(entry.info_hash.clone()),
        "d.name" => RtValue::String(entry.name.clone()),
        "d.base_path" | "d.directory" => RtValue::String(entry.save_path.clone()),
        "d.size_bytes" => RtValue::Int(entry.total_length as i64),
        "d.left_bytes" => RtValue::Int(entry.amount_left as i64),
        "d.completed_bytes" => {
            RtValue::Int(entry.total_length.saturating_sub(entry.amount_left) as i64)
        }
        "d.complete" => RtValue::Bool(entry.total_length > 0 && entry.amount_left == 0),
        "d.is_active" => RtValue::Bool(matches!(
            entry.state.as_str(),
            "downloading" | "seeding" | "checking"
        )),
        "d.state" => RtValue::String(entry.state.as_str().to_owned()),
        "d.state_changed" => RtValue::Int(entry.added_at as i64),
        "d.up.total" => RtValue::Int(entry.stats.uploaded as i64),
        "d.down.total" => RtValue::Int(entry.stats.downloaded as i64),
        "d.down.max_rate" => {
            RtValue::Int(limits.and_then(|limits| limits.download_limit).unwrap_or(0))
        }
        "d.up.max_rate" => RtValue::Int(limits.and_then(|limits| limits.upload_limit).unwrap_or(0)),
        "d.ratio" => RtValue::Int((entry.stats.ratio() * 1000.0).round() as i64),
        "d.custom" => custom
            .and_then(|values| custom_key.and_then(|key| values.get(key)))
            .cloned()
            .unwrap_or_else(|| RtValue::String(String::new())),
        _ => RtValue::Nil,
    }
}

async fn d_multicall(state: &AppState, params: &[RtValue]) -> Result<RtValue, String> {
    let commands = params
        .iter()
        .skip(1)
        .filter_map(RtValue::as_str)
        .map(|command| command.trim_end_matches('=').to_owned())
        .collect::<Vec<_>>();
    let torrent_count = state.registry.read().await.len();
    let _lease = reserve_rtorrent_api_snapshot(
        state,
        estimate_rtorrent_multicall_snapshot_bytes(torrent_count, commands.len()),
    )
    .await?;
    let registry = state.registry.read().await;
    let custom = state.custom.read().await;
    let local_limits = state.torrent_limits.read().await.clone();
    let mut rows = Vec::new();
    for entry in registry.iter() {
        let engine_limits = if let Some(engine) = &state.engine {
            engine.torrent_limits(entry.info_hash.clone()).await.ok()
        } else {
            None
        };
        let limits = engine_limits
            .as_ref()
            .or_else(|| local_limits.get(&entry.info_hash));
        let row = commands
            .iter()
            .map(|command| {
                project_download_field(entry, command, limits, custom.get(&entry.info_hash), None)
            })
            .collect();
        rows.push(RtValue::Array(row));
    }
    Ok(RtValue::Array(rows))
}

async fn file_multicall(state: &AppState, params: &[RtValue]) -> Result<RtValue, String> {
    let commands = multicall_commands(params);
    let Some(entry) = selected_torrent_entry(state, params).await else {
        return Ok(RtValue::Array(Vec::new()));
    };
    let meta = torrent_metadata_snapshot(state, &entry.info_hash).await;
    let rows = if let Some(meta) = meta.as_ref() {
        meta.files
            .iter()
            .map(|file| {
                RtValue::Array(
                    commands
                        .iter()
                        .map(|command| project_file_field(file, command, meta))
                        .collect(),
                )
            })
            .collect()
    } else {
        vec![RtValue::Array(
            commands
                .iter()
                .map(|command| project_registry_file_field(&entry, command))
                .collect(),
        )]
    };
    Ok(RtValue::Array(rows))
}

async fn tracker_multicall(state: &AppState, params: &[RtValue]) -> Result<RtValue, String> {
    let commands = multicall_commands(params);
    let Some(entry) = selected_torrent_entry(state, params).await else {
        return Ok(RtValue::Array(Vec::new()));
    };
    if let Some(engine) = &state.engine {
        if let Ok(trackers) = engine.torrent_trackers(entry.info_hash.clone()).await {
            return Ok(RtValue::Array(
                trackers
                    .iter()
                    .map(|tracker| {
                        RtValue::Array(
                            commands
                                .iter()
                                .map(|command| project_tracker_snapshot_field(tracker, command))
                                .collect(),
                        )
                    })
                    .collect(),
            ));
        }
    }
    let Some(meta) = torrent_metadata_snapshot(state, &entry.info_hash).await else {
        return Ok(RtValue::Array(Vec::new()));
    };
    Ok(RtValue::Array(
        meta.trackers
            .iter()
            .enumerate()
            .map(|(idx, tracker)| {
                RtValue::Array(
                    commands
                        .iter()
                        .map(|command| project_tracker_field(idx, tracker, command))
                        .collect(),
                )
            })
            .collect(),
    ))
}

async fn peer_multicall(state: &AppState, params: &[RtValue]) -> Result<RtValue, String> {
    let commands = multicall_commands(params);
    let Some(entry) = selected_torrent_entry(state, params).await else {
        return Ok(RtValue::Array(Vec::new()));
    };
    let Some(engine) = &state.engine else {
        return Ok(RtValue::Array(Vec::new()));
    };
    let peers = engine
        .torrent_peers(entry.info_hash)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|peer| {
            RtValue::Array(
                commands
                    .iter()
                    .map(|command| project_peer_field(&peer, command))
                    .collect(),
            )
        })
        .collect();
    Ok(RtValue::Array(peers))
}

async fn selected_torrent_entry(state: &AppState, params: &[RtValue]) -> Option<TorrentEntry> {
    let registry = state.registry.read().await;
    params
        .iter()
        .filter_map(RtValue::as_str)
        .find_map(|value| registry.get(value).cloned())
        .or_else(|| registry.iter().next().cloned())
}

async fn torrent_metadata_snapshot(state: &AppState, hash: &str) -> Option<EngineTorrentMetadata> {
    let engine = state.engine.as_ref()?;
    engine.torrent_metadata(hash.to_owned()).await.ok()
}

async fn global_down_limit(state: &AppState) -> i64 {
    if let Some(engine) = &state.engine {
        return engine
            .global_limits()
            .await
            .map(|limits| limits.download_limit)
            .unwrap_or(0);
    }
    *state.global_down_limit.read().await
}

async fn global_up_limit(state: &AppState) -> i64 {
    if let Some(engine) = &state.engine {
        return engine
            .global_limits()
            .await
            .map(|limits| limits.upload_limit)
            .unwrap_or(0);
    }
    *state.global_up_limit.read().await
}

async fn torrent_limits(state: &AppState, hash: &str) -> Option<EngineTorrentLimits> {
    if let Some(engine) = &state.engine {
        if let Ok(limits) = engine.torrent_limits(hash.to_owned()).await {
            return Some(limits);
        }
    }
    state.torrent_limits.read().await.get(hash).cloned()
}

async fn set_torrent_limit(
    state: &AppState,
    hash: &str,
    download: bool,
    value: i64,
) -> Result<(), String> {
    let mut limits = torrent_limits(state, hash).await.unwrap_or_default();
    if download {
        limits.download_limit = (value > 0).then_some(value);
    } else {
        limits.upload_limit = (value > 0).then_some(value);
    }
    state
        .torrent_limits
        .write()
        .await
        .insert(hash.to_owned(), limits.clone());
    if let Some(engine) = &state.engine {
        engine
            .update_torrent_limits(hash.to_owned(), limits)
            .await?;
    }
    Ok(())
}

async fn set_global_limit(
    state: &AppState,
    params: &[RtValue],
    download: bool,
) -> Result<RtValue, String> {
    let value = params.first().and_then(rt_value_i64).unwrap_or(0).max(0);

    if let Some(engine) = &state.engine {
        let mut limits = engine.global_limits().await.unwrap_or_default();
        if download {
            limits.download_limit = value;
        } else {
            limits.upload_limit = value;
        }
        engine
            .update_global_limits(EngineGlobalLimits {
                speed_limits_mode: limits.speed_limits_mode,
                ..limits
            })
            .await?;
    } else if download {
        *state.global_down_limit.write().await = value;
    } else {
        *state.global_up_limit.write().await = value;
    }
    Ok(RtValue::Int(0))
}

fn rt_value_i64(value: &RtValue) -> Option<i64> {
    match value {
        RtValue::Int(value) => Some(*value),
        RtValue::Bool(value) => Some(i64::from(*value)),
        RtValue::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn default_rtorrent_views() -> BTreeSet<String> {
    rtorrent_builtin_views()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn rtorrent_builtin_views() -> [&'static str; 5] {
    ["main", "started", "stopped", "complete", "incomplete"]
}

async fn rtorrent_views(state: &AppState) -> Vec<RtValue> {
    let views = state
        .views
        .read()
        .await
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut ordered = rtorrent_builtin_views()
        .into_iter()
        .filter(|view| views.contains(*view))
        .map(|view| RtValue::String(view.to_owned()))
        .collect::<Vec<_>>();
    ordered.extend(
        views
            .into_iter()
            .filter(|view| !rtorrent_builtin_views().contains(&view.as_str()))
            .map(RtValue::String),
    );
    ordered
}

async fn rtorrent_view_add(state: &AppState, params: &[RtValue]) -> Result<RtValue, String> {
    let Some(view) = params.iter().find_map(RtValue::as_str).map(str::trim) else {
        return Err("view name required".to_owned());
    };
    if view.is_empty() {
        return Err("view name required".to_owned());
    }
    state.views.write().await.insert(view.to_owned());
    Ok(RtValue::Int(0))
}

async fn view_size(state: &AppState, params: &[RtValue]) -> i64 {
    let view = params.first().and_then(RtValue::as_str).unwrap_or("main");
    let registry = state.registry.read().await;
    registry
        .iter()
        .filter(|entry| rtorrent_view_matches(entry, view))
        .count() as i64
}

fn rtorrent_view_matches(entry: &TorrentEntry, view: &str) -> bool {
    match view {
        "main" | "" => true,
        "started" => matches!(
            entry.state.as_str(),
            "downloading" | "seeding" | "checking" | "metadata_pending"
        ),
        "stopped" => matches!(entry.state.as_str(), "paused" | "stopped"),
        "complete" => entry.total_length > 0 && entry.amount_left == 0,
        "incomplete" => entry.amount_left > 0,
        _ => true,
    }
}

fn multicall_commands(params: &[RtValue]) -> Vec<String> {
    let commands = params
        .iter()
        .filter_map(RtValue::as_str)
        .filter(|value| value.ends_with('='))
        .map(|command| command.trim_end_matches('=').to_owned())
        .collect::<Vec<_>>();
    if commands.is_empty() {
        vec!["".to_owned()]
    } else {
        commands
    }
}

fn project_registry_file_field(entry: &TorrentEntry, command: &str) -> RtValue {
    match command {
        "" | "f.path" | "f.frozen_path" => RtValue::String(entry.name.clone()),
        "f.size_bytes" => RtValue::Int(entry.total_length as i64),
        "f.completed_bytes" => {
            RtValue::Int(entry.total_length.saturating_sub(entry.amount_left) as i64)
        }
        "f.priority" => RtValue::Int(1),
        "f.is_created" | "f.is_open" => RtValue::Bool(true),
        "f.is_complete" => RtValue::Bool(entry.total_length > 0 && entry.amount_left == 0),
        "f.offset" | "f.range_first" | "f.range_second" => RtValue::Int(0),
        _ => RtValue::Nil,
    }
}

fn project_file_field(
    file: &EngineTorrentFile,
    command: &str,
    meta: &EngineTorrentMetadata,
) -> RtValue {
    match command {
        "" | "f.path" | "f.frozen_path" => RtValue::String(file.path.clone()),
        "f.size_bytes" => RtValue::Int(file.length as i64),
        "f.priority" => RtValue::Int(file.priority),
        "f.is_created" | "f.is_open" => RtValue::Bool(true),
        "f.is_complete" => RtValue::Bool(file_is_complete(file, meta)),
        "f.completed_bytes" => {
            if file_is_complete(file, meta) {
                RtValue::Int(file.length as i64)
            } else {
                RtValue::Int(0)
            }
        }
        "f.offset" => RtValue::Int(file_start_offset(file, meta) as i64),
        "f.range_first" => RtValue::Int(file_first_piece(file, meta) as i64),
        "f.range_second" => RtValue::Int(file_last_piece(file, meta) as i64),
        _ => RtValue::Nil,
    }
}

fn file_start_offset(file: &EngineTorrentFile, meta: &EngineTorrentMetadata) -> u64 {
    meta.files
        .iter()
        .filter(|candidate| candidate.index < file.index)
        .map(|candidate| candidate.length)
        .sum()
}

fn file_first_piece(file: &EngineTorrentFile, meta: &EngineTorrentMetadata) -> usize {
    if meta.piece_length == 0 {
        return 0;
    }
    (file_start_offset(file, meta) / meta.piece_length) as usize
}

fn file_last_piece(file: &EngineTorrentFile, meta: &EngineTorrentMetadata) -> usize {
    if meta.piece_length == 0 || file.length == 0 {
        return file_first_piece(file, meta);
    }
    ((file_start_offset(file, meta) + file.length - 1) / meta.piece_length) as usize
}

fn file_is_complete(file: &EngineTorrentFile, meta: &EngineTorrentMetadata) -> bool {
    if meta.piece_states.is_empty() {
        return false;
    }
    let first = file_first_piece(file, meta);
    if first >= meta.piece_states.len() {
        return false;
    }
    let last = file_last_piece(file, meta).min(meta.piece_states.len().saturating_sub(1));
    meta.piece_states[first..=last]
        .iter()
        .all(|state| matches!(state, rt_engine::EnginePieceState::Complete))
}

fn project_tracker_field(idx: usize, tracker: &str, command: &str) -> RtValue {
    match command {
        "" | "t.url" | "t.group" => RtValue::String(tracker.to_owned()),
        "t.is_enabled" | "t.is_open" => RtValue::Bool(true),
        "t.type" | "t.id" => RtValue::Int(idx as i64),
        "t.latest_event"
        | "t.latest_sum_peers"
        | "t.scrape_complete"
        | "t.scrape_incomplete"
        | "t.scrape_downloaded" => RtValue::Int(0),
        _ => RtValue::Nil,
    }
}

fn project_tracker_snapshot_field(tracker: &EngineTrackerSnapshot, command: &str) -> RtValue {
    match command {
        "" | "t.url" | "t.group" => RtValue::String(tracker.announce.clone()),
        "t.is_enabled" | "t.is_open" => RtValue::Bool(true),
        "t.type" | "t.id" => RtValue::Int(tracker.id),
        "t.latest_event" => RtValue::String(tracker.status.clone()),
        "t.latest_sum_peers" => RtValue::Int(
            tracker
                .seeders
                .unwrap_or(0)
                .saturating_add(tracker.leechers.unwrap_or(0)),
        ),
        "t.scrape_complete" => RtValue::Int(tracker.seeders.unwrap_or(0)),
        "t.scrape_incomplete" => RtValue::Int(tracker.leechers.unwrap_or(0)),
        "t.scrape_downloaded" => RtValue::Int(tracker.completed.unwrap_or(0)),
        _ => RtValue::Nil,
    }
}

fn project_peer_field(peer: &EnginePeerSnapshot, command: &str) -> RtValue {
    match command {
        "" | "p.address" => RtValue::String(peer.addr.ip().to_string()),
        "p.port" => RtValue::Int(peer.addr.port() as i64),
        "p.client_version" => RtValue::String(peer.client.clone()),
        "p.completed_percent" => RtValue::Int((peer.progress * 100.0).round() as i64),
        "p.down_rate" | "p.down_rate_total" => RtValue::Int(peer.download_rate),
        "p.up_rate" | "p.up_rate_total" => RtValue::Int(peer.upload_rate),
        "p.completed_chunks" => RtValue::Int(peer.pieces as i64),
        "p.is_encrypted" | "p.is_incoming" => RtValue::Bool(false),
        "p.is_interested" => RtValue::Bool(peer.interested),
        "p.is_choked" => RtValue::Bool(peer.choked),
        _ => RtValue::Nil,
    }
}

async fn load(state: &AppState, method: &str, params: &[RtValue]) -> Result<RtValue, String> {
    let payload = params
        .first()
        .and_then(RtValue::as_str)
        .ok_or_else(|| "load requires magnet URI, torrent bytes, or path".to_owned())?;
    let mut entry = if payload.starts_with("magnet:") {
        let magnet = parse_magnet(payload).map_err(|err| err.to_string())?;
        let hash = magnet
            .info_hash_v1
            .map(hex_lower)
            .or_else(|| magnet.info_hash_v2.map(hex_lower))
            .ok_or_else(|| "magnet missing supported info hash".to_owned())?;
        TorrentEntry::new(
            hash,
            magnet.display_name.unwrap_or_else(|| "magnet".to_owned()),
            String::new(),
        )
    } else if let Some(bytes) = load_torrent_bytes(method, payload) {
        let parsed = parse_torrent(&bytes).map_err(|err| err.to_string())?;
        let hash = parsed
            .v1_info_hash()
            .map(hex_lower)
            .or_else(|| parsed.v2_info_hash().map(hex_lower))
            .ok_or_else(|| "torrent missing supported info hash".to_owned())?;
        TorrentEntry::new(hash, parsed.name().to_owned(), String::new())
    } else {
        return Ok(RtValue::Int(0));
    };
    if method.ends_with("start") {
        let _ = entry.transition(rt_session::TorrentState::Downloading);
    }
    let _ = state.registry.write().await.add(entry);
    Ok(RtValue::Int(0))
}

fn load_torrent_bytes(method: &str, payload: &str) -> Option<Vec<u8>> {
    if method.contains(".raw") {
        return general_purpose::STANDARD.decode(payload).ok();
    }
    std::fs::read(payload).ok()
}

enum Lifecycle {
    Erase,
    Pause,
    Resume,
}

async fn lifecycle(
    state: &AppState,
    params: &[RtValue],
    lifecycle: Lifecycle,
) -> Result<RtValue, String> {
    let hash = params
        .first()
        .and_then(RtValue::as_str)
        .ok_or_else(|| "lifecycle command requires info hash".to_owned())?;
    if let Some(engine) = &state.engine {
        match lifecycle {
            Lifecycle::Erase => {
                let _ = engine.remove_torrent(hash.to_owned(), false).await;
            }
            Lifecycle::Pause => {
                let _ = engine.pause_torrent(hash.to_owned()).await;
            }
            Lifecycle::Resume => {
                let _ = engine.resume_torrent(hash.to_owned()).await;
            }
        }
    }
    if matches!(lifecycle, Lifecycle::Erase) {
        let _ = state.registry.write().await.remove(hash);
    }
    Ok(RtValue::Int(0))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn hex_lower<const N: usize>(bytes: [u8; N]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_method_call(xml: &str) -> Result<(String, Vec<RtValue>), String> {
    let method = between(xml, "<methodName>", "</methodName>")
        .ok_or_else(|| "XMLRPC request missing methodName".to_owned())?;
    let mut params = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<param>") {
        rest = &rest[start + "<param>".len()..];
        let Some(end) = rest.find("</param>") else {
            break;
        };
        params.push(parse_value(&rest[..end]));
        rest = &rest[end + "</param>".len()..];
    }
    Ok((xml_unescape(method), params))
}

fn parse_value(xml: &str) -> RtValue {
    let xml = xml.trim();
    if xml.starts_with("<value>") && xml.ends_with("</value>") {
        return parse_value(&xml["<value>".len()..xml.len() - "</value>".len()]);
    }
    if let Some(value) = between(xml, "<array>", "</array>") {
        let data = between(value, "<data>", "</data>").unwrap_or(value);
        return RtValue::Array(parse_value_nodes(data));
    }
    if let Some(value) = between(xml, "<struct>", "</struct>") {
        return RtValue::Struct(parse_struct_members(value));
    }
    if let Some(value) = between(xml, "<string>", "</string>") {
        return RtValue::String(xml_unescape(value));
    }
    if let Some(value) = between(xml, "<base64>", "</base64>") {
        return RtValue::String(value.trim().to_owned());
    }
    if let Some(value) = between(xml, "<i4>", "</i4>").or_else(|| between(xml, "<int>", "</int>")) {
        return RtValue::Int(value.trim().parse().unwrap_or_default());
    }
    if let Some(value) = between(xml, "<boolean>", "</boolean>") {
        return RtValue::Bool(value.trim() == "1" || value.trim().eq_ignore_ascii_case("true"));
    }
    if xml.contains("<nil/>") {
        return RtValue::Nil;
    }
    RtValue::String(xml_unescape(xml))
}

fn parse_value_nodes(mut xml: &str) -> Vec<RtValue> {
    let mut values = Vec::new();
    while let Some((value, rest)) = next_value_node(xml) {
        values.push(parse_value(value));
        xml = rest;
    }
    values
}

fn next_value_node(xml: &str) -> Option<(&str, &str)> {
    let open = "<value>";
    let close = "</value>";
    let start = xml.find(open)? + open.len();
    let mut depth = 1usize;
    let mut pos = start;
    while depth > 0 {
        let next_open = xml[pos..].find(open).map(|idx| pos + idx);
        let next_close = xml[pos..].find(close).map(|idx| pos + idx)?;
        if let Some(next_open) = next_open {
            if next_open < next_close {
                depth += 1;
                pos = next_open + open.len();
                continue;
            }
        }
        depth -= 1;
        if depth == 0 {
            let rest = &xml[next_close + close.len()..];
            return Some((&xml[start..next_close], rest));
        }
        pos = next_close + close.len();
    }
    None
}

fn parse_struct_members(mut xml: &str) -> BTreeMap<String, RtValue> {
    let mut values = BTreeMap::new();
    while let Some(start) = xml.find("<member>") {
        xml = &xml[start + "<member>".len()..];
        let Some(end) = xml.find("</member>") else {
            break;
        };
        let member = &xml[..end];
        if let Some(name) = between(member, "<name>", "</name>") {
            let value = between(member, "<value>", "</value>")
                .map(parse_value)
                .unwrap_or(RtValue::Nil);
            values.insert(xml_unescape(name), value);
        }
        xml = &xml[end + "</member>".len()..];
    }
    values
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

fn method_response(value: &RtValue) -> String {
    format!(
        "<?xml version=\"1.0\"?><methodResponse><params><param><value>{}</value></param></params></methodResponse>",
        value_xml(value)
    )
}

fn fault_response(code: i64, message: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><methodResponse><fault><value><struct><member><name>faultCode</name><value><int>{code}</int></value></member><member><name>faultString</name><value><string>{}</string></value></member></struct></value></fault></methodResponse>",
        xml_escape(message)
    )
}

fn value_xml(value: &RtValue) -> String {
    match value {
        RtValue::Int(value) => format!("<int>{value}</int>"),
        RtValue::Bool(value) => format!("<boolean>{}</boolean>", if *value { 1 } else { 0 }),
        RtValue::String(value) => format!("<string>{}</string>", xml_escape(value)),
        RtValue::Array(values) => format!(
            "<array><data>{}</data></array>",
            values
                .iter()
                .map(|value| format!("<value>{}</value>", value_xml(value)))
                .collect::<String>()
        ),
        RtValue::Struct(values) => format!(
            "<struct>{}</struct>",
            values
                .iter()
                .map(|(key, value)| format!(
                    "<member><name>{}</name><value>{}</value></member>",
                    xml_escape(key),
                    value_xml(value)
                ))
                .collect::<String>()
        ),
        RtValue::Nil => "<nil/>".to_owned(),
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

pub fn value_to_json(value: &RtValue) -> Value {
    match value {
        RtValue::Int(value) => Value::from(*value),
        RtValue::Bool(value) => Value::from(*value),
        RtValue::String(value) => Value::from(value.clone()),
        RtValue::Array(values) => Value::Array(values.iter().map(value_to_json).collect()),
        RtValue::Struct(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), value_to_json(value)))
                .collect(),
        ),
        RtValue::Nil => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use rt_engine::{EnginePieceState, EngineTorrentFile, EngineTorrentMetadata};
    use rt_session::{TorrentEntry, TorrentState};

    async fn state_with_torrent() -> AppState {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut entry = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data/alpha".into());
            entry.total_length = 100;
            entry.amount_left = 25;
            entry.stats.add_download(75);
            entry.stats.add_upload(150);
            entry.transition(TorrentState::Downloading).unwrap();
            registry.write().await.add(entry).unwrap();
        }
        AppState::new(registry)
    }

    #[test]
    fn method_matrix_advertises_representative_rtorrent_families() {
        let methods = supported_methods();
        for method in [
            "system.client_version",
            "session.path",
            "network.port_open",
            "throttle.global_down.max_rate.set",
            "d.hash",
            "d.multicall2",
            "load.normal",
            "d.erase",
            "d.pause",
            "d.resume",
            "d.tracker_announce",
            "f.multicall",
            "t.multicall",
            "p.multicall",
        ] {
            assert!(methods.contains(&method), "missing {method}");
        }
    }

    #[tokio::test]
    async fn download_reads_project_registry_state() {
        let state = state_with_torrent().await;
        let hash = RtValue::String("a".repeat(40));
        assert_eq!(
            execute(&state, "d.name", &[hash.clone()]).await.unwrap(),
            RtValue::String("alpha".to_owned())
        );
        assert_eq!(
            execute(&state, "d.completed_bytes", &[hash.clone()])
                .await
                .unwrap(),
            RtValue::Int(75)
        );
        assert_eq!(
            execute(&state, "d.ratio", &[hash]).await.unwrap(),
            RtValue::Int(2000)
        );
    }

    #[tokio::test]
    async fn view_size_projects_registry_backed_compat_views() {
        let state = state_with_torrent().await;
        {
            let mut complete =
                TorrentEntry::new("b".repeat(40), "beta".into(), "/data/beta".into());
            complete.total_length = 10;
            complete.amount_left = 0;
            complete.transition(TorrentState::Downloading).unwrap();
            complete.transition(TorrentState::Seeding).unwrap();
            state.registry.write().await.add(complete).unwrap();
        }

        let views = execute(&state, "view.list", &[]).await.unwrap();
        assert_eq!(
            views,
            RtValue::Array(
                ["main", "started", "stopped", "complete", "incomplete"]
                    .into_iter()
                    .map(|view| RtValue::String(view.to_owned()))
                    .collect()
            )
        );
        assert_eq!(
            execute(&state, "view.size", &[RtValue::String("main".to_owned())])
                .await
                .unwrap(),
            RtValue::Int(2)
        );
        assert_eq!(
            execute(
                &state,
                "view.size",
                &[RtValue::String("complete".to_owned())]
            )
            .await
            .unwrap(),
            RtValue::Int(1)
        );
        assert_eq!(
            execute(
                &state,
                "view.size",
                &[RtValue::String("incomplete".to_owned())]
            )
            .await
            .unwrap(),
            RtValue::Int(1)
        );
        execute(&state, "view.add", &[RtValue::String("sonarr".to_owned())])
            .await
            .unwrap();
        assert_eq!(
            execute(&state, "view.size", &[RtValue::String("sonarr".to_owned())])
                .await
                .unwrap(),
            RtValue::Int(2)
        );
        let views = execute(&state, "view.list", &[]).await.unwrap();
        let RtValue::Array(views) = views else {
            panic!("expected view list")
        };
        assert!(views.contains(&RtValue::String("sonarr".to_owned())));
    }

    #[tokio::test]
    async fn custom_fields_roundtrip() {
        let state = state_with_torrent().await;
        let hash = RtValue::String("a".repeat(40));
        execute(
            &state,
            "d.custom.set",
            &[
                hash.clone(),
                RtValue::String("label".to_owned()),
                RtValue::String("movies".to_owned()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            execute(
                &state,
                "d.custom",
                &[hash, RtValue::String("label".to_owned())]
            )
            .await
            .unwrap(),
            RtValue::String("movies".to_owned())
        );
    }

    #[tokio::test]
    async fn global_throttle_setters_roundtrip_without_engine() {
        let state = state_with_torrent().await;

        assert_eq!(
            execute(
                &state,
                "throttle.global_down.max_rate.set",
                &[RtValue::Int(1234)]
            )
            .await
            .unwrap(),
            RtValue::Int(0)
        );
        assert_eq!(
            execute(
                &state,
                "throttle.global_up.max_rate.set",
                &[RtValue::String("5678".to_owned())]
            )
            .await
            .unwrap(),
            RtValue::Int(0)
        );
        assert_eq!(
            execute(&state, "throttle.global_down.max_rate", &[])
                .await
                .unwrap(),
            RtValue::Int(1234)
        );
        assert_eq!(
            execute(&state, "throttle.global_up.max_rate", &[])
                .await
                .unwrap(),
            RtValue::Int(5678)
        );
    }

    #[tokio::test]
    async fn torrent_throttle_setters_roundtrip_without_engine() {
        let state = state_with_torrent().await;
        let hash = RtValue::String("a".repeat(40));

        assert_eq!(
            execute(
                &state,
                "d.down.max_rate.set",
                &[hash.clone(), RtValue::Int(333)]
            )
            .await
            .unwrap(),
            RtValue::Int(0)
        );
        assert_eq!(
            execute(
                &state,
                "d.up.max_rate.set",
                &[hash.clone(), RtValue::String("444".to_owned())]
            )
            .await
            .unwrap(),
            RtValue::Int(0)
        );
        assert_eq!(
            execute(&state, "d.down.max_rate", &[hash.clone()])
                .await
                .unwrap(),
            RtValue::Int(333)
        );
        assert_eq!(
            execute(&state, "d.up.max_rate", &[hash.clone()])
                .await
                .unwrap(),
            RtValue::Int(444)
        );
        assert_eq!(
            execute(
                &state,
                "d.multicall2",
                &[
                    RtValue::String("main".to_owned()),
                    RtValue::String("d.down.max_rate=".to_owned()),
                    RtValue::String("d.up.max_rate=".to_owned()),
                ],
            )
            .await
            .unwrap(),
            RtValue::Array(vec![RtValue::Array(vec![
                RtValue::Int(333),
                RtValue::Int(444)
            ])])
        );
    }

    #[tokio::test]
    async fn multicall_returns_rtorrent_row_shape() {
        let state = state_with_torrent().await;
        let value = execute(
            &state,
            "d.multicall2",
            &[
                RtValue::String("main".to_owned()),
                RtValue::String("d.hash=".to_owned()),
                RtValue::String("d.name=".to_owned()),
                RtValue::String("d.left_bytes=".to_owned()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            value,
            RtValue::Array(vec![RtValue::Array(vec![
                RtValue::String("a".repeat(40)),
                RtValue::String("alpha".to_owned()),
                RtValue::Int(25),
            ])])
        );
    }

    #[tokio::test]
    async fn xmlrpc_fixture_roundtrips() {
        let state = state_with_torrent().await;
        let xml = format!(
            r#"<?xml version="1.0"?><methodCall><methodName>d.name</methodName><params><param><value><string>{}</string></value></param></params></methodCall>"#,
            "a".repeat(40)
        );
        let response = execute_xml(&state, &xml).await;
        assert!(response.contains("<methodResponse>"));
        assert!(response.contains("<string>alpha</string>"));
    }

    #[tokio::test]
    async fn xmlrpc_method_list_and_detail_multicalls_have_stable_shapes() {
        let state = state_with_torrent().await;
        let response = execute_xml(
            &state,
            r#"<?xml version="1.0"?><methodCall><methodName>method.list</methodName><params/></methodCall>"#,
        )
        .await;
        assert!(response.contains("<array><data>"));
        assert!(response.contains("<string>d.multicall2</string>"));
        assert!(response.contains("<string>p.multicall</string>"));

        assert_eq!(
            execute(&state, "t.multicall", &[RtValue::String("main".to_owned())])
                .await
                .unwrap(),
            RtValue::Array(Vec::new())
        );
        assert_eq!(
            execute(&state, "p.multicall", &[RtValue::String("main".to_owned())])
                .await
                .unwrap(),
            RtValue::Array(Vec::new())
        );
        assert_eq!(
            execute(&state, "f.multicall", &[RtValue::String("main".to_owned())])
                .await
                .unwrap(),
            RtValue::Array(vec![RtValue::Array(vec![RtValue::String(
                "alpha".to_owned()
            ),])])
        );
    }

    #[tokio::test]
    async fn file_multicall_projects_registry_fallback_fields() {
        let state = state_with_torrent().await;
        let value = execute(
            &state,
            "f.multicall",
            &[
                RtValue::String("a".repeat(40)),
                RtValue::String(String::new()),
                RtValue::String("f.path=".to_owned()),
                RtValue::String("f.size_bytes=".to_owned()),
                RtValue::String("f.completed_bytes=".to_owned()),
                RtValue::String("f.is_complete=".to_owned()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            value,
            RtValue::Array(vec![RtValue::Array(vec![
                RtValue::String("alpha".to_owned()),
                RtValue::Int(100),
                RtValue::Int(75),
                RtValue::Bool(false),
            ])])
        );
    }

    #[test]
    fn file_tracker_and_peer_projectors_use_native_snapshot_fields() {
        let meta = EngineTorrentMetadata {
            piece_length: 16,
            piece_count: 3,
            piece_hashes: vec![],
            piece_states: vec![
                EnginePieceState::Complete,
                EnginePieceState::Complete,
                EnginePieceState::Missing,
            ],
            is_private: false,
            trackers: vec!["udp://tracker.example:6969/announce".to_owned()],
            webseeds: vec![],
            comment: None,
            created_by: None,
            creation_date: None,
            files: vec![
                EngineTorrentFile {
                    index: 0,
                    path: "disc/a.bin".to_owned(),
                    length: 16,
                    priority: 2,
                    wanted: true,
                },
                EngineTorrentFile {
                    index: 1,
                    path: "disc/b.bin".to_owned(),
                    length: 32,
                    priority: 1,
                    wanted: true,
                },
            ],
        };
        assert_eq!(
            project_file_field(&meta.files[0], "f.path", &meta),
            RtValue::String("disc/a.bin".to_owned())
        );
        assert_eq!(
            project_file_field(&meta.files[0], "f.is_complete", &meta),
            RtValue::Bool(true)
        );
        assert_eq!(
            project_file_field(&meta.files[1], "f.range_first", &meta),
            RtValue::Int(1)
        );
        assert_eq!(
            project_file_field(&meta.files[1], "f.is_complete", &meta),
            RtValue::Bool(false)
        );
        assert_eq!(
            project_tracker_field(0, &meta.trackers[0], "t.url"),
            RtValue::String("udp://tracker.example:6969/announce".to_owned())
        );
        let tracker = EngineTrackerSnapshot {
            id: 3,
            tier: 1,
            announce: meta.trackers[0].clone(),
            status: "working".to_owned(),
            last_announce_at: Some(100),
            next_announce_at: Some(200),
            last_success_at: Some(90),
            failure_reason: None,
            warning_message: None,
            seeders: Some(8),
            leechers: Some(5),
            completed: Some(2),
        };
        assert_eq!(
            project_tracker_snapshot_field(&tracker, "t.latest_event"),
            RtValue::String("working".to_owned())
        );
        assert_eq!(
            project_tracker_snapshot_field(&tracker, "t.latest_sum_peers"),
            RtValue::Int(13)
        );
        assert_eq!(
            project_tracker_snapshot_field(&tracker, "t.scrape_complete"),
            RtValue::Int(8)
        );
        assert_eq!(
            project_tracker_snapshot_field(&tracker, "t.scrape_incomplete"),
            RtValue::Int(5)
        );
        assert_eq!(
            project_tracker_snapshot_field(&tracker, "t.scrape_downloaded"),
            RtValue::Int(2)
        );

        let peer = EnginePeerSnapshot {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)), 51413),
            client: "Transmission 4.0".to_owned(),
            choked: false,
            upload_choked: true,
            interested: true,
            pieces: 2,
            pieces_total: 3,
            progress: 2.0 / 3.0,
            download_rate: 12_000,
            upload_rate: 4_000,
            downloaded: 20,
            uploaded: 10,
        };
        assert_eq!(
            project_peer_field(&peer, "p.address"),
            RtValue::String("10.0.0.7".to_owned())
        );
        assert_eq!(project_peer_field(&peer, "p.port"), RtValue::Int(51413));
        assert_eq!(
            project_peer_field(&peer, "p.completed_percent"),
            RtValue::Int(67)
        );
    }

    #[test]
    fn xml_value_parser_accepts_nested_arrays_structs_base64_and_nil() {
        let value = parse_value(
            r#"<value><array><data>
              <value><string>alpha</string></value>
              <value><struct>
                <member><name>count</name><value><int>2</int></value></member>
                <member><name>raw</name><value><base64>YWJj</base64></value></member>
                <member><name>empty</name><value><nil/></value></member>
              </struct></value>
            </data></array></value>"#,
        );
        let json = value_to_json(&value);
        assert_eq!(json[0], "alpha");
        assert_eq!(json[1]["count"], 2);
        assert_eq!(json[1]["raw"], "YWJj");
        assert!(json[1]["empty"].is_null());
    }

    #[test]
    fn value_to_json_preserves_rtorrent_types() {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_owned(), RtValue::String("alpha".to_owned()));
        fields.insert("active".to_owned(), RtValue::Bool(true));
        fields.insert("ratio".to_owned(), RtValue::Int(2000));
        let json = value_to_json(&RtValue::Struct(fields));
        assert_eq!(json["name"], "alpha");
        assert_eq!(json["active"], true);
        assert_eq!(json["ratio"], 2000);
    }

    #[tokio::test]
    async fn magnet_load_and_erase_update_registry() {
        let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
        execute(
            &state,
            "load.normal",
            &[RtValue::String(format!(
                "magnet:?xt=urn:btih:{}&dn=loaded",
                "b".repeat(40)
            ))],
        )
        .await
        .unwrap();
        assert_eq!(state.registry.read().await.len(), 1);
        execute(&state, "d.erase", &[RtValue::String("b".repeat(40))])
            .await
            .unwrap();
        assert_eq!(state.registry.read().await.len(), 0);
    }

    #[test]
    fn xmlrpc_parser_accepts_array_struct_base64_and_nil_shapes() {
        let xml = r#"<value><array><data>
            <value><int>7</int></value>
            <value><boolean>1</boolean></value>
            <value><base64>YWJj</base64></value>
            <value><nil/></value>
            <value><struct><member><name>k</name><value><string>v</string></value></member></struct></value>
        </data></array></value>"#;
        assert_eq!(
            parse_value(xml),
            RtValue::Array(vec![
                RtValue::Int(7),
                RtValue::Bool(true),
                RtValue::String("YWJj".to_owned()),
                RtValue::Nil,
                RtValue::Struct(BTreeMap::from([(
                    "k".to_owned(),
                    RtValue::String("v".to_owned())
                )])),
            ])
        );
    }

    #[tokio::test]
    async fn raw_torrent_load_accepts_xmlrpc_base64_payload() {
        let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
        let raw = single_file_torrent("raw-test", 4);
        let xml = format!(
            r#"<?xml version="1.0"?><methodCall><methodName>load.raw_start</methodName><params><param><value><base64>{}</base64></value></param></params></methodCall>"#,
            general_purpose::STANDARD.encode(raw)
        );
        let response = execute_xml(&state, &xml).await;
        assert!(response.contains("<int>0</int>"), "{response}");

        let registry = state.registry.read().await;
        assert_eq!(registry.len(), 1);
        let entry = registry.iter().next().unwrap();
        assert_eq!(entry.name, "raw-test");
        assert_eq!(entry.state, TorrentState::Downloading);
    }

    #[test]
    fn rtorrent_api_snapshot_estimate_scales_with_torrents_and_commands() {
        assert_eq!(estimate_rtorrent_multicall_snapshot_bytes(0, 0), 8 * 1024);
        assert_eq!(
            estimate_rtorrent_multicall_snapshot_bytes(10, 0),
            8 * 1024 + 10 * (512 + 160)
        );
        assert!(
            estimate_rtorrent_multicall_snapshot_bytes(10, 20)
                > estimate_rtorrent_multicall_snapshot_bytes(10, 1)
        );
    }

    fn single_file_torrent(name: &str, length: i64) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"d4:infod6:lengthi");
        raw.extend_from_slice(length.to_string().as_bytes());
        raw.extend_from_slice(b"e4:name");
        raw.extend_from_slice(name.len().to_string().as_bytes());
        raw.extend_from_slice(b":");
        raw.extend_from_slice(name.as_bytes());
        raw.extend_from_slice(b"12:piece lengthi16384e6:pieces20:");
        raw.extend_from_slice(&[0_u8; 20]);
        raw.extend_from_slice(b"ee");
        raw
    }
}
