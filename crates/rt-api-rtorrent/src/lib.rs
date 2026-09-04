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
use tokio::{sync::RwLock, task::JoinSet};

// XMLRPC multicall has no page/cursor contract in this compatibility layer.
// Bound the legacy all-torrent projection instead of allowing a client to
// force an unbounded response and serial per-torrent actor queries.
const MAX_LEGACY_FULL_LIST_ENTRIES: usize = 10_000;
const RTORRENT_RUNTIME_PROJECTION_CONCURRENCY: usize = 64;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    /// Optional library-boundary credentials. `execute_xml` remains useful
    /// for explicit local-development states with no configured tokens, but a
    /// configured state cannot be driven through the unauthenticated helper.
    pub api_tokens: Arc<Vec<String>>,
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
            api_tokens: Arc::new(Vec::new()),
            session_path: String::new(),
            network_port: 0,
            global_down_limit: Arc::new(RwLock::new(0)),
            global_up_limit: Arc::new(RwLock::new(0)),
            torrent_limits: Arc::new(RwLock::new(BTreeMap::new())),
            custom: Arc::new(RwLock::new(BTreeMap::new())),
            views: Arc::new(RwLock::new(default_rtorrent_views())),
        }
    }

    pub fn with_tokens(mut self, api_tokens: Vec<String>) -> Self {
        self.api_tokens = Arc::new(api_tokens);
        self
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

async fn execute(state: &AppState, method: &str, params: &[RtValue]) -> Result<RtValue, String> {
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
        "throttle.global_down.max_rate" => global_down_limit(state).await.map(RtValue::Int),
        "throttle.global_up.max_rate" => global_up_limit(state).await.map(RtValue::Int),
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
        "d.tracker_announce" => tracker_announce(state, params).await,
        "f.multicall" => file_multicall(state, params).await,
        "t.multicall" => tracker_multicall(state, params).await,
        "p.multicall" => peer_multicall(state, params).await,
        _ if method.starts_with("d.") => d_read_or_write(state, method, params).await,
        _ => Err(format!("unsupported rTorrent XMLRPC method {method}")),
    }
}

pub async fn execute_xml(state: &AppState, request: &str) -> String {
    execute_xml_with_token(state, request, None).await
}

