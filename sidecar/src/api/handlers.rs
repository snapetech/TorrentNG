use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::ffi::CString;
use std::{
    collections::BTreeMap,
    path::{Path as FsPath, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};
use tokio::{process::Command, time::sleep};

use super::server::AppState;
use super::ws::Event;
use crate::cache::{
    AppEventRow, Category, ListParams, RatioGroup, RssRule, SavedView, WorkflowRule, WorkflowRun,
};
use crate::rtorrent::{engine::ProbeValue, XmlValue};

// --- Health ---

#[derive(Serialize)]
pub struct HealthResponse {
    status: &'static str,
    rtorrent: &'static str,
    cached_torrents: i64,
}

pub async fn health(State(s): State<AppState>) -> impl IntoResponse {
    let cached = s.db.count().unwrap_or(0);
    let rtorrent_connected = s.rt.call("system.client_version", &[]).await.is_ok();
    let status = if rtorrent_connected {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(HealthResponse {
            status: if rtorrent_connected { "ok" } else { "degraded" },
            rtorrent: if rtorrent_connected {
                "connected"
            } else {
                "unreachable"
            },
            cached_torrents: cached,
        }),
    )
}

// --- Metrics ---

pub async fn metrics_handler(State(s): State<AppState>) -> impl IntoResponse {
    // Update gauges from cache before rendering
    if let Ok(count) = s.db.count() {
        s.metrics.torrents_total.store(count, Ordering::Relaxed);
    }
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        s.metrics.render(),
    )
}

// --- Storage ---

#[derive(Serialize)]
pub struct StorageRoot {
    path: String,
    total_bytes: u64,
    available_bytes: u64,
    used_bytes: u64,
    used_percent: f64,
    readonly: bool,
    ok: bool,
    error: Option<String>,
}

pub async fn storage_roots(State(s): State<AppState>) -> impl IntoResponse {
    let roots = if s.cfg.storage_roots.is_empty() {
        vec![FsPath::new("/").to_path_buf()]
    } else {
        s.cfg.storage_roots.clone()
    };
    let rows: Vec<StorageRoot> = roots.iter().map(|path| storage_root(path)).collect();
    Json(serde_json::json!({ "roots": rows }))
}

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    limit: Option<usize>,
    kind: Option<String>,
    level: Option<String>,
}