/// Execute an XML-RPC request with an explicit library-boundary credential.
/// The daemon does not mount this facade; callers that embed it directly must
/// use this entry point when `AppState::with_tokens` is configured.
pub async fn execute_xml_with_token(
    state: &AppState,
    request: &str,
    presented_token: Option<&str>,
) -> String {
    if !state.api_tokens.is_empty()
        && !presented_token
            .is_some_and(|token| state.api_tokens.iter().any(|allowed| allowed == token))
    {
        return fault_response(401, "unauthorized");
    }
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
    let hash_key = canonical_hash_key(hash);
    if method == "d.custom.set" {
        let key = params
            .get(1)
            .and_then(RtValue::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| "d.custom.set requires a non-empty field name".to_owned())?;
        let value = params
            .get(2)
            .cloned()
            .ok_or_else(|| "d.custom.set requires a value".to_owned())?;
        if state.registry.read().await.get(hash).is_none() {
            return Err(format!("torrent not found: {hash}"));
        }
        state
            .custom
            .write()
            .await
            .entry(hash_key.clone())
            .or_default()
            .insert(key.to_owned(), value);
        return Ok(RtValue::Int(0));
    }
    if method == "d.down.max_rate.set" || method == "d.up.max_rate.set" {
        let value = params
            .get(1)
            .and_then(rt_value_i64)
            .ok_or_else(|| format!("{method} requires a numeric rate"))?
            .max(0);
        set_torrent_limit(state, hash, method == "d.down.max_rate.set", value).await?;
        return Ok(RtValue::Int(0));
    }
    let limits = torrent_limits(state, hash).await?;
    let registry = state.registry.read().await;
    let entry = registry
        .get(hash)
        .ok_or_else(|| format!("torrent not found: {hash}"))?;
    Ok(project_download_field(
        &entry,
        method,
        limits.as_ref(),
        state.custom.read().await.get(&hash_key),
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
        "d.size_bytes" => RtValue::Int(rt_i64(entry.total_length)),
        "d.left_bytes" => RtValue::Int(rt_i64(entry.amount_left)),
        "d.completed_bytes" => {
            RtValue::Int(rt_i64(entry.total_length.saturating_sub(entry.amount_left)))
        }
        "d.complete" => RtValue::Bool(entry.total_length > 0 && entry.amount_left == 0),
        "d.is_active" => RtValue::Bool(matches!(
            entry.state.as_str(),
            "downloading" | "seeding" | "checking"
        )),
        "d.state" => RtValue::String(entry.state.as_str().to_owned()),
        "d.state_changed" => RtValue::Int(rt_i64(entry.added_at)),
        "d.up.total" => RtValue::Int(rt_i64(entry.stats.uploaded)),
        "d.down.total" => RtValue::Int(rt_i64(entry.stats.downloaded)),
        "d.down.max_rate" => {
            RtValue::Int(limits.and_then(|limits| limits.download_limit).unwrap_or(0))
        }
        "d.up.max_rate" => RtValue::Int(limits.and_then(|limits| limits.upload_limit).unwrap_or(0)),
        "d.ratio" => RtValue::Int(rt_ratio_milli(entry.stats.uploaded, entry.stats.downloaded)),
        "d.custom" => custom
            .and_then(|values| custom_key.and_then(|key| values.get(key)))
            .cloned()
            .unwrap_or_else(|| RtValue::String(String::new())),
        _ => RtValue::Nil,
    }
}

async fn d_multicall(state: &AppState, params: &[RtValue]) -> Result<RtValue, String> {
    let view = params
        .first()
        .and_then(RtValue::as_str)
        .map(str::trim)
        .filter(|view| !view.is_empty())
        .ok_or_else(|| "d.multicall requires a non-empty view".to_owned())?;
    let commands = d_multicall_commands(params)?;
    let snapshot = {
        let registry = state.registry.read().await;
        registry
            .snapshot()
            .iter()
            .filter(|entry| rtorrent_view_matches(entry, view))
            .cloned()
            .collect::<Vec<_>>()
    };
    if snapshot.len() > MAX_LEGACY_FULL_LIST_ENTRIES {
        return Err(format!(
            "rTorrent d.multicall full-list response has {} torrents; maximum is {MAX_LEGACY_FULL_LIST_ENTRIES}; use the native paged API",
            snapshot.len()
        ));
    }
    let _lease = reserve_rtorrent_api_snapshot(
        state,
        estimate_rtorrent_multicall_snapshot_bytes(snapshot.len(), commands.len()),
    )
    .await?;
    let custom = state.custom.read().await.clone();
    let local_limits = state.torrent_limits.read().await.clone();
    let need_limits = commands.iter().any(|command| {
        matches!(
            command.as_str(),
            "d.down.max_rate" | "d.up.max_rate" | "d.down.max_rate.set" | "d.up.max_rate.set"
        )
    });
    let engine_limits = if need_limits {
        if let Some(engine) = &state.engine {
            Some(load_rtorrent_limit_projections(engine.clone(), &snapshot).await?)
        } else {
            None
        }
    } else {
        None
    };
    let mut rows = Vec::with_capacity(snapshot.len());
    for entry in snapshot.iter() {
        let limits = engine_limits
            .as_ref()
            .and_then(|limits| limits.get(&entry.info_hash))
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

async fn load_rtorrent_limit_projections(
    engine: EngineHandle,
    entries: &[TorrentEntry],
) -> Result<BTreeMap<String, EngineTorrentLimits>, String> {
    let mut limits = BTreeMap::new();
    for batch in entries.chunks(RTORRENT_RUNTIME_PROJECTION_CONCURRENCY) {
        let mut tasks = JoinSet::new();
        for entry in batch {
            let hash = entry.info_hash.clone();
            let engine = engine.clone();
            tasks.spawn(async move {
                let limits = engine
                    .torrent_limits(hash.clone())
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>((hash, limits))
            });
        }
        while let Some(result) = tasks.join_next().await {
            let (hash, projection) = result
                .map_err(|error| format!("rTorrent runtime projection task failed: {error}"))??;
            limits.insert(hash, projection);
        }
    }
    Ok(limits)
}

async fn file_multicall(state: &AppState, params: &[RtValue]) -> Result<RtValue, String> {
    let commands = multicall_commands(params)?;
    let Some(entry) = selected_torrent_entry(state, params).await else {
        return Ok(RtValue::Array(Vec::new()));
    };
    let meta = torrent_metadata_snapshot(state, &entry.info_hash).await?;
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
    let commands = multicall_commands(params)?;
    let Some(entry) = selected_torrent_entry(state, params).await else {
        return Ok(RtValue::Array(Vec::new()));
    };
    if let Some(engine) = &state.engine {
        let trackers = engine
            .torrent_trackers(entry.info_hash.clone())
            .await
            .map_err(|error| error.to_string())?;
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
    let Some(meta) = torrent_metadata_snapshot(state, &entry.info_hash).await? else {
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
    let commands = multicall_commands(params)?;
    let Some(entry) = selected_torrent_entry(state, params).await else {
        return Ok(RtValue::Array(Vec::new()));
    };
    let Some(engine) = &state.engine else {
        return Ok(RtValue::Array(Vec::new()));
    };
    let peers = engine
        .torrent_peers(entry.info_hash)
        .await
        .map_err(|error| error.to_string())?
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
    let snapshot = registry.snapshot();
    params
        .iter()
        .filter_map(RtValue::as_str)
        .find_map(|value| snapshot.find(value).cloned())
        .or_else(|| snapshot.get(0).cloned())
}

async fn torrent_metadata_snapshot(
    state: &AppState,
    hash: &str,
) -> Result<Option<EngineTorrentMetadata>, String> {
    let Some(engine) = state.engine.as_ref() else {
        return Ok(None);
    };
    engine
        .torrent_metadata(hash.to_owned())
        .await
        .map(Some)
        .map_err(|error| error.to_string())
}

async fn global_down_limit(state: &AppState) -> Result<i64, String> {
    if let Some(engine) = &state.engine {
        return engine
            .global_limits()
            .await
            .map(|limits| limits.download_limit)
            .map_err(|error| error.to_string());
    }
    Ok(*state.global_down_limit.read().await)
}

async fn global_up_limit(state: &AppState) -> Result<i64, String> {
    if let Some(engine) = &state.engine {
        return engine
            .global_limits()
            .await
            .map(|limits| limits.upload_limit)
            .map_err(|error| error.to_string());
    }
    Ok(*state.global_up_limit.read().await)
}

async fn torrent_limits(
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
    let hash = canonical_hash_key(hash);
    Ok(state.torrent_limits.read().await.get(&hash).cloned())
}

async fn set_torrent_limit(
    state: &AppState,
    hash: &str,
    download: bool,
    value: i64,
) -> Result<(), String> {
    if state.engine.is_none() && state.registry.read().await.get(hash).is_none() {
        return Err(format!("torrent not found: {hash}"));
    }
    let mut limits = torrent_limits(state, hash).await?.unwrap_or_default();
    if download {
        limits.download_limit = (value > 0).then_some(value);
    } else {
        limits.upload_limit = (value > 0).then_some(value);
    }
    if let Some(engine) = &state.engine {
        engine
            .update_torrent_limits(hash.to_owned(), limits.clone())
            .await?;
    }
    let hash = canonical_hash_key(hash);
    state.torrent_limits.write().await.insert(hash, limits);
    Ok(())
}

async fn set_global_limit(
    state: &AppState,
    params: &[RtValue],
    download: bool,
) -> Result<RtValue, String> {
    let value = params
        .first()
        .and_then(rt_value_i64)
        .ok_or_else(|| "global throttle setter requires a numeric rate".to_owned())?
        .max(0);

    if let Some(engine) = &state.engine {
        let mut limits = engine
            .global_limits()
            .await
            .map_err(|error| error.to_string())?;
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
        .count()
        .try_into()
        .unwrap_or(i64::MAX)
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

fn d_multicall_commands(params: &[RtValue]) -> Result<Vec<String>, String> {
    if params.len() < 2 {
        return Err("d.multicall requires a view and at least one command".to_owned());
    }
    let mut commands = Vec::with_capacity(params.len() - 1);
    for (index, value) in params.iter().enumerate().skip(1) {
        let command = value
            .as_str()
            .ok_or_else(|| format!("d.multicall command {index} must be a string"))?;
        let command = command
            .strip_suffix('=')
            .ok_or_else(|| format!("d.multicall command {index} must end with '='"))?
            .trim();
        if command.is_empty() || command.contains('=') {
            return Err(format!("d.multicall command {index} is invalid"));
        }
        commands.push(command.to_owned());
    }
    Ok(commands)
}

fn multicall_commands(params: &[RtValue]) -> Result<Vec<String>, String> {
    let mut commands = Vec::new();
    let mut command_section = false;
    for (index, value) in params.iter().enumerate() {
        let Some(value) = value.as_str() else {
            if command_section {
                return Err(format!("multicall command {index} must be a string"));
            }
            continue;
        };
        if value.is_empty() {
            if command_section {
                commands.push(String::new());
            }
            continue;
        }
        if let Some(command) = value.strip_suffix('=') {
            let command = command.trim();
            if command.is_empty() || command.contains('=') {
                return Err(format!("multicall command {index} is invalid"));
            }
            command_section = true;
            commands.push(command.to_owned());
        } else if command_section {
            return Err(format!("multicall command {index} must end with '='"));
        }
    }
    if commands.is_empty() {
        Ok(vec!["".to_owned()])
    } else {
        Ok(commands)
    }
}

fn project_registry_file_field(entry: &TorrentEntry, command: &str) -> RtValue {
    match command {
        "" | "f.path" | "f.frozen_path" => RtValue::String(entry.name.clone()),
        "f.size_bytes" => RtValue::Int(rt_i64(entry.total_length)),
        "f.completed_bytes" => {
            RtValue::Int(rt_i64(entry.total_length.saturating_sub(entry.amount_left)))
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
        "f.size_bytes" => RtValue::Int(rt_i64(file.length)),
        "f.priority" => RtValue::Int(file.priority),
        "f.is_created" | "f.is_open" => RtValue::Bool(true),
        "f.is_complete" => RtValue::Bool(file_is_complete(file, meta)),
        "f.completed_bytes" => {
            if file_is_complete(file, meta) {
                RtValue::Int(rt_i64(file.length))
            } else {
                RtValue::Int(0)
            }
        }
        "f.offset" => RtValue::Int(rt_i64(file_start_offset(file, meta))),
        "f.range_first" => RtValue::Int(rt_usize_i64(file_first_piece(file, meta))),
        "f.range_second" => RtValue::Int(rt_usize_i64(file_last_piece(file, meta))),
        _ => RtValue::Nil,
    }
}

fn file_start_offset(file: &EngineTorrentFile, meta: &EngineTorrentMetadata) -> u64 {
    meta.files
        .iter()
        .filter(|candidate| candidate.index < file.index)
        .map(|candidate| candidate.length)
        .fold(0, u64::saturating_add)
}

fn file_first_piece(file: &EngineTorrentFile, meta: &EngineTorrentMetadata) -> usize {
    if meta.piece_length == 0 {
        return 0;
    }
    usize::try_from(file_start_offset(file, meta) / meta.piece_length).unwrap_or(usize::MAX)
}

fn file_last_piece(file: &EngineTorrentFile, meta: &EngineTorrentMetadata) -> usize {
    if meta.piece_length == 0 || file.length == 0 {
        return file_first_piece(file, meta);
    }
    usize::try_from(
        file_start_offset(file, meta)
            .saturating_add(file.length)
            .saturating_sub(1)
            / meta.piece_length,
    )
    .unwrap_or(usize::MAX)
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
        "t.type" | "t.id" => RtValue::Int(rt_usize_i64(idx)),
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
        "p.port" => RtValue::Int(i64::from(peer.addr.port())),
        "p.client_version" => RtValue::String(peer.client.clone()),
        "p.completed_percent" => RtValue::Int(rt_percent(peer.progress)),
        "p.down_rate" | "p.down_rate_total" => RtValue::Int(peer.download_rate),
        "p.up_rate" | "p.up_rate_total" => RtValue::Int(peer.upload_rate),
        "p.completed_chunks" => RtValue::Int(rt_usize_i64(peer.pieces)),
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
    let start = method.ends_with("start");
    if payload.starts_with("magnet:") {
        let magnet = parse_magnet(payload).map_err(|err| err.to_string())?;
        if let Some(engine) = &state.engine {
            engine
                .add_magnet_with_labels(magnet, None, !start, None, Vec::new())
                .await?;
            return Ok(RtValue::Int(0));
        }
        let hash = magnet
            .info_hash_v1
            .map(hex_lower)
            .or_else(|| magnet.info_hash_v2.map(hex_lower))
            .ok_or_else(|| "magnet missing supported info hash".to_owned())?;
        let mut entry = TorrentEntry::new(
            hash,
            magnet.display_name.unwrap_or_else(|| "magnet".to_owned()),
            String::new(),
        );
        if start {
            entry
                .transition(rt_session::TorrentState::Downloading)
                .map_err(|err| err.to_string())?;
        }
        state
            .registry
            .write()
            .await
            .add(entry)
            .map_err(|err| err.to_string())?;
        return Ok(RtValue::Int(0));
    }

    let bytes = load_torrent_bytes(method, payload)?;
    if let Some(engine) = &state.engine {
        engine
            .add_torrent_raw_with_labels(bytes, None, !start, None, Vec::new())
            .await?;
        return Ok(RtValue::Int(0));
    }

    let mut entry = {
        let parsed = parse_torrent(&bytes).map_err(|err| err.to_string())?;
        let hash = parsed
            .v1_info_hash()
            .map(hex_lower)
            .or_else(|| parsed.v2_info_hash().map(hex_lower))
            .ok_or_else(|| "torrent missing supported info hash".to_owned())?;
        TorrentEntry::new(hash, parsed.name().to_owned(), String::new())
    };
    if start {
        entry
            .transition(rt_session::TorrentState::Downloading)
            .map_err(|err| err.to_string())?;
    }
    state
        .registry
        .write()
        .await
        .add(entry)
        .map_err(|err| err.to_string())?;
    Ok(RtValue::Int(0))
}

fn load_torrent_bytes(method: &str, payload: &str) -> Result<Vec<u8>, String> {
    if !method.contains(".raw") {
        return Err(
            "path-based rTorrent loads are unsupported at this library boundary; use load.raw or load.raw_start with embedded metainfo".to_owned(),
        );
    }
    general_purpose::STANDARD
        .decode(payload)
        .map_err(|err| format!("invalid base64 torrent payload: {err}"))
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
                engine.remove_torrent(hash.to_owned(), false).await?;
            }
            Lifecycle::Pause => {
                engine.pause_torrent(hash.to_owned()).await?;
            }
            Lifecycle::Resume => {
                engine.resume_torrent(hash.to_owned()).await?;
            }
        }
        if matches!(lifecycle, Lifecycle::Erase) {
            state.custom.write().await.remove(&canonical_hash_key(hash));
            state
                .torrent_limits
                .write()
                .await
                .remove(&canonical_hash_key(hash));
        }
        return Ok(RtValue::Int(0));
    }

    let mut registry = state.registry.write().await;
    match lifecycle {
        Lifecycle::Erase => {
            registry.remove(hash).map_err(|err| err.to_string())?;
            state.custom.write().await.remove(&canonical_hash_key(hash));
            state
                .torrent_limits
                .write()
                .await
                .remove(&canonical_hash_key(hash));
        }
        Lifecycle::Pause => {
            let mut entry = registry
                .get_mut(hash)
                .ok_or_else(|| format!("torrent {hash} not found"))?;
            entry
                .transition(rt_session::TorrentState::Paused)
                .map_err(|err| err.to_string())?;
        }
        Lifecycle::Resume => {
            let mut entry = registry
                .get_mut(hash)
                .ok_or_else(|| format!("torrent {hash} not found"))?;
            entry
                .transition(rt_session::TorrentState::Downloading)
                .map_err(|err| err.to_string())?;
        }
    }
    Ok(RtValue::Int(0))
}

/// TNG-022: `d.tracker_announce = <hash>` is rTorrent's "force reannounce"
/// call. This used to be a pure literal `0` return that never read
/// `params` or touched the engine at all -- a client asking for a
/// reannounce got a convincing "success" with nothing actually happening.
/// Mirrors `lifecycle`'s hash-extraction pattern and the already-working
/// qBittorrent-compat equivalent (`torrents_reannounce`, which calls this
/// same `Engine::reannounce_torrent`).
async fn tracker_announce(state: &AppState, params: &[RtValue]) -> Result<RtValue, String> {
    let hash = params
        .first()
        .and_then(RtValue::as_str)
        .ok_or_else(|| "d.tracker_announce requires info hash".to_owned())?;
    let Some(engine) = &state.engine else {
        return Err("rTorrent tracker announce requires a live engine".to_owned());
    };
    engine.reannounce_torrent(hash.to_owned()).await?;
    Ok(RtValue::Int(0))
}

fn unix_now() -> i64 {
    rt_i64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
}

fn rt_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn rt_usize_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn rt_ratio_milli(uploaded: u64, downloaded: u64) -> i64 {
    if downloaded == 0 {
        return 0;
    }
    let value = (uploaded as f64 / downloaded as f64) * 1000.0;
    if !value.is_finite() || value >= i64::MAX as f64 {
        i64::MAX
    } else if value <= i64::MIN as f64 {
        i64::MIN
    } else {
        value.round() as i64
    }
}

fn rt_percent(progress: f64) -> i64 {
    if !progress.is_finite() {
        return 0;
    }
    (progress * 100.0).round().clamp(0.0, 100.0) as i64
}

fn canonical_hash_key(hash: &str) -> String {
    if matches!(hash.len(), 40 | 64) && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        hash.to_ascii_lowercase()
    } else {
        hash.to_owned()
    }
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
            return Err("XMLRPC request contains an unterminated param".to_owned());
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
        return value
            .trim()
            .parse()
            .map(RtValue::Int)
            .unwrap_or_else(|_| RtValue::String(xml_unescape(value.trim())));
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
            execute(&state, "d.name", std::slice::from_ref(&hash))
                .await
                .unwrap(),
            RtValue::String("alpha".to_owned())
        );
        assert_eq!(
            execute(&state, "d.completed_bytes", std::slice::from_ref(&hash))
                .await
                .unwrap(),
            RtValue::Int(75)
        );
        assert_eq!(
            execute(&state, "d.ratio", &[hash]).await.unwrap(),
            RtValue::Int(2000)
        );
    }

    #[test]
    fn xml_projection_saturates_unrepresentable_unsigned_values() {
        let mut entry = TorrentEntry::new("a".repeat(40), "large".into(), "/data".into());
        entry.total_length = u64::MAX;
        entry.amount_left = u64::MAX;
        entry.added_at = u64::MAX;
        entry.stats.uploaded = u64::MAX;
        entry.stats.downloaded = 1;

        assert_eq!(
            project_download_field(&entry, "d.size_bytes", None, None, None),
            RtValue::Int(i64::MAX)
        );
        assert_eq!(
            project_download_field(&entry, "d.state_changed", None, None, None),
            RtValue::Int(i64::MAX)
        );
        assert_eq!(
            project_download_field(&entry, "d.ratio", None, None, None),
            RtValue::Int(i64::MAX)
        );

        let meta = EngineTorrentMetadata {
            piece_length: 1,
            piece_count: 1,
            piece_hashes: Vec::new(),
            piece_states: Vec::new(),
            is_private: false,
            trackers: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            files: Vec::new(),
        };
        let file = EngineTorrentFile {
            index: 0,
            path: "large.bin".to_owned(),
            length: u64::MAX,
            priority: 1,
            wanted: true,
        };
        assert_eq!(
            project_file_field(&file, "f.size_bytes", &meta),
            RtValue::Int(i64::MAX)
        );

        let peer = EnginePeerSnapshot {
            addr: "127.0.0.1:6881".parse().unwrap(),
            client: "test".to_owned(),
            choked: false,
            upload_choked: false,
            interested: false,
            pieces: usize::MAX,
            pieces_total: usize::MAX,
            progress: f64::NAN,
            download_rate: 0,
            upload_rate: 0,
            downloaded: u64::MAX,
            uploaded: u64::MAX,
        };
        assert_eq!(
            project_peer_field(&peer, "p.completed_chunks"),
            RtValue::Int(i64::MAX)
        );
        assert_eq!(
            project_peer_field(&peer, "p.completed_percent"),
            RtValue::Int(0)
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
    async fn torrent_local_state_is_case_insensitive_and_erased_with_torrent() {
        let state = state_with_torrent().await;
        let upper = RtValue::String("A".repeat(40));
        let lower = RtValue::String("a".repeat(40));

        execute(
            &state,
            "d.custom.set",
            &[
                upper.clone(),
                RtValue::String("label".to_owned()),
                RtValue::String("movies".to_owned()),
            ],
        )
        .await
        .unwrap();
        execute(
            &state,
            "d.down.max_rate.set",
            &[upper.clone(), RtValue::Int(333)],
        )
        .await
        .unwrap();

        assert_eq!(
            execute(
                &state,
                "d.custom",
                &[lower.clone(), RtValue::String("label".to_owned())],
            )
            .await
            .unwrap(),
            RtValue::String("movies".to_owned())
        );
        assert_eq!(
            execute(&state, "d.down.max_rate", std::slice::from_ref(&lower))
                .await
                .unwrap(),
            RtValue::Int(333)
        );

        assert!(execute(
            &state,
            "d.custom.set",
            &[
                RtValue::String("b".repeat(40)),
                RtValue::String("label".to_owned()),
                RtValue::String("orphan".to_owned()),
            ],
        )
        .await
        .is_err());

        execute(&state, "d.erase", std::slice::from_ref(&upper))
            .await
            .unwrap();
        assert!(!state.custom.read().await.contains_key(&"a".repeat(40)));
        assert!(!state
            .torrent_limits
            .read()
            .await
            .contains_key(&"a".repeat(40)));
    }

    #[tokio::test]
    async fn rtorrent_mutators_reject_missing_or_malformed_values() {
        let state = state_with_torrent().await;
        let hash = RtValue::String("a".repeat(40));

        assert!(execute(&state, "d.custom.set", std::slice::from_ref(&hash))
            .await
            .is_err());
        assert!(execute(
            &state,
            "d.custom.set",
            &[
                hash.clone(),
                RtValue::String(String::new()),
                RtValue::Int(1)
            ],
        )
        .await
        .is_err());
        assert!(execute(
            &state,
            "d.down.max_rate.set",
            &[hash.clone(), RtValue::String("not-a-rate".to_owned())],
        )
        .await
        .is_err());
        assert!(execute(&state, "throttle.global_down.max_rate.set", &[])
            .await
            .is_err());

        let parsed = parse_value("<value><int>not-a-number</int></value>");
        assert_eq!(parsed, RtValue::String("not-a-number".to_owned()));
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
            execute(&state, "d.down.max_rate", std::slice::from_ref(&hash))
                .await
                .unwrap(),
            RtValue::Int(333)
        );
        assert_eq!(
            execute(&state, "d.up.max_rate", std::slice::from_ref(&hash))
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
    async fn multicall_honors_view_and_rejects_malformed_commands() {
        let state = state_with_torrent().await;
        let mut complete = TorrentEntry::new("b".repeat(40), "beta".into(), "/data/beta".into());
        complete.total_length = 10;
        complete.amount_left = 0;
        complete.transition(TorrentState::Downloading).unwrap();
        complete.transition(TorrentState::Seeding).unwrap();
        state.registry.write().await.add(complete).unwrap();

        let value = execute(
            &state,
            "d.multicall2",
            &[
                RtValue::String("complete".to_owned()),
                RtValue::String("d.hash=".to_owned()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            value,
            RtValue::Array(vec![RtValue::Array(vec![RtValue::String("b".repeat(40))])])
        );

        let malformed = execute(
            &state,
            "d.multicall2",
            &[
                RtValue::String("main".to_owned()),
                RtValue::String("d.hash".to_owned()),
            ],
        )
        .await;
        assert!(malformed.is_err());
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
    async fn configured_embedded_xmlrpc_state_rejects_missing_or_wrong_token() {
        let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())))
            .with_tokens(vec!["embedded-secret".to_owned()]);
        let request = r#"<methodCall><methodName>method.list</methodName><params/></methodCall>"#;

        let unauthorized = execute_xml(&state, request).await;
        assert!(unauthorized.contains("unauthorized"));

        let wrong = execute_xml_with_token(&state, request, Some("wrong")).await;
        assert!(wrong.contains("unauthorized"));

        let authorized = execute_xml_with_token(&state, request, Some("embedded-secret")).await;
        assert!(authorized.contains("<methodResponse>"));
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

    #[tokio::test]
    async fn lifecycle_fallback_mutates_registry_and_rejects_missing_torrents() {
        let state = state_with_torrent().await;
        let hash = RtValue::String("a".repeat(40));

        execute(&state, "d.pause", std::slice::from_ref(&hash))
            .await
            .unwrap();
        assert_eq!(
            state
                .registry
                .read()
                .await
                .get(&"a".repeat(40))
                .unwrap()
                .state,
            TorrentState::Paused
        );

        execute(&state, "d.resume", std::slice::from_ref(&hash))
            .await
            .unwrap();
        assert_eq!(
            state
                .registry
                .read()
                .await
                .get(&"a".repeat(40))
                .unwrap()
                .state,
            TorrentState::Downloading
        );

        let missing = execute(&state, "d.erase", &[RtValue::String("b".repeat(40))]).await;
        assert!(missing.is_err());
    }

    #[tokio::test]
    async fn path_load_rejects_unsupported_filesystem_boundary() {
        let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
        let result = execute(
            &state,
            "load.normal",
            &[RtValue::String("/tmp/does-not-exist.torrent".to_owned())],
        )
        .await;
        assert!(result
            .expect_err("path loads must not report a false success")
            .contains("path-based rTorrent loads are unsupported"));
    }

    #[tokio::test]
    async fn tracker_announce_requires_info_hash() {
        // TNG-022: d.tracker_announce used to be a literal `Ok(Int(0))`
        // that never even read `params` -- an empty/missing hash was
        // silently accepted as "success". It now has to parse a real
        // hash out of params before it can call the engine, so a missing
        // one must be a real error, not a stub success.
        let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
        let result = execute(&state, "d.tracker_announce", &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn tracker_announce_fails_closed_without_engine() {
        // A force-reannounce has no meaningful projection-only fallback: it
        // must reach the engine or tell the caller that it did not happen.
        let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
        let result = execute(
            &state,
            "d.tracker_announce",
            &[RtValue::String("b".repeat(40))],
        )
        .await;
        assert!(result
            .expect_err("announce must not report success without an engine")
            .contains("requires a live engine"));
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