pub async fn list_logs(
    State(s): State<AppState>,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let levels = query
        .level
        .as_deref()
        .map(|level| vec![level])
        .unwrap_or_default();
    match s
        .db
        .list_app_events_filtered(limit, query.kind.as_deref(), &levels, None)
    {
        Ok(events) => {
            Json(serde_json::json!({ "logs": events })).into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "api",
                operation = "list_logs",
                error = %e,
                "list logs failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn tracker_health(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.tracker_health() {
        Ok(trackers) => Json(serde_json::json!({ "trackers": trackers })).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "tracker_health", error = %e, "tracker health query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn sidebar_facets(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.sidebar_facets() {
        Ok(facets) => Json(facets).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "sidebar_facets", error = %e, "sidebar facet query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn engine_diagnostics(State(s): State<AppState>) -> impl IntoResponse {
    Json(s.rt.engine_diagnostics().await)
}

pub async fn engine_commands(State(s): State<AppState>) -> impl IntoResponse {
    Json(s.rt.command_index().await)
}

// --- rTorrent managed settings ---

const RTORRENT_MANAGED_BEGIN: &str = "# TorrentNG managed settings begin";
const RTORRENT_MANAGED_END: &str = "# TorrentNG managed settings end";

#[derive(Debug, Clone, Serialize)]
pub struct RtorrentSettingDescriptor {
    key: &'static str,
    label: &'static str,
    command: &'static str,
    setter: &'static str,
    value_type: &'static str,
    unit: Option<&'static str>,
    restart_required: bool,
    minimum: Option<i64>,
    maximum: Option<i64>,
    default_value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct RtorrentSettingState {
    key: &'static str,
    live: ProbeValue<String>,
    saved: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RtorrentSettingsResponse {
    settings: Vec<RtorrentSettingDescriptor>,
    values: Vec<RtorrentSettingState>,
    overlay_path: String,
    overlay_writable: bool,
    custom_rc: String,
    restart_supported: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RtorrentSettingsPatch {
    values: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    custom_rc: String,
    #[serde(default = "default_true")]
    apply_live: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RtorrentSettingsApplyResponse {
    saved: bool,
    restart_required: bool,
    applied: Vec<String>,
    errors: Vec<String>,
    overlay_path: String,
}

fn default_true() -> bool {
    true
}

fn rtorrent_settings() -> Vec<RtorrentSettingDescriptor> {
    vec![
        int_setting(
            "max_uploads_global",
            "Global upload slots",
            "throttle.max_uploads.global",
            "throttle.max_uploads.global.set",
            None,
            false,
            0,
            100_000,
            350,
        ),
        int_setting(
            "max_downloads_global",
            "Global download slots",
            "throttle.max_downloads.global",
            "throttle.max_downloads.global.set",
            None,
            false,
            0,
            100_000,
            300,
        ),
        int_setting(
            "max_uploads",
            "Per-torrent upload slots",
            "throttle.max_uploads",
            "throttle.max_uploads.set",
            None,
            false,
            0,
            10_000,
            10,
        ),
        int_setting(
            "max_downloads",
            "Per-torrent download slots",
            "throttle.max_downloads",
            "throttle.max_downloads.set",
            None,
            false,
            0,
            10_000,
            12,
        ),
        int_setting(
            "pieces_memory_max",
            "Piece memory cache",
            "pieces.memory.max",
            "pieces.memory.max.set",
            Some("M"),
            false,
            64,
            262_144,
            4096,
        ),
        int_setting(
            "max_open_files",
            "Open files",
            "network.max_open_files",
            "network.max_open_files.set",
            None,
            true,
            64,
            1_000_000,
            4096,
        ),
        int_setting(
            "max_open_sockets",
            "Open sockets",
            "network.max_open_sockets",
            "network.max_open_sockets.set",
            None,
            true,
            64,
            1_000_000,
            2048,
        ),
        int_setting(
            "http_max_open",
            "Tracker HTTP open requests",
            "network.http.max_open",
            "network.http.max_open.set",
            None,
            false,
            1,
            100_000,
            512,
        ),
        int_setting(
            "http_max_total_connections",
            "Tracker HTTP total connections",
            "network.http.max_total_connections",
            "network.http.max_total_connections.set",
            None,
            false,
            1,
            100_000,
            256,
        ),
        int_setting(
            "http_max_host_connections",
            "Tracker HTTP per-host connections",
            "network.http.max_host_connections",
            "network.http.max_host_connections.set",
            None,
            false,
            1,
            100_000,
            64,
        ),
        int_setting(
            "http_max_cache_connections",
            "Tracker HTTP cache connections",
            "network.http.max_cache_connections",
            "network.http.max_cache_connections.set",
            None,
            false,
            1,
            100_000,
            512,
        ),
        int_setting(
            "http_dns_cache_timeout",
            "Tracker DNS cache",
            "network.http.dns_cache_timeout",
            "network.http.dns_cache_timeout.set",
            Some("s"),
            false,
            0,
            86_400,
            25,
        ),
        int_setting(
            "trackers_numwant",
            "Tracker numwant",
            "trackers.numwant",
            "trackers.numwant.set",
            None,
            false,
            0,
            10_000,
            200,
        ),
        bool_setting(
            "hash_on_completion",
            "Hash on completion",
            "pieces.hash.on_completion",
            "pieces.hash.on_completion.set",
            false,
            true,
        ),
        bool_setting(
            "session_on_completion",
            "Persist completion state",
            "session.on_completion",
            "session.on_completion.set",
            false,
            true,
        ),
        bool_setting(
            "session_use_lock",
            "Session lock",
            "session.use_lock",
            "session.use_lock.set",
            true,
            true,
        ),
        bool_setting(
            "pex",
            "Peer exchange",
            "protocol.pex",
            "protocol.pex.set",
            false,
            true,
        ),
        bool_setting(
            "udp_trackers",
            "UDP trackers",
            "trackers.use_udp",
            "trackers.use_udp.set",
            false,
            true,
        ),
        enum_setting("dht_mode", "DHT mode", "dht", "dht.mode.set", false, "auto"),
    ]
}

fn int_setting(
    key: &'static str,
    label: &'static str,
    command: &'static str,
    setter: &'static str,
    unit: Option<&'static str>,
    restart_required: bool,
    minimum: i64,
    maximum: i64,
    default_value: i64,
) -> RtorrentSettingDescriptor {
    RtorrentSettingDescriptor {
        key,
        label,
        command,
        setter,
        value_type: "int",
        unit,
        restart_required,
        minimum: Some(minimum),
        maximum: Some(maximum),
        default_value: serde_json::json!(default_value),
    }
}

fn bool_setting(
    key: &'static str,
    label: &'static str,
    command: &'static str,
    setter: &'static str,
    restart_required: bool,
    default_value: bool,
) -> RtorrentSettingDescriptor {
    RtorrentSettingDescriptor {
        key,
        label,
        command,
        setter,
        value_type: "bool",
        unit: None,
        restart_required,
        minimum: None,
        maximum: None,
        default_value: serde_json::json!(default_value),
    }
}

fn enum_setting(
    key: &'static str,
    label: &'static str,
    command: &'static str,
    setter: &'static str,
    restart_required: bool,
    default_value: &'static str,
) -> RtorrentSettingDescriptor {
    RtorrentSettingDescriptor {
        key,
        label,
        command,
        setter,
        value_type: "enum",
        unit: None,
        restart_required,
        minimum: None,
        maximum: None,
        default_value: serde_json::json!(default_value),
    }
}

pub async fn get_rtorrent_settings(State(s): State<AppState>) -> impl IntoResponse {
    let descriptors = rtorrent_settings();
    let overlay_path = rtorrent_overlay_path();
    let (saved, custom_rc) = read_rtorrent_overlay(&overlay_path);
    let mut values = Vec::with_capacity(descriptors.len());
    for desc in &descriptors {
        values.push(RtorrentSettingState {
            key: desc.key,
            live: probe_setting(&s.rt, desc.command).await,
            saved: saved.get(desc.key).cloned(),
        });
    }
    Json(RtorrentSettingsResponse {
        settings: descriptors,
        values,
        overlay_path: overlay_path.display().to_string(),
        overlay_writable: overlay_path.parent().is_some_and(|path| path.exists()),
        custom_rc,
        restart_supported: true,
    })
    .into_response()
}

pub async fn set_rtorrent_settings(
    State(s): State<AppState>,
    Json(patch): Json<RtorrentSettingsPatch>,
) -> impl IntoResponse {
    let descriptors = rtorrent_settings();
    let mut by_key = BTreeMap::new();
    for desc in &descriptors {
        by_key.insert(desc.key, desc);
    }

    let mut normalized = BTreeMap::new();
    let mut errors = Vec::new();
    for (key, raw) in patch.values {
        let Some(desc) = by_key.get(key.as_str()) else {
            errors.push(format!("{key}: unknown setting"));
            continue;
        };
        match normalize_setting(desc, raw) {
            Ok(value) => {
                normalized.insert(key, value);
            }
            Err(e) => errors.push(format!("{key}: {e}")),
        }
    }

    if !errors.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(RtorrentSettingsApplyResponse {
                saved: false,
                restart_required: false,
                applied: Vec::new(),
                errors,
                overlay_path: rtorrent_overlay_path().display().to_string(),
            }),
        )
            .into_response();
    }

    let overlay_path = rtorrent_overlay_path();
    if let Err(e) =
        write_rtorrent_overlay(&overlay_path, &descriptors, &normalized, &patch.custom_rc)
    {
        tracing::error!(component = "api", operation = "write_rtorrent_overlay", error = %e, "rTorrent overlay write failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let mut applied = Vec::new();
    let mut apply_errors = Vec::new();
    let mut restart_required = false;
    if patch.apply_live {
        for desc in &descriptors {
            let Some(value) = normalized.get(desc.key) else {
                continue;
            };
            if desc.restart_required {
                restart_required = true;
                continue;
            }
            let arg = xml_arg(desc, value);
            match s.rt.call(desc.setter, &[arg]).await {
                Ok(_) => applied.push(desc.key.to_owned()),
                Err(e) => {
                    restart_required = true;
                    apply_errors.push(format!("{}: {e}", desc.key));
                }
            }
        }
    } else {
        restart_required = true;
    }

    if !patch.custom_rc.trim().is_empty() {
        restart_required = true;
    }

    Json(RtorrentSettingsApplyResponse {
        saved: true,
        restart_required,
        applied,
        errors: apply_errors,
        overlay_path: overlay_path.display().to_string(),
    })
    .into_response()
}

pub async fn restart_process() -> impl IntoResponse {
    tokio::spawn(async {
        sleep(Duration::from_millis(250)).await;
        tracing::warn!("restarting TorrentNG process by admin request");
        std::process::exit(0);
    });
    Json(serde_json::json!({ "restarting": true })).into_response()
}

async fn probe_setting(rt: &crate::rtorrent::Client, command: &str) -> ProbeValue<String> {
    match rt.call(command, &[]).await {
        Ok(value) => ProbeValue::ok(xml_setting_display(&value)),
        Err(e) => ProbeValue::err(e.to_string()),
    }
}

fn normalize_setting(
    desc: &RtorrentSettingDescriptor,
    raw: serde_json::Value,
) -> Result<String, String> {
    match desc.value_type {
        "bool" => raw
            .as_bool()
            .map(|value| if value { "yes" } else { "no" }.to_owned())
            .ok_or_else(|| "expected boolean".to_owned()),
        "enum" => {
            let value = raw
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "expected string".to_owned())?;
            match desc.key {
                "dht_mode" if matches!(value, "off" | "disable" | "auto" | "on") => {
                    Ok(if value == "off" { "disable" } else { value }.to_owned())
                }
                "dht_mode" => Err("expected disable, auto, or on".to_owned()),
                _ => Ok(value.to_owned()),
            }
        }
        _ => {
            let value = raw
                .as_i64()
                .or_else(|| raw.as_str()?.trim().parse().ok())
                .ok_or_else(|| "expected integer".to_owned())?;
            if let Some(min) = desc.minimum {
                if value < min {
                    return Err(format!("must be >= {min}"));
                }
            }
            if let Some(max) = desc.maximum {
                if value > max {
                    return Err(format!("must be <= {max}"));
                }
            }
            Ok(match desc.unit {
                Some("M") => format!("{value}M"),
                _ => value.to_string(),
            })
        }
    }
}

fn xml_arg(desc: &RtorrentSettingDescriptor, value: &str) -> XmlValue {
    match desc.value_type {
        "bool" => XmlValue::from(matches!(value, "yes" | "true" | "1" | "on")),
        "int" if desc.unit.is_none() => value.parse::<i64>().unwrap_or_default().into(),
        _ => value.into(),
    }
}

fn rtorrent_overlay_path() -> PathBuf {
    std::env::var("TNG_RTORRENT_OVERLAY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/config/rtorrent.rc"))
}

fn read_rtorrent_overlay(path: &FsPath) -> (BTreeMap<String, String>, String) {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let mut saved = BTreeMap::new();
    let mut custom = Vec::new();
    let mut in_managed = false;
    let mut in_custom = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == RTORRENT_MANAGED_BEGIN {
            in_managed = true;
            in_custom = false;
            continue;
        }
        if trimmed == RTORRENT_MANAGED_END {
            in_managed = false;
            in_custom = true;
            continue;
        }
        if in_managed {
            if let Some((command, value)) = trimmed.split_once('=') {
                let command = command.trim().trim_end_matches(".set");
                let value = value.trim();
                if let Some(desc) = rtorrent_settings().into_iter().find(|desc| {
                    desc.command == command || desc.setter.trim_end_matches(".set") == command
                }) {
                    saved.insert(desc.key.to_owned(), value.to_owned());
                }
            }
        } else if in_custom {
            custom.push(line.to_owned());
        }
    }
    (saved, custom.join("\n").trim().to_owned())
}

fn write_rtorrent_overlay(
    path: &FsPath,
    descriptors: &[RtorrentSettingDescriptor],
    values: &BTreeMap<String, String>,
    custom_rc: &str,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = String::new();
    out.push_str("# Generated by TorrentNG. Edit from the admin UI when possible.\n");
    out.push_str(RTORRENT_MANAGED_BEGIN);
    out.push('\n');
    for desc in descriptors {
        let value = values
            .get(desc.key)
            .cloned()
            .unwrap_or_else(|| setting_json_default(desc));
        out.push_str(desc.setter);
        out.push_str(" = ");
        out.push_str(&value);
        out.push('\n');
    }
    out.push_str(RTORRENT_MANAGED_END);
    out.push_str("\n\n");
    let custom = custom_rc.trim();
    if !custom.is_empty() {
        out.push_str("# Custom rTorrent lines. These are imported after managed settings.\n");
        out.push_str(custom);
        out.push('\n');
    }
    std::fs::write(path, out)
}

fn setting_json_default(desc: &RtorrentSettingDescriptor) -> String {
    match desc.value_type {
        "bool" => {
            if desc.default_value.as_bool().unwrap_or(false) {
                "yes".to_owned()
            } else {
                "no".to_owned()
            }
        }
        "int" => {
            let value = desc.default_value.as_i64().unwrap_or_default();
            match desc.unit {
                Some("M") => format!("{value}M"),
                _ => value.to_string(),
            }
        }
        _ => desc.default_value.as_str().unwrap_or_default().to_owned(),
    }
}

fn xml_setting_display(value: &XmlValue) -> String {
    match value {
        XmlValue::String(value) => value.trim().to_owned(),
        XmlValue::Int(value) => value.to_string(),
        XmlValue::Bool(value) => if *value { "yes" } else { "no" }.to_owned(),
        _ => format!("{value:?}"),
    }
}

#[derive(Deserialize)]
pub struct SessionFeaturePatch {
    dht: Option<bool>,
    pex: Option<bool>,
}

#[derive(Serialize)]
pub struct SessionFeatureResponse {
    dht: Option<bool>,
    pex: Option<bool>,
}

pub async fn set_session_features(
    State(s): State<AppState>,
    Json(patch): Json<SessionFeaturePatch>,
) -> impl IntoResponse {
    if patch.dht.is_none() && patch.pex.is_none() {
        return (StatusCode::BAD_REQUEST, "no session feature provided").into_response();
    }

    let mut dht = None;
    let mut pex = None;

    if let Some(enabled) = patch.dht {
        let mode = if enabled { "auto" } else { "disable" };
        if let Err(e) = s.rt.call("dht.mode.set", &[XmlValue::from(mode)]).await {
            tracing::error!(component = "rtorrent", operation = "set_dht_mode", enabled, error = %e, "rTorrent DHT mode update failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        dht = Some(enabled);
    }

    if let Some(enabled) = patch.pex {
        if let Err(e) =
            s.rt.call("protocol.pex.set", &[XmlValue::from(enabled)])
                .await
        {
            tracing::error!(component = "rtorrent", operation = "set_pex", enabled, error = %e, "rTorrent PEX update failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        pex = Some(enabled);
    }

    Json(SessionFeatureResponse { dht, pex }).into_response()
}

// --- Saved views ---

pub async fn list_saved_views(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_saved_views() {
        Ok(views) => Json(views).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_saved_views", error = %e, "saved view listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn upsert_saved_view(
    State(s): State<AppState>,
    Json(mut view): Json<SavedView>,
) -> impl IntoResponse {
    view.name = view.name.trim().to_owned();
    if view.name.is_empty() {
        return (StatusCode::BAD_REQUEST, "saved view name must not be empty").into_response();
    }
    match s.db.upsert_saved_view(view) {
        Ok(views) => {
            emit(&s, Event::SavedViewsUpdated);
            Json(views).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_saved_view", error = %e, "saved view upsert failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn delete_saved_view(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match s.db.delete_saved_view(&id) {
        Ok(views) => {
            emit(&s, Event::SavedViewsUpdated);
            Json(views).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_saved_view", view_id = %id, error = %e, "saved view delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Ratio groups ---

pub async fn list_ratio_groups(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_ratio_groups() {
        Ok(groups) => Json(groups).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_ratio_groups", error = %e, "ratio group listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn upsert_ratio_group(
    State(s): State<AppState>,
    Json(mut group): Json<RatioGroup>,
) -> impl IntoResponse {
    group.name = group.name.trim().to_owned();
    group.category = group
        .category
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    group.tracker = group
        .tracker
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    if group.name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "ratio group name must not be empty",
        )
            .into_response();
    }
    if !group.ratio_limit.is_finite() || group.ratio_limit < 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            "ratio_limit must be a non-negative number",
        )
            .into_response();
    }
    if group.seeding_time_limit < -1 {
        return (
            StatusCode::BAD_REQUEST,
            "seeding_time_limit must be -1 or greater",
        )
            .into_response();
    }
    match s.db.upsert_ratio_group(group) {
        Ok(groups) => {
            emit(&s, Event::RatioGroupsUpdated);
            Json(groups).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_ratio_group", error = %e, "ratio group upsert failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn delete_ratio_group(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match s.db.delete_ratio_group(&name) {
        Ok(groups) => {
            emit(&s, Event::RatioGroupsUpdated);
            Json(groups).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_ratio_group", ratio_group = %name, error = %e, "ratio group delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct ApplyRatioGroupBody {
    #[serde(default)]
    pub dry_run: bool,
}

pub async fn apply_ratio_group(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<ApplyRatioGroupBody>,
) -> impl IntoResponse {
    let group = match s.db.get_ratio_group(&name) {
        Ok(Some(group)) => group,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "get_ratio_group", ratio_group = %name, error = %e, "ratio group lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if !group.enabled {
        return (StatusCode::BAD_REQUEST, "ratio group is disabled").into_response();
    }

    let hashes = match s.db.ratio_group_hashes(&group) {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(component = "api", operation = "ratio_group_hashes", ratio_group = %name, error = %e, "ratio group hash query failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if body.dry_run {
        return Json(BulkResult {
            applied: hashes,
            errors: vec![],
            dry_run: true,
        })
        .into_response();
    }

    let ratio_limit_milli = (group.ratio_limit * 1000.0) as i64;
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for hash in hashes {
        match s
            .rt
            .set_share_limits(&hash, ratio_limit_milli, group.seeding_time_limit)
            .await
        {
            Ok(()) => applied.push(hash),
            Err(e) => errors.push(format!("{hash}: {e}")),
        }
    }

    Json(BulkResult {
        applied,
        errors,
        dry_run: false,
    })
    .into_response()
}

// --- Workflow rules ---

pub async fn list_workflows(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_workflow_rules() {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_workflows", error = %e, "workflow rule listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn list_workflow_runs(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_workflow_runs() {
        Ok(runs) => Json(runs).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_workflow_runs", error = %e, "workflow run listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn list_rss_rules(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_rss_rules() {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_rss_rules", error = %e, "RSS rule listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn upsert_rss_rule(
    State(s): State<AppState>,
    Json(mut rule): Json<RssRule>,
) -> impl IntoResponse {
    rule.name = rule.name.trim().to_owned();
    rule.feed_url = rule.feed_url.trim().to_owned();
    rule.include = rule.include.trim().to_owned();
    rule.exclude = rule
        .exclude
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    rule.category = rule
        .category
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    rule.save_path = rule
        .save_path
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    rule.tags = rule
        .tags
        .into_iter()
        .map(|tag| tag.trim().to_owned())
        .filter(|tag| !tag.is_empty())
        .collect();

    if rule.name.is_empty() {
        return (StatusCode::BAD_REQUEST, "rss rule name must not be empty").into_response();
    }
    if rule.feed_url.is_empty() {
        return (StatusCode::BAD_REQUEST, "feed_url must not be empty").into_response();
    }
    if rule.include.is_empty() {
        return (StatusCode::BAD_REQUEST, "include must not be empty").into_response();
    }

    match s.db.upsert_rss_rule(rule) {
        Ok(rules) => {
            emit(&s, Event::RssRulesUpdated);
            Json(rules).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_rss_rule", error = %e, "RSS rule upsert failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn delete_rss_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match s.db.delete_rss_rule(&id) {
        Ok(rules) => {
            emit(&s, Event::RssRulesUpdated);
            Json(rules).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_rss_rule", rule_id = %id, error = %e, "RSS rule delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct TestRssRuleBody {
    pub title: String,
    pub link: Option<String>,
}

pub async fn test_rss_rules(
    State(s): State<AppState>,
    Json(body): Json<TestRssRuleBody>,
) -> impl IntoResponse {
    let title = body.title.trim();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "title must not be empty").into_response();
    }
    match s.db.match_rss_item(title, body.link.as_deref()) {
        Ok(matches) => Json(serde_json::json!({ "matches": matches })).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "test_rss_rules", error = %e, "RSS rule test failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn apply_rss_rules(
    State(s): State<AppState>,
    Json(body): Json<ApplyRssRuleBody>,
) -> impl IntoResponse {
    let title = body.title.trim();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "title must not be empty").into_response();
    }
    let Some(link) = body
        .link
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return (StatusCode::BAD_REQUEST, "link must not be empty").into_response();
    };

    let matches = match s.db.match_rss_item(title, Some(link)) {
        Ok(matches) => matches,
        Err(e) => {
            tracing::error!(component = "api", operation = "apply_rss_rules", error = %e, "RSS rule apply failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let matched: Vec<_> = matches.into_iter().filter(|m| m.matched).collect();
    if body.dry_run {
        return Json(BulkResult {
            applied: matched.iter().map(|m| m.rule_name.clone()).collect(),
            errors: vec![],
            dry_run: true,
        })
        .into_response();
    }

    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for rule_match in matched {
        let category = rule_match.category.as_deref().unwrap_or("");
        let save_path = rule_match.save_path.as_deref().unwrap_or("");
        match s
            .rt
            .load_magnet(link, save_path, category, rule_match.start)
            .await
        {
            Ok(()) => applied.push(rule_match.rule_name),
            Err(e) => errors.push(format!("{}: {e}", rule_match.rule_name)),
        }
    }

    Json(BulkResult {
        applied,
        errors,
        dry_run: false,
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct CrossSeedBody {
    pub hashes: Vec<String>,
    #[serde(default)]
    pub trackers: Vec<String>,
    #[serde(default)]
    pub reannounce: bool,
    #[serde(default)]
    pub dry_run: bool,
}

pub async fn cross_seed_helper(
    State(s): State<AppState>,
    Json(body): Json<CrossSeedBody>,
) -> impl IntoResponse {
    let hashes = normalized_nonempty(&body.hashes);
    if hashes.is_empty() {
        return (StatusCode::BAD_REQUEST, "hashes must not be empty").into_response();
    }
    let trackers = normalized_nonempty(&body.trackers);
    if trackers.is_empty() && !body.reannounce {
        return (
            StatusCode::BAD_REQUEST,
            "trackers or reannounce must be provided",
        )
            .into_response();
    }
    if body.dry_run {
        return Json(BulkResult {
            applied: hashes.into_iter().map(str::to_owned).collect(),
            errors: vec![],
            dry_run: true,
        })
        .into_response();
    }

    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for hash in hashes {
        let mut hash_errors = Vec::new();
        for tracker in &trackers {
            if let Err(e) = s.rt.add_tracker(hash, tracker).await {
                hash_errors.push(format!("add tracker {tracker}: {e}"));
            }
        }
        if body.reannounce {
            if let Err(e) = s.rt.reannounce(hash).await {
                hash_errors.push(format!("reannounce: {e}"));
            }
        }
        if hash_errors.is_empty() {
            applied.push(hash.to_owned());
            emit_torrent_updated(&s, hash);
        } else {
            errors.push(format!("{hash}: {}", hash_errors.join("; ")));
        }
    }

    Json(BulkResult {
        applied,
        errors,
        dry_run: false,
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct ApplyRssRuleBody {
    pub title: String,
    pub link: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

pub async fn upsert_workflow(
    State(s): State<AppState>,
    Json(mut rule): Json<WorkflowRule>,
) -> impl IntoResponse {
    rule.name = rule.name.trim().to_owned();
    rule.event = rule.event.trim().to_owned();
    rule.action = rule.action.trim().to_owned();
    rule.category = rule
        .category
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    rule.tracker = rule
        .tracker
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    rule.command = rule
        .command
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    rule.url = rule
        .url
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    rule.target_path = rule
        .target_path
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());

    if rule.name.is_empty() {
        return (StatusCode::BAD_REQUEST, "workflow name must not be empty").into_response();
    }
    if !matches!(
        rule.event.as_str(),
        "completed" | "added" | "category_changed"
    ) {
        return (StatusCode::BAD_REQUEST, "unsupported workflow event").into_response();
    }
    if !matches!(
        rule.action.as_str(),
        "webhook" | "script" | "set_category" | "set_location"
    ) {
        return (StatusCode::BAD_REQUEST, "unsupported workflow action").into_response();
    }
    if rule.action == "webhook" && rule.url.is_none() {
        return (StatusCode::BAD_REQUEST, "url is required for webhook rules").into_response();
    }
    if rule.action == "script" && rule.command.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "command is required for script rules",
        )
            .into_response();
    }
    if rule.action == "set_location" && rule.target_path.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "target_path is required for set_location rules",
        )
            .into_response();
    }
    if rule.action == "set_category" && rule.category.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "category is required for set_category rules",
        )
            .into_response();
    }

    match s.db.upsert_workflow_rule(rule) {
        Ok(rules) => {
            emit(&s, Event::WorkflowsUpdated);
            Json(rules).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_workflow_rule", error = %e, "workflow rule upsert failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn delete_workflow(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match s.db.delete_workflow_rule(&id) {
        Ok(rules) => {
            emit(&s, Event::WorkflowsUpdated);
            Json(rules).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_workflow_rule", rule_id = %id, error = %e, "workflow rule delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct RunWorkflowBody {
    #[serde(default)]
    pub dry_run: bool,
}

pub async fn run_workflow(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RunWorkflowBody>,
) -> impl IntoResponse {
    let rule = match s.db.get_workflow_rule(&id) {
        Ok(Some(rule)) => rule,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "get_workflow_rule", rule_id = %id, error = %e, "workflow rule lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if !rule.enabled {
        return (StatusCode::BAD_REQUEST, "workflow rule is disabled").into_response();
    }
    let hashes = match s.db.workflow_hashes(&rule) {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(component = "api", operation = "workflow_hashes", rule_id = %id, error = %e, "workflow hash query failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if body.dry_run {
        record_workflow_run(&s, &rule, true, hashes.clone(), hashes.clone(), Vec::new());
        return Json(BulkResult {
            applied: hashes,
            errors: vec![],
            dry_run: true,
        })
        .into_response();
    }

    let matched = hashes.clone();
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for hash in hashes {
        match rule.action.as_str() {
            "set_category" => {
                let Some(category) = rule.category.as_deref() else {
                    errors.push(format!("{hash}: category is not configured"));
                    continue;
                };
                if let Err(e) = s.db.set_torrent_category(&hash, category) {
                    errors.push(format!("{hash}: {e}"));
                    continue;
                }
                match s.rt.set_category(&hash, category).await {
                    Ok(()) => {
                        emit_torrent_updated(&s, &hash);
                        applied.push(hash);
                    }
                    Err(e) => errors.push(format!("{hash}: {e}")),
                }
            }
            "set_location" => {
                let Some(target_path) = rule.target_path.as_deref() else {
                    errors.push(format!("{hash}: target_path is not configured"));
                    continue;
                };
                match s.rt.set_location(&hash, target_path).await {
                    Ok(()) => {
                        if let Err(e) = s.db.set_torrent_location(&hash, target_path) {
                            errors.push(format!("{hash}: {e}"));
                            continue;
                        }
                        emit_torrent_updated(&s, &hash);
                        applied.push(hash);
                    }
                    Err(e) => errors.push(format!("{hash}: {e}")),
                }
            }
            "webhook" => match execute_workflow_webhook(&rule, &hash).await {
                Ok(()) => applied.push(hash),
                Err(e) => errors.push(format!("{hash}: {e}")),
            },
            "script" => match execute_workflow_script(&s, &rule, &hash).await {
                Ok(()) => applied.push(hash),
                Err(e) => errors.push(format!("{hash}: {e}")),
            },
            _ => errors.push(format!("{hash}: unsupported action {}", rule.action)),
        }
    }

    record_workflow_run(&s, &rule, false, matched, applied.clone(), errors.clone());

    Json(BulkResult {
        applied,
        errors,
        dry_run: false,
    })
    .into_response()
}

async fn execute_workflow_script(
    s: &AppState,
    rule: &WorkflowRule,
    hash: &str,
) -> Result<(), String> {
    if !s.cfg.workflows.allow_scripts {
        return Err("script execution is not enabled".to_owned());
    }
    let Some(command) = rule.command.as_deref() else {
        return Err("command is not configured".to_owned());
    };
    let mut parts = command.split_whitespace();
    let Some(program) = parts.next() else {
        return Err("command is empty".to_owned());
    };
    let program_path = std::path::Path::new(program);
    if !s.cfg.workflows.allowed_script_dirs.is_empty() {
        let canonical = program_path
            .canonicalize()
            .map_err(|e| format!("canonicalize script: {e}"))?;
        let allowed = s.cfg.workflows.allowed_script_dirs.iter().any(|dir| {
            dir.canonicalize()
                .map(|allowed_dir| canonical.starts_with(allowed_dir))
                .unwrap_or(false)
        });
        if !allowed {
            return Err("script path is outside allowed_script_dirs".to_owned());
        }
    }

    let mut child = Command::new(program);
    child
        .args(parts)
        .env("TNG_WORKFLOW_ID", &rule.id)
        .env("TNG_WORKFLOW_NAME", &rule.name)
        .env("TNG_TORRENT_HASH", hash);
    if let Some(category) = &rule.category {
        child.env("TNG_CATEGORY", category);
    }
    if let Some(tracker) = &rule.tracker {
        child.env("TNG_TRACKER", tracker);
    }
    let output = tokio::time::timeout(
        Duration::from_secs(s.cfg.workflows.script_timeout_secs.max(1)),
        child.output(),
    )
    .await
    .map_err(|_| "script timed out".to_owned())?
    .map_err(|e| format!("script failed to start: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("script exited with {}", output.status))
    }
}

async fn execute_workflow_webhook(rule: &WorkflowRule, hash: &str) -> Result<(), String> {
    let Some(url) = rule.url.as_deref() else {
        return Err("url is not configured".to_owned());
    };
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?
        .post(url)
        .json(&serde_json::json!({
            "workflow_id": rule.id,
            "workflow_name": rule.name,
            "event": rule.event,
            "action": rule.action,
            "hash": hash,
            "category": rule.category,
            "tracker": rule.tracker,
            "timestamp": chrono::Utc::now().timestamp(),
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("webhook returned {}", response.status()))
    }
}

fn record_workflow_run(
    s: &AppState,
    rule: &WorkflowRule,
    dry_run: bool,
    matched: Vec<String>,
    applied: Vec<String>,
    errors: Vec<String>,
) {
    let run = WorkflowRun {
        id: uuid::Uuid::new_v4().to_string(),
        rule_id: rule.id.clone(),
        rule_name: rule.name.clone(),
        action: rule.action.clone(),
        dry_run,
        matched,
        applied,
        errors,
        started_at: chrono::Utc::now().timestamp(),
    };
    if let Err(e) = s.db.record_workflow_run(run) {
        tracing::error!(component = "api", operation = "record_workflow_run", rule_id = %rule.id, error = %e, "workflow run record failed");
    } else {
        emit(s, Event::WorkflowRunsUpdated);
    }
}

fn emit(s: &AppState, event: Event) {
    record_app_event(s, &event);
    let _ = s.events.send(event);
}

fn record_app_event(s: &AppState, event: &Event) {
    let Some((kind, message, payload)) = app_event_projection(event) else {
        return;
    };
    if let Err(e) = s.db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: chrono::Utc::now().timestamp(),
            level: "info".to_owned(),
            kind,
            message,
            payload,
        },
        s.cfg.logging.event_retention,
    ) {
        tracing::warn!(component = "app_events", operation = "append", error = %e, "failed to append app event");
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

fn emit_torrent_updated(s: &AppState, hash: &str) {
    emit(
        s,
        Event::TorrentUpdated {
            hash: hash.to_owned(),
        },
    );
    emit(s, Event::TrackerHealthUpdated);
}

fn storage_root(path: &FsPath) -> StorageRoot {
    match statvfs(path) {
        Ok(stat) => {
            let total_bytes = stat.total_bytes;
            let available_bytes = stat.available_bytes;
            let used_bytes = total_bytes.saturating_sub(available_bytes);
            let used_percent = if total_bytes > 0 {
                (used_bytes as f64 / total_bytes as f64) * 100.0
            } else {
                0.0
            };
            StorageRoot {
                path: path.display().to_string(),
                total_bytes,
                available_bytes,
                used_bytes,
                used_percent,
                readonly: stat.readonly,
                ok: true,
                error: None,
            }
        }
        Err(e) => StorageRoot {
            path: path.display().to_string(),
            total_bytes: 0,
            available_bytes: 0,
            used_bytes: 0,
            used_percent: 0.0,
            readonly: false,
            ok: false,
            error: Some(e),
        },
    }
}

struct FsStat {
    total_bytes: u64,
    available_bytes: u64,
    readonly: bool,
}

#[cfg(unix)]
fn statvfs(path: &FsPath) -> Result<FsStat, String> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = stat.f_frsize.max(stat.f_bsize);
    Ok(FsStat {
        total_bytes: stat.f_blocks.saturating_mul(block_size),
        available_bytes: stat.f_bavail.saturating_mul(block_size),
        readonly: (stat.f_flag & libc::ST_RDONLY) != 0,
    })
}

#[cfg(not(unix))]
fn statvfs(_path: &FsPath) -> Result<FsStat, String> {
    Err("storage stats are unsupported on this platform".to_owned())
}

// --- Torrent list ---

pub async fn list_torrents(
    State(s): State<AppState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    s.metrics.api_requests_total.fetch_add(1, Ordering::Relaxed);
    match s.db.list(&params) {
        Ok((rows, total)) => {
            Json(serde_json::json!({ "total": total, "torrents": rows })).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "list_torrents", error = %e, "torrent list query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Single torrent ---

pub async fn get_torrent(State(s): State<AppState>, Path(hash): Path<String>) -> impl IntoResponse {
    match s.db.get(&hash) {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "get_torrent", torrent = %hash, error = %e, "torrent lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateTorrentBody {
    pub save_path: Option<String>,
}

pub async fn update_torrent(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<UpdateTorrentBody>,
) -> impl IntoResponse {
    let Some(save_path) = body.save_path.as_deref().map(str::trim) else {
        return (StatusCode::BAD_REQUEST, "save_path is required").into_response();
    };
    if save_path.is_empty() {
        return (StatusCode::BAD_REQUEST, "save_path must not be empty").into_response();
    }

    match s.db.exists(&hash) {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "cache", operation = "exists", torrent = %hash, error = %e, "cache torrent existence check failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if let Err(e) = s.rt.set_location(&hash, save_path).await {
        tracing::error!(component = "rtorrent", operation = "set_location", torrent = %hash, error = %e, "rTorrent location update failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Err(e) = s.db.set_torrent_location(&hash, save_path) {
        tracing::error!(component = "cache", operation = "set_location", torrent = %hash, error = %e, "cache location update failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    emit_torrent_updated(&s, &hash);
    StatusCode::NO_CONTENT.into_response()
}

// --- Add torrent ---

pub async fn add_torrent(State(s): State<AppState>, mut multipart: Multipart) -> impl IntoResponse {
    s.metrics.api_requests_total.fetch_add(1, Ordering::Relaxed);
    let mut save_path = String::new();
    let mut category = String::new();
    let mut start = true;
    let mut magnet: Option<String> = None;
    let mut torrent_data: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name() {
            Some("save_path") => {
                save_path = field.text().await.unwrap_or_default();
            }
            Some("category") => {
                category = field.text().await.unwrap_or_default();
            }
            Some("start") => {
                start = field.text().await.unwrap_or_default() != "false";
            }
            Some("magnet") => {
                magnet = Some(field.text().await.unwrap_or_default());
            }
            Some("torrent") => {
                torrent_data = field.bytes().await.ok().map(|b| b.to_vec());
            }
            _ => {}
        }
    }

    if let Some(m) = magnet {
        let m = m.trim();
        if m.is_empty() {
            return (StatusCode::BAD_REQUEST, "magnet must not be empty").into_response();
        }
        match s.rt.load_magnet(m, &save_path, &category, start).await {
            Ok(_) => return StatusCode::ACCEPTED.into_response(),
            Err(e) => {
                tracing::error!(component = "api", operation = "add_magnet", error = %e, "native magnet add failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }
    if let Some(data) = torrent_data {
        if data.is_empty() {
            return (StatusCode::BAD_REQUEST, "torrent file must not be empty").into_response();
        }
        match s.rt.load_torrent(&data, &save_path, &category, start).await {
            Ok(_) => return StatusCode::ACCEPTED.into_response(),
            Err(e) => {
                tracing::error!(component = "api", operation = "add_torrent", error = %e, "native torrent add failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }
    (StatusCode::BAD_REQUEST, "missing torrent or magnet").into_response()
}

// --- Delete ---

pub async fn delete_torrent(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let delete_files = q.get("delete_files").map(|v| v == "true").unwrap_or(false);
    match s.rt.remove(&hash, delete_files).await {
        Ok(_) => {
            if let Err(e) = s.db.delete(&hash) {
                tracing::warn!(
                    component = "cache",
                    operation = "delete_torrent",
                    torrent = %hash,
                    error = %e,
                    "cache delete failed after native delete"
                );
            }
            emit(&s, Event::TorrentRemoved { hash });
            emit(&s, Event::TrackerHealthUpdated);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "api",
                operation = "delete_torrent",
                torrent = %hash,
                error = %e,
                "native delete failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Per-torrent actions ---

pub async fn torrent_start(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    match s.rt.start(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "start", torrent = %hash, error = %e, "native start failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
pub async fn torrent_stop(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    match s.rt.stop(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "stop", torrent = %hash, error = %e, "native stop failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
pub async fn torrent_recheck(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    match s.rt.recheck(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "recheck", torrent = %hash, error = %e, "native recheck failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
pub async fn torrent_reannounce(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    match s.rt.reannounce(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "reannounce", torrent = %hash, error = %e, "native reannounce failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Trackers ---

pub async fn torrent_trackers(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    match s.rt.list_trackers(&hash).await {
        Ok(trackers) => Json(serde_json::json!({ "trackers": trackers })).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_trackers", torrent = %hash, error = %e, "native tracker listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct TrackerEditItem {
    pub orig_url: String,
    pub new_url: String,
}

#[derive(Deserialize)]
pub struct PatchTrackersBody {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default)]
    pub edit: Vec<TrackerEditItem>,
}

pub async fn patch_torrent_trackers(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<PatchTrackersBody>,
) -> impl IntoResponse {
    let add = normalized_nonempty(&body.add);
    let remove = normalized_nonempty(&body.remove);
    let edit: Vec<(&str, &str)> = body
        .edit
        .iter()
        .map(|item| (item.orig_url.trim(), item.new_url.trim()))
        .filter(|(orig_url, new_url)| !orig_url.is_empty() && !new_url.is_empty())
        .collect();

    if add.is_empty() && remove.is_empty() && edit.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "add, remove, or edit must contain at least one tracker",
        )
            .into_response();
    }

    let mut failures = Vec::new();
    for url in add {
        if let Err(e) = s.rt.add_tracker(&hash, url).await {
            tracing::warn!(
                component = "api",
                operation = "add_tracker",
                torrent = %hash,
                tracker = %redact_log_url(url),
                error = %e,
                "add tracker failed"
            );
            failures.push(format!("add {url}: {e}"));
        }
    }
    for url in remove {
        if let Err(e) = s.rt.remove_tracker(&hash, url).await {
            tracing::warn!(
                component = "api",
                operation = "remove_tracker",
                torrent = %hash,
                tracker = %redact_log_url(url),
                error = %e,
                "remove tracker failed"
            );
            failures.push(format!("remove {url}: {e}"));
        }
    }
    for (orig_url, new_url) in edit {
        if let Err(e) = s.rt.edit_tracker(&hash, orig_url, new_url).await {
            tracing::warn!(
                component = "api",
                operation = "edit_tracker",
                torrent = %hash,
                tracker = %redact_log_url(orig_url),
                error = %e,
                "edit tracker failed"
            );
            failures.push(format!("edit {orig_url}: {e}"));
        }
    }

    if !failures.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to patch trackers: {}", failures.join("; ")),
        )
            .into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

// --- Files ---

pub async fn torrent_files(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    match s.rt.list_files(&hash).await {
        Ok(files) => Json(serde_json::json!({ "files": files })).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_files", torrent = %hash, error = %e, "native file listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct FilePriorityItem {
    pub index: usize,
    pub priority: i64,
}

#[derive(Deserialize)]
pub struct SetFilePrioritiesBody {
    pub files: Vec<FilePriorityItem>,
}

pub async fn set_file_priorities(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<SetFilePrioritiesBody>,
) -> impl IntoResponse {
    if body.files.is_empty() {
        return (StatusCode::BAD_REQUEST, "files must not be empty").into_response();
    }

    let mut failures = Vec::new();
    for item in &body.files {
        if let Err(e) =
            s.rt.set_file_priority(&hash, item.index, item.priority)
                .await
        {
            tracing::warn!(
                component = "api",
                operation = "set_file_priority",
                torrent = %hash,
                file_index = item.index,
                priority = item.priority,
                error = %e,
                "native file priority update failed"
            );
            failures.push(format!("{}: {e}", item.index));
        }
    }
    if failures.len() == body.files.len() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to set file priorities: {}", failures.join("; ")),
        )
            .into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

// --- Categories ---

pub async fn list_categories(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_categories() {
        Ok(cats) => Json(cats).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_categories", error = %e, "category listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct CategoryBody {
    pub name: String,
    pub save_path: Option<String>,
}

pub async fn upsert_category(
    State(s): State<AppState>,
    Json(body): Json<CategoryBody>,
) -> impl IntoResponse {
    let name = body.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "category name must not be empty").into_response();
    }
    let save_path = body.save_path.as_deref().unwrap_or("");
    match s.db.upsert_category(name, save_path) {
        Ok(_) => {
            emit(&s, Event::CategoriesUpdated);
            Json(Category {
                name: name.to_owned(),
                save_path: save_path.to_owned(),
                torrent_count: 0,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_category", category = %name, error = %e, "category upsert failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn delete_category(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match s.db.delete_category(&name) {
        Ok(_) => {
            emit(&s, Event::CategoriesUpdated);
            emit(&s, Event::TrackerHealthUpdated);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_category", category = %name, error = %e, "category delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Tags ---

pub async fn list_tags(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_tags() {
        Ok(tags) => Json(tags).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_tags", error = %e, "tag listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct TagBody {
    pub name: String,
}

pub async fn create_tag(State(s): State<AppState>, Json(body): Json<TagBody>) -> impl IntoResponse {
    let name = body.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "tag name must not be empty").into_response();
    }
    match s.db.ensure_tag(name) {
        Ok(_) => {
            emit(&s, Event::TagsUpdated);
            StatusCode::CREATED.into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "create_tag", tag = %name, error = %e, "tag create failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn delete_tag(State(s): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    match s.db.delete_tag(&name) {
        Ok(_) => {
            emit(&s, Event::TagsUpdated);
            emit(&s, Event::TrackerHealthUpdated);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_tag", tag = %name, error = %e, "tag delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Torrent category/tag assignment ---

#[derive(Deserialize)]
pub struct SetCategoryBody {
    pub category: String,
}

pub async fn set_torrent_category(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<SetCategoryBody>,
) -> impl IntoResponse {
    match s.db.exists(&hash) {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "cache", operation = "exists", torrent = %hash, error = %e, "cache torrent existence check failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // Persist to DB and push to rTorrent (d.custom1 = category name)
    if let Err(e) = s.db.set_torrent_category(&hash, &body.category) {
        tracing::error!(component = "cache", operation = "set_category", torrent = %hash, category = %body.category, error = %e, "cache category update failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Err(e) = s.rt.set_category(&hash, &body.category).await {
        tracing::warn!(component = "rtorrent", operation = "set_category", torrent = %hash, category = %body.category, error = %e, "rTorrent category update failed");
    }
    emit_torrent_updated(&s, &hash);
    emit(&s, Event::CategoriesUpdated);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct ModTagsBody {
    pub tags: Vec<String>,
}

pub async fn add_torrent_tags(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<ModTagsBody>,
) -> impl IntoResponse {
    let tags = normalized_tags(&body.tags);
    if tags.is_empty() {
        return (StatusCode::BAD_REQUEST, "tags must not be empty").into_response();
    }

    match s.db.exists(&hash) {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "cache", operation = "exists", torrent = %hash, error = %e, "cache torrent existence check failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    for tag in &tags {
        if let Err(e) = s.db.add_torrent_tag(&hash, tag) {
            tracing::error!(component = "cache", operation = "add_tag", torrent = %hash, tag = %tag, error = %e, "cache tag add failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    emit_torrent_updated(&s, &hash);
    emit(&s, Event::TagsUpdated);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn remove_torrent_tags(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<ModTagsBody>,
) -> impl IntoResponse {
    let tags = normalized_tags(&body.tags);
    if tags.is_empty() {
        return (StatusCode::BAD_REQUEST, "tags must not be empty").into_response();
    }

    match s.db.exists(&hash) {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "cache", operation = "exists", torrent = %hash, error = %e, "cache torrent existence check failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    for tag in &tags {
        if let Err(e) = s.db.remove_torrent_tag(&hash, tag) {
            tracing::error!(component = "cache", operation = "remove_tag", torrent = %hash, tag = %tag, error = %e, "cache tag removal failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    emit_torrent_updated(&s, &hash);
    emit(&s, Event::TagsUpdated);
    StatusCode::NO_CONTENT.into_response()
}

fn normalized_tags(tags: &[String]) -> Vec<&str> {
    normalized_nonempty(tags)
}

fn normalized_nonempty(values: &[String]) -> Vec<&str> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect()
}

// --- Bulk actions ---

#[derive(Deserialize)]
pub struct BulkBody {
    pub hashes: Vec<String>,
    #[serde(default)]
    pub dry_run: bool,
    pub category: Option<String>,
    pub save_path: Option<String>,
}

#[derive(Serialize)]
pub struct BulkResult {
    pub applied: Vec<String>,
    pub errors: Vec<String>,
    pub dry_run: bool,
}

pub async fn bulk_action(
    State(s): State<AppState>,
    Path(action): Path<String>,
    Json(body): Json<BulkBody>,
) -> impl IntoResponse {
    let valid_action = matches!(
        action.as_str(),
        "start" | "stop" | "recheck" | "reannounce" | "set-category" | "set-location"
    );
    if !valid_action {
        return (StatusCode::BAD_REQUEST, format!("unknown action: {action}")).into_response();
    }

    let category = body.category.as_deref().map(str::trim);
    let save_path = body.save_path.as_deref().map(str::trim);
    if action == "set-category" && category.is_none() {
        return (StatusCode::BAD_REQUEST, "category is required").into_response();
    }
    if action == "set-location" {
        match save_path {
            Some(path) if !path.is_empty() => {}
            _ => return (StatusCode::BAD_REQUEST, "save_path must not be empty").into_response(),
        }
    }

    if body.dry_run {
        return Json(BulkResult {
            applied: body.hashes.clone(),
            errors: vec![],
            dry_run: true,
        })
        .into_response();
    }
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for hash in &body.hashes {
        let res = match action.as_str() {
            "start" => s.rt.start(hash).await,
            "stop" => s.rt.stop(hash).await,
            "recheck" => s.rt.recheck(hash).await,
            "reannounce" => s.rt.reannounce(hash).await,
            "set-category" => {
                let category = category.expect("category was validated");
                match s.db.exists(hash) {
                    Ok(true) => {
                        if let Err(e) = s.db.set_torrent_category(hash, category) {
                            errors.push(format!("{hash}: {e}"));
                            continue;
                        }
                    }
                    Ok(false) => {
                        errors.push(format!("{hash}: not found"));
                        continue;
                    }
                    Err(e) => {
                        errors.push(format!("{hash}: {e}"));
                        continue;
                    }
                }
                s.rt.set_category(hash, category).await
            }
            "set-location" => {
                let save_path = save_path.expect("save_path was validated");
                match s.db.exists(hash) {
                    Ok(true) => {}
                    Ok(false) => {
                        errors.push(format!("{hash}: not found"));
                        continue;
                    }
                    Err(e) => {
                        errors.push(format!("{hash}: {e}"));
                        continue;
                    }
                }
                match s.rt.set_location(hash, save_path).await {
                    Ok(()) => {
                        if let Err(e) = s.db.set_torrent_location(hash, save_path) {
                            errors.push(format!("{hash}: {e}"));
                            continue;
                        }
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            _ => unreachable!("bulk action was validated"),
        };
        match res {
            Ok(_) => {
                emit_torrent_updated(&s, hash);
                if action == "set-category" {
                    emit(&s, Event::CategoriesUpdated);
                }
                applied.push(hash.clone());
            }
            Err(e) => errors.push(format!("{hash}: {e}")),
        }
    }
    Json(BulkResult {
        applied,
        errors,
        dry_run: false,
    })
    .into_response()
}

// --- User-agent settings ---

#[derive(Serialize)]
pub struct UserAgentResponse {
    pub user_agent: String,
}

#[derive(Deserialize)]
pub struct SetUserAgentBody {
    pub user_agent: String,
}

pub async fn get_user_agent(State(s): State<AppState>) -> impl IntoResponse {
    match s.rt.get_user_agent().await {
        Ok(ua) => Json(UserAgentResponse { user_agent: ua }).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "get_user_agent", error = %e, "user-agent lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn set_user_agent(
    State(s): State<AppState>,
    Json(body): Json<SetUserAgentBody>,
) -> impl IntoResponse {
    let ua = body.user_agent.trim().to_owned();
    if ua.is_empty() {
        return (StatusCode::BAD_REQUEST, "user_agent must not be empty").into_response();
    }
    match s.rt.set_user_agent(&ua).await {
        Ok(_) => {
            tracing::info!(user_agent = %ua, "user agent updated");
            Json(UserAgentResponse { user_agent: ua }).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "set_user_agent", error = %e, "user-agent update failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
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
