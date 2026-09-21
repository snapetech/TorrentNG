use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{
    de::{self, IgnoredAny, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
#[cfg(unix)]
use std::ffi::CString;
use std::{
    collections::{BTreeMap, HashSet},
    io::{Read, Write},
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::atomic::Ordering,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::sleep,
};

use super::server::AppState;
use super::ws::Event;
use crate::backend::{post_remote_json, ratio_milli, BackendHealth, BackendStatus};
use crate::cache::{
    bounded_page_limit, is_automation_state_capacity_error, is_category_tag_capacity_error,
    validate_page_offset, AppEventRow, Category, ListParams, RatioGroup, RssRule,
    RssRuleUpsertResult, SavedView, TorrentLiveRow, WorkflowRule, WorkflowRun,
};
use crate::rtorrent::{engine::ProbeValue, XmlValue};
use crate::safe_file::open_regular_read;

const MAX_RSS_RULE_ID_BYTES: usize = 128;
const MAX_RSS_RULE_NAME_BYTES: usize = 256;
const MAX_RSS_RULE_FIELD_BYTES: usize = 8_192;
const MAX_RSS_RULE_TAGS: usize = 1_024;
const MAX_RSS_RULE_TAG_BYTES: usize = 256;
const MAX_API_CATEGORY_NAME_BYTES: usize = 256;
const MAX_API_CATEGORY_PATH_BYTES: usize = 4_096;
const MAX_API_TAGS: usize = 1_024;
const MAX_API_TAG_BYTES: usize = 256;
const MAX_API_TORRENT_ADD_FIELDS: usize = 5;
const MAX_API_MAGNET_BYTES: usize = 8_192;
const MAX_API_TRACKER_MUTATIONS: usize = 1_024;
const MAX_API_TRACKER_URL_BYTES: usize = 8_192;
const MAX_API_FILE_PRIORITY_UPDATES: usize = 4_096;
const MAX_CROSS_SEED_OPERATIONS: usize = 10_000;
const MAX_RATIO_GROUPS: usize = 1_024;
const MAX_RATIO_GROUP_NAME_BYTES: usize = 256;
const MAX_WORKFLOW_RULES: usize = 1_024;
const MAX_WORKFLOW_ID_BYTES: usize = 128;
const MAX_WORKFLOW_NAME_BYTES: usize = 256;
const MAX_WORKFLOW_TOKEN_BYTES: usize = 64;
const MAX_WORKFLOW_TEXT_BYTES: usize = 8_192;
const MAX_WORKFLOW_PATH_BYTES: usize = 4_096;

fn metadata_write_status(error: &anyhow::Error) -> StatusCode {
    if is_category_tag_capacity_error(error) {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

fn metadata_read_status(error: &anyhow::Error) -> StatusCode {
    if is_category_tag_capacity_error(error) {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

fn automation_state_write_status(error: &anyhow::Error) -> StatusCode {
    if is_automation_state_capacity_error(error) {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

// --- Health ---

#[derive(Serialize)]
pub struct HealthResponse {
    status: &'static str,
    backend: BackendHealth,
    rtorrent: &'static str,
    cache: &'static str,
    cached_torrents: i64,
}

pub async fn health(State(s): State<AppState>) -> impl IntoResponse {
    let (cached, cache_ok) = match s.db.run_blocking("health_count", |db| db.count()).await {
        Ok(count) => (count, true),
        Err(e) => {
            tracing::error!(
                component = "cache",
                operation = "count",
                result = "error",
                error = %crate::url_redaction::redact_display(&e),
                "compatible-client service cache health probe failed"
            );
            (0, false)
        }
    };
    let backend_status =
        match tokio::time::timeout(Duration::from_secs(3), s.backend.health()).await {
            Ok(status) => status,
            Err(_) => BackendStatus::Unreachable,
        };
    let connected = backend_status == BackendStatus::Connected && cache_ok;
    let status = if connected {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(HealthResponse {
            status: if connected { "ok" } else { "degraded" },
            backend: BackendHealth {
                backend_type: s.backend.backend_type().as_str(),
                status: backend_status.as_str(),
            },
            rtorrent: if s.backend.backend_type() == crate::backend::BackendType::Rtorrent {
                backend_status.as_str()
            } else {
                "not_selected"
            },
            cache: if cache_ok { "ok" } else { "unavailable" },
            cached_torrents: cached,
        }),
    )
}

// --- Metrics ---

pub async fn metrics_handler(State(s): State<AppState>) -> impl IntoResponse {
    // Update gauges from cache before rendering
    if let Ok(count) = s.db.run_blocking("metrics_count", |db| db.count()).await {
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
    // A compatible-client service without configured roots does not own `/`; reporting it as a
    // storage root gives callers a false answer about where writes may occur.
    let roots = s.cfg.storage_roots.clone();
    let fallback_roots = roots.clone();
    let rows = match tokio::task::spawn_blocking(move || {
        roots
            .iter()
            .map(|path| storage_root(path))
            .collect::<Vec<_>>()
    })
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(
                component = "api",
                operation = "storage_roots",
                result = "worker_failed",
                error = %crate::task_join_error_summary("storage root probe worker", &error),
                "storage root probe worker failed"
            );
            fallback_roots
                .iter()
                .map(|path| StorageRoot {
                    path: path.display().to_string(),
                    total_bytes: 0,
                    available_bytes: 0,
                    used_bytes: 0,
                    used_percent: 0.0,
                    readonly: false,
                    ok: false,
                    error: Some("storage root probe worker failed".to_owned()),
                })
                .collect()
        }
    };
    Json(serde_json::json!({ "roots": rows }))
}

pub async fn list_jobs() -> impl IntoResponse {
    // Jobs are owned by the TorrentNG client. A compatible-client service has no durable local job
    // store and must not turn an unavailable remote control plane into an
    // empty successful result.
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": {
                "code": "NOT_IMPLEMENTED",
                "message": "job control is only available in the direct TorrentNG client arrangement"
            }
        })),
    )
        .into_response()
}

pub async fn transfer_info(State(s): State<AppState>) -> impl IntoResponse {
    let backend_status =
        match tokio::time::timeout(Duration::from_secs(3), s.backend.health()).await {
            Ok(status) => status,
            Err(_) => BackendStatus::Unreachable,
        };
    if backend_status != BackendStatus::Connected {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
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
                component = "api",
                operation = "transfer_info",
                result = "error",
                error = %crate::sync::error_chain(&e),
                "transfer rates unavailable"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
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
                    component = "api",
                    operation = "transfer_info",
                    result = "error",
                    error = %crate::url_redaction::redact_display(&e),
                    "global transfer limits unavailable"
                );
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
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
        Json(serde_json::json!({
            "connection_status": "connected",
            "dl_info_speed": rates.download,
            "dl_info_data": totals.download,
            "up_info_speed": rates.upload,
            "up_info_data": totals.upload,
            "dl_rate_limit": limits.download_limit,
            "up_rate_limit": limits.upload_limit,
            "speed_limits_mode": limits.speed_limits_mode,
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    limit: Option<usize>,
    kind: Option<String>,
    level: Option<String>,
    last_known_id: Option<i64>,
}

pub async fn list_logs(
    State(s): State<AppState>,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let levels = query.level.map(|level| vec![level]).unwrap_or_default();
    let kind = query.kind;
    let last_known_id = query.last_known_id;
    match s
        .db
        .run_blocking("list_logs", move |db| {
            let level_refs = levels.iter().map(String::as_str).collect::<Vec<_>>();
            db.list_app_events_filtered(limit, kind.as_deref(), &level_refs, last_known_id)
        })
        .await
    {
        Ok(events) => Json(serde_json::json!({ "logs": events })).into_response(),
        Err(e) => {
            tracing::error!(
                component = "api",
                operation = "list_logs",
                result = "error",
                error = %crate::url_redaction::redact_display(&e),
                "list logs failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn tracker_health(State(s): State<AppState>) -> impl IntoResponse {
    match s
        .db
        .run_blocking("tracker_health", |db| db.tracker_health())
        .await
    {
        Ok(trackers) => Json(serde_json::json!({ "trackers": trackers })).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "tracker_health", result = "error", error = %crate::url_redaction::redact_display(&e), "tracker health query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn sidebar_facets(
    State(s): State<AppState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    match s
        .db
        .run_blocking("sidebar_facets", move |db| db.sidebar_facets(&params))
        .await
    {
        Ok(facets) => Json(facets).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "sidebar_facets", result = "error", error = %crate::url_redaction::redact_display(&e), "sidebar facet query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn engine_diagnostics(State(s): State<AppState>) -> impl IntoResponse {
    let mut diagnostics = serde_json::to_value(s.rt.engine_diagnostics().await)
        .unwrap_or_else(|_| serde_json::json!({}));
    if let serde_json::Value::Object(ref mut obj) = diagnostics {
        obj.insert(
            "backend".to_owned(),
            serde_json::json!({
                "type": s.backend.backend_type().as_str(),
                "capabilities": s.backend.capabilities(),
            }),
        );
    }
    Json(diagnostics)
}

pub async fn engine_commands(State(s): State<AppState>) -> impl IntoResponse {
    Json(s.rt.command_index().await)
}

// --- rTorrent managed settings ---

const RTORRENT_MANAGED_BEGIN: &str = "# TorrentNG managed settings begin";
const RTORRENT_MANAGED_END: &str = "# TorrentNG managed settings end";
const RTORRENT_CUSTOM_HEADER: &str =
    "# Custom rTorrent lines. These are imported after managed settings.";
const MAX_RTORRENT_OVERLAY_BYTES: usize = 1024 * 1024;

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
    custom_rc: Option<String>,
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
            IntSettingBounds::new(0, 100_000, 350),
        ),
        int_setting(
            "max_downloads_global",
            "Global download slots",
            "throttle.max_downloads.global",
            "throttle.max_downloads.global.set",
            None,
            false,
            IntSettingBounds::new(0, 100_000, 300),
        ),
        int_setting(
            "max_uploads",
            "Per-torrent upload slots",
            "throttle.max_uploads",
            "throttle.max_uploads.set",
            None,
            false,
            IntSettingBounds::new(0, 10_000, 10),
        ),
        int_setting(
            "max_downloads",
            "Per-torrent download slots",
            "throttle.max_downloads",
            "throttle.max_downloads.set",
            None,
            false,
            IntSettingBounds::new(0, 10_000, 12),
        ),
        int_setting(
            "pieces_memory_max",
            "Piece memory cache",
            "pieces.memory.max",
            "pieces.memory.max.set",
            Some("M"),
            false,
            IntSettingBounds::new(64, 262_144, 4096),
        ),
        int_setting(
            "max_open_files",
            "Open files",
            "network.max_open_files",
            "network.max_open_files.set",
            None,
            true,
            IntSettingBounds::new(64, 1_000_000, 4096),
        ),
        int_setting(
            "max_open_sockets",
            "Open sockets",
            "network.max_open_sockets",
            "network.max_open_sockets.set",
            None,
            true,
            IntSettingBounds::new(64, 1_000_000, 2048),
        ),
        int_setting(
            "http_max_open",
            "Tracker HTTP open requests",
            "network.http.max_open",
            "network.http.max_open.set",
            None,
            false,
            IntSettingBounds::new(1, 100_000, 512),
        ),
        int_setting(
            "http_max_total_connections",
            "Tracker HTTP total connections",
            "network.http.max_total_connections",
            "network.http.max_total_connections.set",
            None,
            false,
            IntSettingBounds::new(1, 100_000, 256),
        ),
        int_setting(
            "http_max_host_connections",
            "Tracker HTTP per-host connections",
            "network.http.max_host_connections",
            "network.http.max_host_connections.set",
            None,
            false,
            IntSettingBounds::new(1, 100_000, 64),
        ),
        int_setting(
            "http_max_cache_connections",
            "Tracker HTTP cache connections",
            "network.http.max_cache_connections",
            "network.http.max_cache_connections.set",
            None,
            false,
            IntSettingBounds::new(1, 100_000, 512),
        ),
        int_setting(
            "http_dns_cache_timeout",
            "Tracker DNS cache",
            "network.http.dns_cache_timeout",
            "network.http.dns_cache_timeout.set",
            Some("s"),
            false,
            IntSettingBounds::new(0, 86_400, 25),
        ),
        int_setting(
            "trackers_numwant",
            "Tracker numwant",
            "trackers.numwant",
            "trackers.numwant.set",
            None,
            false,
            IntSettingBounds::new(0, 10_000, 200),
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

#[derive(Clone, Copy)]
struct IntSettingBounds {
    minimum: i64,
    maximum: i64,
    default_value: i64,
}

impl IntSettingBounds {
    const fn new(minimum: i64, maximum: i64, default_value: i64) -> Self {
        Self {
            minimum,
            maximum,
            default_value,
        }
    }
}

fn int_setting(
    key: &'static str,
    label: &'static str,
    command: &'static str,
    setter: &'static str,
    unit: Option<&'static str>,
    restart_required: bool,
    bounds: IntSettingBounds,
) -> RtorrentSettingDescriptor {
    RtorrentSettingDescriptor {
        key,
        label,
        command,
        setter,
        value_type: "int",
        unit,
        restart_required,
        minimum: Some(bounds.minimum),
        maximum: Some(bounds.maximum),
        default_value: serde_json::json!(bounds.default_value),
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
    if !s.backend.capabilities().supports_config_overlay {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    let descriptors = rtorrent_settings();
    let overlay_path = rtorrent_overlay_path();
    let overlay_path_for_read = overlay_path.clone();
    let (saved, custom_rc, overlay_writable) = match tokio::task::spawn_blocking(move || {
        let value = read_rtorrent_overlay(&overlay_path_for_read)?;
        let writable = rtorrent_overlay_is_writable(&overlay_path_for_read);
        Ok::<_, std::io::Error>((value.0, value.1, writable))
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => {
            tracing::error!(component = "api", operation = "read_rtorrent_overlay", result = "error", error = %crate::url_redaction::redact_display(&e), "rTorrent overlay read failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "read_rtorrent_overlay", result = "worker_failed", error = %crate::task_join_error_summary("rTorrent overlay read worker", &e), "rTorrent overlay read worker failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
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
        overlay_writable,
        custom_rc,
        restart_supported: true,
    })
    .into_response()
}

pub async fn set_rtorrent_settings(
    State(s): State<AppState>,
    Json(patch): Json<RtorrentSettingsPatch>,
) -> impl IntoResponse {
    if !s.backend.capabilities().supports_config_overlay {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    let _write_guard = s.control_plane_write.lock().await;
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
    let descriptors_for_write = descriptors.clone();
    let normalized_for_write = normalized.clone();
    let custom_rc_for_write = patch.custom_rc.clone();
    let overlay_path_for_write = overlay_path.clone();
    let (_saved, custom_rc) = match tokio::task::spawn_blocking(move || {
        let (saved, custom_rc) = merge_rtorrent_overlay(
            &overlay_path_for_write,
            &normalized_for_write,
            custom_rc_for_write.as_deref(),
        )?;
        write_rtorrent_overlay(
            &overlay_path_for_write,
            &descriptors_for_write,
            &saved,
            &custom_rc,
        )?;
        Ok::<_, std::io::Error>((saved, custom_rc))
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => {
            tracing::error!(component = "api", operation = "write_rtorrent_overlay", result = "error", error = %crate::url_redaction::redact_display(&e), "rTorrent overlay write failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "write_rtorrent_overlay", result = "worker_failed", error = %crate::task_join_error_summary("rTorrent overlay write worker", &e), "rTorrent overlay write worker failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

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

    if !custom_rc.trim().is_empty() {
        restart_required = true;
    }

    let response = RtorrentSettingsApplyResponse {
        saved: true,
        restart_required,
        applied,
        errors: apply_errors,
        overlay_path: overlay_path.display().to_string(),
    };
    record_operator_event(
        &s,
        "settings_changed",
        "rTorrent settings saved",
        serde_json::json!({
            "component": "rtorrent",
            "operation": "apply_settings",
            "result": if response.errors.is_empty() { "saved" } else { "partial" },
            "changed_count": normalized.len(),
            "applied_live_count": response.applied.len(),
            "error_count": response.errors.len(),
            "restart_required": response.restart_required,
            "custom_rc": !custom_rc.trim().is_empty(),
            "overlay_file": overlay_path.file_name().and_then(|name| name.to_str()).unwrap_or("rtorrent.tng.rc"),
        }),
        if response.errors.is_empty() {
            "info"
        } else {
            "warn"
        },
    )
    .await;

    Json(response).into_response()
}

pub async fn restart_process(State(s): State<AppState>) -> impl IntoResponse {
    if !s.backend.capabilities().supports_restart {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    record_operator_event(
        &s,
        "admin_restart_requested",
        "TorrentNG restart requested",
        serde_json::json!({
            "component": "sidecar",
            "operation": "restart",
            "result": "requested",
        }),
        "warn",
    )
    .await;
    tokio::spawn(async {
        sleep(Duration::from_millis(250)).await;
        tracing::warn!(
            component = "sidecar",
            operation = "restart",
            "restarting TorrentNG process by admin request"
        );
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

fn rtorrent_overlay_is_writable(path: &FsPath) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let parent = if parent.as_os_str().is_empty() {
        FsPath::new(".")
    } else {
        parent
    };
    if !parent.is_dir() {
        return false;
    }

    let probe = parent.join(format!(".tng-overlay-probe.{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let Ok(file) = options.open(&probe) else {
        return false;
    };
    drop(file);
    std::fs::remove_file(probe).is_ok()
}

fn read_rtorrent_overlay(path: &FsPath) -> std::io::Result<(BTreeMap<String, String>, String)> {
    let mut bytes = Vec::new();
    match open_regular_read(path) {
        Ok(file) => {
            file.take(MAX_RTORRENT_OVERLAY_BYTES.saturating_add(1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_RTORRENT_OVERLAY_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "rTorrent overlay exceeds the configured size limit",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let raw = String::from_utf8(bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("rTorrent overlay is not valid UTF-8: {error}"),
        )
    })?;
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
        } else if in_custom && trimmed != RTORRENT_CUSTOM_HEADER {
            custom.push(line.to_owned());
        }
    }
    Ok((saved, custom.join("\n").trim().to_owned()))
}

fn merge_rtorrent_overlay(
    path: &FsPath,
    updates: &BTreeMap<String, String>,
    custom_rc: Option<&str>,
) -> std::io::Result<(BTreeMap<String, String>, String)> {
    let (mut saved, existing_custom_rc) = read_rtorrent_overlay(path)?;
    // The settings endpoint is a patch. Preserve managed settings and custom
    // lines that the caller did not include; writing defaults for omitted
    // fields would silently reset an operator's configuration.
    saved.extend(
        updates
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    Ok((saved, custom_rc.unwrap_or(&existing_custom_rc).to_owned()))
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
        out.push_str(RTORRENT_CUSTOM_HEADER);
        out.push('\n');
        out.push_str(custom);
        out.push('\n');
    }
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rTorrent overlay path has no valid file name",
        ));
    };
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let write_result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(out.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
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

pub async fn get_session_features(State(s): State<AppState>) -> impl IntoResponse {
    let (dht, pex) = s.backend.feature_status().await;
    Json(SessionFeatureResponse {
        dht: feature_status_to_bool(&dht),
        pex: feature_status_to_bool(&pex),
    })
    .into_response()
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
        if let Err(e) = s.backend.set_dht(enabled).await {
            tracing::error!(component = "backend", operation = "set_dht_mode", result = "error", enabled, error = %crate::url_redaction::redact_display(&e), "backend DHT mode update failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        dht = Some(enabled);
    }

    if let Some(enabled) = patch.pex {
        if let Err(e) = s.backend.set_pex(enabled).await {
            tracing::error!(component = "backend", operation = "set_pex", result = "error", enabled, error = %crate::url_redaction::redact_display(&e), "backend PEX update failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        pex = Some(enabled);
    }

    Json(SessionFeatureResponse { dht, pex }).into_response()
}

fn feature_status_to_bool(status: &str) -> Option<bool> {
    match status {
        "enabled" | "on" | "true" | "1" | "yes" => Some(true),
        "disabled" | "off" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

// --- Saved views ---

pub async fn list_saved_views(State(s): State<AppState>) -> impl IntoResponse {
    match s
        .db
        .run_blocking("list_saved_views", |db| db.list_saved_views())
        .await
    {
        Ok(views) => Json(views).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_saved_views", result = "error", error = %crate::url_redaction::redact_display(&e), "saved view listing failed");
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
    let _write_guard = s.control_plane_write.lock().await;
    match s
        .db
        .run_blocking("upsert_saved_view", move |db| db.upsert_saved_view(view))
        .await
    {
        Ok(views) => {
            emit(&s, Event::SavedViewsUpdated).await;
            Json(views).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_saved_view", result = "error", error = %crate::url_redaction::redact_display(&e), "saved view upsert failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn delete_saved_view(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let _write_guard = s.control_plane_write.lock().await;
    let delete_id = id.clone();
    match s
        .db
        .run_blocking("delete_saved_view", move |db| {
            db.delete_saved_view(&delete_id)
        })
        .await
    {
        Ok(views) => {
            emit(&s, Event::SavedViewsUpdated).await;
            Json(views).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_saved_view", result = "error", view_id = %crate::url_redaction::redact_display(&id), error = %crate::url_redaction::redact_display(&e), "saved view delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Ratio groups ---

pub async fn list_ratio_groups(State(s): State<AppState>) -> impl IntoResponse {
    match s
        .db
        .run_blocking("list_ratio_groups", |db| db.list_ratio_groups())
        .await
    {
        Ok(groups) => Json(groups).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_ratio_groups", result = "error", error = %crate::url_redaction::redact_display(&e), "ratio group listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn upsert_ratio_group(
    State(s): State<AppState>,
    Json(mut group): Json<RatioGroup>,
) -> impl IntoResponse {
    if group.name.len() > MAX_RATIO_GROUP_NAME_BYTES
        || group
            .category
            .as_deref()
            .is_some_and(|value| value.len() > MAX_API_CATEGORY_NAME_BYTES)
        || group
            .tracker
            .as_deref()
            .is_some_and(|value| value.len() > MAX_API_TRACKER_URL_BYTES)
    {
        return (StatusCode::BAD_REQUEST, "ratio group exceeds input limits").into_response();
    }
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
    let _write_guard = s.control_plane_write.lock().await;
    let name_for_capacity = group.name.clone();
    let existing = match s
        .db
        .run_blocking("check_ratio_group_capacity", |db| db.list_ratio_groups())
        .await
    {
        Ok(groups) => groups,
        Err(error) => {
            tracing::error!(component = "api", operation = "check_ratio_group_capacity", result = "error", error = %crate::url_redaction::redact_display(&error), "ratio group capacity check failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if existing.len() >= MAX_RATIO_GROUPS
        && !existing.iter().any(|entry| entry.name == name_for_capacity)
    {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    match s
        .db
        .run_blocking("upsert_ratio_group", move |db| db.upsert_ratio_group(group))
        .await
    {
        Ok(groups) => {
            emit(&s, Event::RatioGroupsUpdated).await;
            Json(groups).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_ratio_group", result = "error", error = %crate::url_redaction::redact_display(&e), "ratio group upsert failed");
            automation_state_write_status(&e).into_response()
        }
    }
}

pub async fn delete_ratio_group(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let _write_guard = s.control_plane_write.lock().await;
    let delete_name = name.clone();
    match s
        .db
        .run_blocking("delete_ratio_group", move |db| {
            db.delete_ratio_group(&delete_name)
        })
        .await
    {
        Ok(groups) => {
            emit(&s, Event::RatioGroupsUpdated).await;
            Json(groups).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_ratio_group", result = "error", ratio_group = %crate::url_redaction::redact_display(&name), error = %crate::url_redaction::redact_display(&e), "ratio group delete failed");
            automation_state_write_status(&e).into_response()
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
    let lookup_name = name.clone();
    let group = match s
        .db
        .run_blocking("get_ratio_group", move |db| {
            db.get_ratio_group(&lookup_name)
        })
        .await
    {
        Ok(Some(group)) => group,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "get_ratio_group", result = "error", ratio_group = %crate::url_redaction::redact_display(&name), error = %crate::url_redaction::redact_display(&e), "ratio group lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if !group.enabled {
        return (StatusCode::BAD_REQUEST, "ratio group is disabled").into_response();
    }
    if !s.backend.capabilities().supports_share_limits {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }

    let group_for_hashes = group.clone();
    let hashes = match s
        .db
        .run_blocking("ratio_group_hashes", move |db| {
            db.ratio_group_hashes(&group_for_hashes, MAX_BULK_TORRENTS.saturating_add(1))
        })
        .await
    {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(component = "api", operation = "ratio_group_hashes", result = "error", ratio_group = %crate::url_redaction::redact_display(&name), error = %crate::url_redaction::redact_display(&e), "ratio group hash query failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if hashes.len() > MAX_BULK_TORRENTS {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    if body.dry_run {
        return Json(BulkResult {
            applied: hashes,
            errors: vec![],
            dry_run: true,
        })
        .into_response();
    }

    let ratio_limit_milli = ratio_milli(Some(group.ratio_limit));
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for hash in hashes {
        match s
            .backend
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
    match s
        .db
        .run_blocking("list_workflows", |db| db.list_workflow_rules())
        .await
    {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_workflows", result = "error", error = %crate::url_redaction::redact_display(&e), "workflow rule listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn list_workflow_runs(State(s): State<AppState>) -> impl IntoResponse {
    match s
        .db
        .run_blocking("list_workflow_runs", |db| db.list_workflow_runs())
        .await
    {
        Ok(runs) => Json(runs).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_workflow_runs", result = "error", error = %crate::url_redaction::redact_display(&e), "workflow run listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn list_rss_rules(State(s): State<AppState>) -> impl IntoResponse {
    match s
        .db
        .run_blocking("list_rss_rules", |db| db.list_rss_rules())
        .await
    {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_rss_rules", result = "error", error = %crate::url_redaction::redact_display(&e), "RSS rule listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn upsert_rss_rule(
    State(s): State<AppState>,
    Json(mut rule): Json<RssRule>,
) -> impl IntoResponse {
    if rule.id.len() > MAX_RSS_RULE_ID_BYTES
        || rule.name.len() > MAX_RSS_RULE_NAME_BYTES
        || rule.feed_url.len() > MAX_RSS_RULE_FIELD_BYTES
        || rule.include.len() > MAX_RSS_RULE_FIELD_BYTES
        || rule
            .exclude
            .as_deref()
            .is_some_and(|value| value.len() > MAX_RSS_RULE_FIELD_BYTES)
        || rule
            .category
            .as_deref()
            .is_some_and(|value| value.len() > MAX_RSS_RULE_FIELD_BYTES)
        || rule
            .save_path
            .as_deref()
            .is_some_and(|value| value.len() > MAX_RSS_RULE_FIELD_BYTES)
        || rule.tags.len() > MAX_RSS_RULE_TAGS
        || rule
            .tags
            .iter()
            .any(|tag| tag.len() > MAX_RSS_RULE_TAG_BYTES)
    {
        return (StatusCode::BAD_REQUEST, "rss rule exceeds input limits").into_response();
    }
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

    let _write_guard = s.control_plane_write.lock().await;
    match s
        .db
        .run_blocking("upsert_rss_rule", move |db| db.upsert_rss_rule(rule))
        .await
    {
        Ok(RssRuleUpsertResult::Upserted(rules)) => {
            emit(&s, Event::RssRulesUpdated).await;
            Json(rules).into_response()
        }
        Ok(RssRuleUpsertResult::Capacity) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_rss_rule", result = "error", error = %crate::url_redaction::redact_display(&e), "RSS rule upsert failed");
            automation_state_write_status(&e).into_response()
        }
    }
}

pub async fn delete_rss_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let _write_guard = s.control_plane_write.lock().await;
    let delete_id = id.clone();
    match s
        .db
        .run_blocking("delete_rss_rule", move |db| db.delete_rss_rule(&delete_id))
        .await
    {
        Ok(rules) => {
            emit(&s, Event::RssRulesUpdated).await;
            Json(rules).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_rss_rule", result = "error", rule_id = %crate::url_redaction::redact_display(&id), error = %crate::url_redaction::redact_display(&e), "RSS rule delete failed");
            automation_state_write_status(&e).into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct TestRssRuleBody {
    #[serde(deserialize_with = "deserialize_rss_sample_text")]
    pub title: String,
    #[serde(default, deserialize_with = "deserialize_optional_rss_sample_text")]
    pub link: Option<String>,
}

fn deserialize_rss_sample_text<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedJsonString::<MAX_RSS_RULE_FIELD_BYTES>::deserialize(deserializer).map(|value| value.0)
}

fn deserialize_optional_rss_sample_text<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<BoundedJsonString<MAX_RSS_RULE_FIELD_BYTES>>::deserialize(deserializer)
        .map(|value| value.map(|value| value.0))
}

pub async fn test_rss_rules(
    State(s): State<AppState>,
    Json(body): Json<TestRssRuleBody>,
) -> impl IntoResponse {
    let title = body.title.trim();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "title must not be empty").into_response();
    }
    let title = title.to_owned();
    let link = body.link.clone();
    match s
        .db
        .run_blocking("test_rss_rules", move |db| {
            db.match_rss_item(&title, link.as_deref())
        })
        .await
    {
        Ok(matches) => Json(serde_json::json!({ "matches": matches })).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "test_rss_rules", result = "error", error = %crate::url_redaction::redact_display(&e), "RSS rule test failed");
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

    let title = title.to_owned();
    let link = link.to_owned();
    let link_for_match = link.clone();
    let matches = match s
        .db
        .run_blocking("apply_rss_rules", move |db| {
            db.match_rss_item(&title, Some(&link_for_match))
        })
        .await
    {
        Ok(matches) => matches,
        Err(e) => {
            tracing::error!(component = "api", operation = "apply_rss_rules", result = "error", error = %crate::url_redaction::redact_display(&e), "RSS rule apply failed");
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
            .load_magnet(&link, save_path, category, rule_match.start)
            .await
        {
            Ok(()) => applied.push(rule_match.rule_name),
            Err(_) => errors.push(rss_magnet_failure_summary(&rule_match.rule_name)),
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
    #[serde(deserialize_with = "deserialize_cross_seed_hashes")]
    pub hashes: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_cross_seed_trackers")]
    pub trackers: Vec<String>,
    #[serde(default)]
    pub reannounce: bool,
    #[serde(default)]
    pub dry_run: bool,
}

fn deserialize_cross_seed_hashes<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_strings::<D, MAX_BULK_TORRENTS, { MAX_BULK_HASH_CHARS * 4 }>(deserializer)
}

fn deserialize_cross_seed_trackers<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_strings::<D, MAX_API_TRACKER_MUTATIONS, MAX_API_TRACKER_URL_BYTES>(
        deserializer,
    )
}

pub async fn cross_seed_helper(
    State(s): State<AppState>,
    Json(body): Json<CrossSeedBody>,
) -> impl IntoResponse {
    let mut seen_hashes = HashSet::new();
    let hashes = normalized_nonempty(&body.hashes)
        .into_iter()
        .filter(|hash| seen_hashes.insert(hash.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    if hashes.is_empty() {
        return (StatusCode::BAD_REQUEST, "hashes must not be empty").into_response();
    }
    let mut seen_trackers = HashSet::new();
    let trackers = normalized_nonempty(&body.trackers)
        .into_iter()
        .filter(|tracker| seen_trackers.insert((*tracker).to_owned()))
        .collect::<Vec<_>>();
    if trackers.is_empty() && !body.reannounce {
        return (
            StatusCode::BAD_REQUEST,
            "trackers or reannounce must be provided",
        )
            .into_response();
    }
    let backend_operations = hashes
        .len()
        .saturating_mul(trackers.len())
        .saturating_add(if body.reannounce { hashes.len() } else { 0 });
    if !body.dry_run && backend_operations > MAX_CROSS_SEED_OPERATIONS {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("cross-seed request exceeds {MAX_CROSS_SEED_OPERATIONS} backend operations"),
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
            if s.backend.add_tracker(hash, tracker).await.is_err() {
                hash_errors.push(tracker_failure_summary("add tracker", tracker));
            }
        }
        if body.reannounce {
            if let Err(e) = s.backend.reannounce(hash).await {
                hash_errors.push(format!("reannounce: {e}"));
            }
        }
        if hash_errors.is_empty() {
            applied.push(hash.to_owned());
            emit_torrent_updated(&s, hash).await;
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
    #[serde(deserialize_with = "deserialize_rss_sample_text")]
    pub title: String,
    #[serde(default, deserialize_with = "deserialize_optional_rss_sample_text")]
    pub link: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

pub async fn upsert_workflow(
    State(s): State<AppState>,
    Json(mut rule): Json<WorkflowRule>,
) -> impl IntoResponse {
    if workflow_rule_exceeds_input_limits(&rule) {
        return (
            StatusCode::BAD_REQUEST,
            "workflow rule exceeds input limits",
        )
            .into_response();
    }
    let target_category_was_supplied = rule.target_category.is_some();
    rule.name = rule.name.trim().to_owned();
    rule.event = rule.event.trim().to_owned();
    rule.action = rule.action.trim().to_owned();
    rule.category = rule
        .category
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    rule.target_category = rule
        .target_category
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    if rule.action == "set_category" && !target_category_was_supplied {
        // Keep older clients that submitted the target in `category` working.
        rule.target_category = rule.category.take();
    }
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
    if rule.action == "set_category" && rule.target_category.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "target_category is required for set_category rules",
        )
            .into_response();
    }
    if [rule.category.as_deref(), rule.target_category.as_deref()]
        .into_iter()
        .flatten()
        .any(|category| category.len() > MAX_API_CATEGORY_NAME_BYTES)
    {
        return (
            StatusCode::BAD_REQUEST,
            "workflow category and target_category are limited to 256 bytes",
        )
            .into_response();
    }

    let _write_guard = s.control_plane_write.lock().await;
    let requested_id = rule.id.clone();
    let creating = requested_id.trim().is_empty();
    let existing = match s
        .db
        .run_blocking("check_workflow_rule_capacity", |db| {
            db.list_workflow_rules()
        })
        .await
    {
        Ok(rules) => rules,
        Err(error) => {
            tracing::error!(component = "api", operation = "check_workflow_rule_capacity", result = "error", error = %crate::url_redaction::redact_display(&error), "workflow rule capacity check failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if existing.len() >= MAX_WORKFLOW_RULES
        && (creating || !existing.iter().any(|entry| entry.id == requested_id))
    {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    match s
        .db
        .run_blocking("upsert_workflow_rule", move |db| {
            db.upsert_workflow_rule(rule)
        })
        .await
    {
        Ok(rules) => {
            emit(&s, Event::WorkflowsUpdated).await;
            Json(rules).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_workflow_rule", result = "error", error = %crate::url_redaction::redact_display(&e), "workflow rule upsert failed");
            automation_state_write_status(&e).into_response()
        }
    }
}

fn workflow_rule_exceeds_input_limits(rule: &WorkflowRule) -> bool {
    rule.id.len() > MAX_WORKFLOW_ID_BYTES
        || rule.name.len() > MAX_WORKFLOW_NAME_BYTES
        || rule.event.len() > MAX_WORKFLOW_TOKEN_BYTES
        || rule.action.len() > MAX_WORKFLOW_TOKEN_BYTES
        || rule
            .category
            .as_deref()
            .is_some_and(|value| value.len() > MAX_API_CATEGORY_NAME_BYTES)
        || rule
            .target_category
            .as_deref()
            .is_some_and(|value| value.len() > MAX_API_CATEGORY_NAME_BYTES)
        || rule
            .tracker
            .as_deref()
            .is_some_and(|value| value.len() > MAX_WORKFLOW_TEXT_BYTES)
        || rule
            .command
            .as_deref()
            .is_some_and(|value| value.len() > MAX_WORKFLOW_TEXT_BYTES)
        || rule
            .url
            .as_deref()
            .is_some_and(|value| value.len() > MAX_WORKFLOW_TEXT_BYTES)
        || rule
            .target_path
            .as_deref()
            .is_some_and(|value| value.len() > MAX_WORKFLOW_PATH_BYTES)
}

pub async fn delete_workflow(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let _write_guard = s.control_plane_write.lock().await;
    let delete_id = id.clone();
    match s
        .db
        .run_blocking("delete_workflow_rule", move |db| {
            db.delete_workflow_rule(&delete_id)
        })
        .await
    {
        Ok(rules) => {
            emit(&s, Event::WorkflowsUpdated).await;
            Json(rules).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_workflow_rule", result = "error", rule_id = %crate::url_redaction::redact_display(&id), error = %crate::url_redaction::redact_display(&e), "workflow rule delete failed");
            automation_state_write_status(&e).into_response()
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
    let lookup_id = id.clone();
    let rule = match s
        .db
        .run_blocking("get_workflow_rule", move |db| {
            db.get_workflow_rule(&lookup_id)
        })
        .await
    {
        Ok(Some(rule)) => rule,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "get_workflow_rule", result = "error", rule_id = %crate::url_redaction::redact_display(&id), error = %crate::url_redaction::redact_display(&e), "workflow rule lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if !rule.enabled {
        return (StatusCode::BAD_REQUEST, "workflow rule is disabled").into_response();
    }
    let rule_for_hashes = rule.clone();
    let hashes = match s
        .db
        .run_blocking("workflow_hashes", move |db| {
            db.workflow_hashes(&rule_for_hashes, MAX_BULK_TORRENTS.saturating_add(1))
        })
        .await
    {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!(component = "api", operation = "workflow_hashes", result = "error", rule_id = %crate::url_redaction::redact_display(&id), error = %crate::url_redaction::redact_display(&e), "workflow hash query failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if hashes.len() > MAX_BULK_TORRENTS {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    if body.dry_run {
        if let Err(error) =
            record_workflow_run(&s, &rule, true, hashes.clone(), hashes.clone(), Vec::new()).await
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("workflow dry-run history could not be persisted: {error}"),
            )
                .into_response();
        }
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
    if rule.action == "set_category" && !s.backend.capabilities().supports_categories {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    if rule.action == "set_location" && !s.backend.capabilities().supports_location_update {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    if rule.action == "set_category" {
        let Some(category) = rule.target_category.as_deref() else {
            return (StatusCode::BAD_REQUEST, "target_category is not configured").into_response();
        };
        let category_name = category.to_owned();
        if let Err(error) =
            s.db.run_blocking("preflight_workflow_set_category", move |db| {
                crate::cache::categories::ensure_category_name_capacity(db, &category_name)
            })
            .await
        {
            tracing::warn!(component = "cache", operation = "preflight_workflow_set_category", category = %crate::url_redaction::redact_display(&category), result = "error", error = %crate::url_redaction::redact_display(&error), "workflow category update rejected before backend mutation");
            return metadata_write_status(&error).into_response();
        }
    }
    for hash in hashes {
        match rule.action.as_str() {
            "set_category" => {
                let Some(category) = rule.target_category.as_deref() else {
                    errors.push(format!("{hash}: target_category is not configured"));
                    continue;
                };
                match s.backend.set_category(&hash, category).await {
                    Ok(()) => {
                        let cache_hash = hash.clone();
                        let cache_category = category.to_owned();
                        if let Err(e) =
                            s.db.run_blocking("workflow_set_category_cache", move |db| {
                                db.set_torrent_category(&cache_hash, &cache_category)
                            })
                            .await
                        {
                            errors.push(format!("{hash}: {e}"));
                        } else {
                            emit_torrent_updated(&s, &hash).await;
                            applied.push(hash);
                        }
                    }
                    Err(e) => errors.push(format!("{hash}: {e}")),
                }
            }
            "set_location" => {
                let Some(target_path) = rule.target_path.as_deref() else {
                    errors.push(format!("{hash}: target_path is not configured"));
                    continue;
                };
                match s.backend.set_location(&hash, target_path).await {
                    Ok(()) => {
                        let cache_hash = hash.clone();
                        let cache_path = target_path.to_owned();
                        if let Err(e) =
                            s.db.run_blocking("workflow_set_location_cache", move |db| {
                                db.set_torrent_location(&cache_hash, &cache_path)
                            })
                            .await
                        {
                            errors.push(format!("{hash}: {e}"));
                            continue;
                        }
                        emit_torrent_updated(&s, &hash).await;
                        applied.push(hash);
                    }
                    Err(e) => errors.push(format!("{hash}: {e}")),
                }
            }
            "webhook" => {
                match execute_workflow_webhook(&rule, &hash, s.cfg.workflows.allow_private_webhooks)
                    .await
                {
                    Ok(()) => applied.push(hash),
                    Err(e) => errors.push(format!("{hash}: {e}")),
                }
            }
            "script" => match execute_workflow_script(&s, &rule, &hash).await {
                Ok(()) => applied.push(hash),
                Err(e) => errors.push(format!("{hash}: {e}")),
            },
            _ => errors.push(format!("{hash}: unsupported action {}", rule.action)),
        }
    }

    let history_error =
        record_workflow_run(&s, &rule, false, matched, applied.clone(), errors.clone())
            .await
            .err();
    let history_failed = history_error.is_some();
    if let Some(error) = history_error {
        errors.push(format!("workflow history could not be persisted: {error}"));
    }

    let result = Json(BulkResult {
        applied,
        errors,
        dry_run: false,
    });
    if history_failed {
        (StatusCode::INTERNAL_SERVER_ERROR, result).into_response()
    } else {
        result.into_response()
    }
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
    let args = parts.map(str::to_owned).collect::<Vec<_>>();
    let program_path = PathBuf::from(program);
    let allowed_script_dirs = s.cfg.workflows.allowed_script_dirs.clone();
    let canonical = tokio::task::spawn_blocking(move || {
        if !program_path.is_absolute() {
            return Err("script command must use an absolute executable path".to_owned());
        }
        let canonical = program_path
            .canonicalize()
            .map_err(|e| format!("canonicalize script: {e}"))?;
        let allowed = allowed_script_dirs.iter().any(|dir| {
            dir.canonicalize()
                .map(|allowed_dir| canonical.starts_with(allowed_dir))
                .unwrap_or(false)
        });
        if !allowed {
            return Err("script path is outside allowed_script_dirs".to_owned());
        }
        Ok::<_, String>(canonical)
    })
    .await
    .map_err(|error| crate::task_join_error_summary("script path validation worker", &error))??;

    const MAX_SCRIPT_OUTPUT_BYTES: u64 = 64 * 1024;

    let mut child = Command::new(canonical);
    child
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("TNG_WORKFLOW_ID", &rule.id)
        .env("TNG_WORKFLOW_NAME", &rule.name)
        .env("TNG_TORRENT_HASH", hash);
    if let Some(category) = &rule.category {
        child.env("TNG_CATEGORY", category);
    }
    if let Some(tracker) = &rule.tracker {
        child.env("TNG_TRACKER", tracker);
    }
    let mut child = child
        .spawn()
        .map_err(|e| format!("script failed to start: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "script stdout pipe was not created".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "script stderr pipe was not created".to_owned())?;
    let (stdout, stderr, status) = wait_for_script_output(
        &mut child,
        stdout,
        stderr,
        MAX_SCRIPT_OUTPUT_BYTES,
        Duration::from_secs(s.cfg.workflows.script_timeout_secs.max(1)),
    )
    .await?;
    if !stdout.is_empty() {
        tracing::info!(
            component = "workflow",
            operation = "script",
            stream = "stdout",
            output = %escape_script_log_output(&stdout),
            "workflow script output"
        );
    }
    if !stderr.is_empty() {
        tracing::warn!(
            component = "workflow",
            operation = "script",
            stream = "stderr",
            output = %escape_script_log_output(&stderr),
            "workflow script error output"
        );
    }
    if status.success() {
        Ok(())
    } else {
        Err(format!("script exited with {status}"))
    }
}

fn escape_script_log_output(bytes: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(bytes);
    crate::log_sanitization::sanitize_operator_log_text(&decoded)
}

async fn read_script_output<R: AsyncRead + Unpin>(
    stream: R,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    stream
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut output)
        .await
        .map_err(|e| format!("read script output: {e}"))?;
    if output.len() as u64 > max_bytes {
        return Err(format!("script output exceeds {max_bytes} bytes"));
    }
    Ok(output)
}

async fn wait_for_script_output<Out, ErrOut>(
    child: &mut tokio::process::Child,
    stdout: Out,
    stderr: ErrOut,
    max_bytes: u64,
    timeout: Duration,
) -> Result<(Vec<u8>, Vec<u8>, std::process::ExitStatus), String>
where
    Out: AsyncRead + Unpin,
    ErrOut: AsyncRead + Unpin,
{
    let output = tokio::time::timeout(timeout, async {
        let status = async {
            child
                .wait()
                .await
                .map_err(|error| format!("script wait failed: {error}"))
        };
        tokio::try_join!(
            read_script_output(stdout, max_bytes),
            read_script_output(stderr, max_bytes),
            status,
        )
    })
    .await;

    match output {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => {
            // A reader stops at max_bytes + 1. If the child is still writing
            // to that pipe, waiting for it can deadlock once the pipe fills.
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(error)
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err("script timed out".to_owned())
        }
    }
}

async fn execute_workflow_webhook(
    rule: &WorkflowRule,
    hash: &str,
    allow_private: bool,
) -> Result<(), String> {
    let Some(url) = rule.url.as_deref() else {
        return Err("url is not configured".to_owned());
    };
    post_remote_json(
        url,
        &serde_json::json!({
            "workflow_id": rule.id,
            "workflow_name": rule.name,
            "event": rule.event,
            "action": rule.action,
            "hash": hash,
            "category": rule.category,
            "tracker": rule.tracker,
            "timestamp": chrono::Utc::now().timestamp(),
        }),
        allow_private,
    )
    .await
    .map_err(|e| e.to_string())
}

async fn record_workflow_run(
    s: &AppState,
    rule: &WorkflowRule,
    dry_run: bool,
    matched: Vec<String>,
    applied: Vec<String>,
    errors: Vec<String>,
) -> Result<(), String> {
    let run = WorkflowRun {
        id: uuid::Uuid::new_v4().to_string(),
        rule_id: rule.id.clone(),
        rule_name: rule.name.clone(),
        action: rule.action.clone(),
        dry_run,
        matched_total: matched.len(),
        matched,
        applied_total: applied.len(),
        applied,
        errors_total: errors.len(),
        errors,
        started_at: chrono::Utc::now().timestamp(),
    };
    let _write_guard = s.control_plane_write.lock().await;
    let result =
        s.db.run_blocking("record_workflow_run", move |db| db.record_workflow_run(run))
            .await;
    if let Err(e) = result {
        tracing::error!(component = "api", operation = "record_workflow_run", result = "error", rule_id = %rule.id, error = %crate::url_redaction::redact_display(&e), "workflow run record failed");
        Err(e.to_string())
    } else {
        emit(s, Event::WorkflowRunsUpdated).await;
        Ok(())
    }
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
    kind: &str,
    message: &str,
    payload: serde_json::Value,
    level: &str,
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
        tracing::warn!(component = "app_events", operation = "append", result = "error", error = %crate::url_redaction::redact_display(&e), "failed to append app event");
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
    s.db.run_blocking("set_torrent_runtime_state", move |db| {
        db.set_torrent_runtime_state(&hash, state, active, open)
    })
    .await
    .map_err(|error| error.to_string())
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
    let block_size = statvfs_block_size(stat.f_frsize, stat.f_bsize);
    Ok(FsStat {
        total_bytes: stat.f_blocks.saturating_mul(block_size),
        available_bytes: stat.f_bavail.saturating_mul(block_size),
        readonly: (stat.f_flag & libc::ST_RDONLY) != 0,
    })
}

#[cfg(any(unix, test))]
fn statvfs_block_size<T>(fragment_size: T, block_size: T) -> T
where
    T: Default + PartialEq,
{
    if fragment_size == T::default() {
        block_size
    } else {
        fragment_size
    }
}

#[cfg(not(unix))]
fn statvfs(_path: &FsPath) -> Result<FsStat, String> {
    Err("storage stats are unsupported on this platform".to_owned())
}

// --- Torrent list ---

const MAX_TORRENT_LIVE_STATS: usize = 128;
const MAX_TORRENT_LIVE_HASH_QUERY_BYTES: usize = MAX_TORRENT_LIVE_STATS * 65;
const LIVE_RATE_STALE_AFTER_SECS: i64 = 15;

#[derive(Deserialize)]
pub struct TorrentLiveStatsQuery {
    hashes: Option<BoundedQueryString<MAX_TORRENT_LIVE_HASH_QUERY_BYTES>>,
}

struct BoundedQueryString<const MAX_BYTES: usize>(String);

impl<'de, const MAX_BYTES: usize> Deserialize<'de> for BoundedQueryString<MAX_BYTES> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct QueryStringVisitor<const MAX_BYTES: usize>;

        impl<'de, const MAX_BYTES: usize> Visitor<'de> for QueryStringVisitor<MAX_BYTES> {
            type Value = BoundedQueryString<MAX_BYTES>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "a query string of at most {MAX_BYTES} bytes")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_BYTES {
                    return Err(E::custom("query value exceeds its byte limit"));
                }
                Ok(BoundedQueryString(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_BYTES {
                    return Err(E::custom("query value exceeds its byte limit"));
                }
                Ok(BoundedQueryString(value))
            }
        }

        deserializer.deserialize_str(QueryStringVisitor::<MAX_BYTES>)
    }
}

#[derive(Debug, Serialize)]
struct TorrentLiveStatResponse {
    hash: String,
    amount_left: u64,
    download_rate: i64,
    upload_rate: i64,
    sampled_at: u64,
}

#[derive(Debug, Serialize)]
struct TorrentLiveStatsResponse {
    sampled_at: u64,
    torrents: Vec<TorrentLiveStatResponse>,
}

fn parse_torrent_live_hashes(raw: Option<&str>) -> Result<Vec<String>, String> {
    let raw = raw.ok_or_else(|| "hashes is required".to_owned())?;
    if raw.len() > MAX_TORRENT_LIVE_HASH_QUERY_BYTES {
        return Err(format!(
            "hashes query must be at most {MAX_TORRENT_LIVE_HASH_QUERY_BYTES} bytes"
        ));
    }
    let mut hashes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut requested = 0;
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        requested += 1;
        if requested > MAX_TORRENT_LIVE_STATS {
            return Err(format!(
                "at most {MAX_TORRENT_LIVE_STATS} hashes may be requested"
            ));
        }
        if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(
                "hashes must contain 40- or 64-character hexadecimal info hashes".to_owned(),
            );
        }
        let value = value.to_ascii_lowercase();
        if seen.insert(value.clone()) {
            hashes.push(value);
        }
    }
    if hashes.is_empty() {
        return Err("hashes must contain at least one info hash".to_owned());
    }
    Ok(hashes)
}

fn live_sampled_at() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn live_row_response(row: TorrentLiveRow, sampled_at: u64) -> TorrentLiveStatResponse {
    let size_bytes = u64::try_from(row.size_bytes.max(0)).unwrap_or(0);
    let bytes_done = u64::try_from(row.bytes_done.max(0)).unwrap_or(0);
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64;
    let fresh =
        row.updated_at > 0 && now_secs.saturating_sub(row.updated_at) <= LIVE_RATE_STALE_AFTER_SECS;
    TorrentLiveStatResponse {
        hash: row.hash,
        amount_left: size_bytes.saturating_sub(bytes_done),
        download_rate: if fresh { row.down_rate.max(0) } else { 0 },
        upload_rate: if fresh { row.up_rate.max(0) } else { 0 },
        sampled_at: if row.updated_at > 0 {
            u64::try_from(row.updated_at)
                .unwrap_or(sampled_at.saturating_div(1_000))
                .saturating_mul(1_000)
        } else {
            sampled_at
        },
    }
}

/// `GET /api/v1/torrents/live?hashes=a,b,...` — cached live rates for the
/// explicitly visible torrents. It does not call the remote backend.
pub async fn live_torrent_stats(
    State(s): State<AppState>,
    Query(query): Query<TorrentLiveStatsQuery>,
) -> impl IntoResponse {
    s.metrics.api_requests_total.fetch_add(1, Ordering::Relaxed);
    let hashes =
        match parse_torrent_live_hashes(query.hashes.as_ref().map(|query| query.0.as_str())) {
            Ok(hashes) => hashes,
            Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
        };
    let rows = match s
        .db
        .run_blocking("live_torrent_stats", move |db| db.live_stats(&hashes))
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(
                component = "api",
                operation = "live_torrent_stats",
                result = "error",
                error = %crate::url_redaction::redact_display(&error),
                "live torrent stats query failed"
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let sampled_at = live_sampled_at();
    let torrents = rows
        .into_iter()
        .map(|row| live_row_response(row, sampled_at))
        .collect();
    Json(TorrentLiveStatsResponse {
        sampled_at,
        torrents,
    })
    .into_response()
}

pub async fn list_torrents(
    State(s): State<AppState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    s.metrics.api_requests_total.fetch_add(1, Ordering::Relaxed);
    let mut params = params;
    if let Err(error) = validate_page_offset(params.offset) {
        return (StatusCode::BAD_REQUEST, error.to_string()).into_response();
    }
    // The compatible-client service cache is a compatibility projection, not an export API.
    // Clamp the caller-controlled page before the SQL query so a client
    // cannot turn this endpoint into a large response by bypassing the
    // TorrentNG handler's limit contract.
    params.limit = bounded_page_limit(params.limit);
    match s
        .db
        .run_blocking("list_torrents", move |db| db.list(&params))
        .await
    {
        Ok((rows, total)) => {
            Json(serde_json::json!({ "total": total, "torrents": rows })).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "list_torrents", result = "error", error = %crate::url_redaction::redact_display(&e), "torrent list query failed");
            metadata_read_status(&e).into_response()
        }
    }
}

// --- Single torrent ---

pub async fn get_torrent(State(s): State<AppState>, Path(hash): Path<String>) -> impl IntoResponse {
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    let lookup_hash = hash.clone();
    match s
        .db
        .run_blocking("get_torrent", move |db| db.get(&lookup_hash))
        .await
    {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "get_torrent", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "torrent lookup failed");
            metadata_read_status(&e).into_response()
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

    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };

    if let Err(e) = s.backend.set_location(&hash, save_path).await {
        tracing::error!(component = "api", operation = "set_location", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "backend location update failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let cache_hash = hash.clone();
    let cache_path = save_path.to_owned();
    if let Err(e) =
        s.db.run_blocking("update_torrent_location", move |db| {
            db.set_torrent_location(&cache_hash, &cache_path)
        })
        .await
    {
        tracing::error!(component = "cache", operation = "set_location", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "cache location update failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    emit_torrent_updated(&s, &hash).await;
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
    let mut multipart_field_count = 0usize;
    let mut seen_multipart_fields = HashSet::new();

    loop {
        let Some(field) = (match multipart.next_field().await {
            Ok(field) => field,
            Err(error) => {
                tracing::warn!(
                    component = "api",
                    operation = "add_torrent",
                    result = "bad_request",
                    error = %crate::url_redaction::redact_display(&error),
                    "invalid multipart request"
                );
                return (StatusCode::BAD_REQUEST, "invalid multipart request").into_response();
            }
        }) else {
            break;
        };
        multipart_field_count += 1;
        let Some(field_name) = field.name().map(str::to_owned) else {
            return (StatusCode::BAD_REQUEST, "multipart field is missing a name").into_response();
        };
        if multipart_field_count > MAX_API_TORRENT_ADD_FIELDS
            || !seen_multipart_fields.insert(field_name.clone())
        {
            return (
                StatusCode::BAD_REQUEST,
                "multipart fields must be unique and within the field limit",
            )
                .into_response();
        }
        match field_name.as_str() {
            "save_path" => {
                let value = match crate::multipart::read_bounded_multipart_text(
                    field,
                    MAX_API_CATEGORY_PATH_BYTES,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid save_path field: {error}"),
                        )
                            .into_response();
                    }
                };
                if value.len() > MAX_API_CATEGORY_PATH_BYTES {
                    return (
                        StatusCode::BAD_REQUEST,
                        "save_path exceeds the 4096-byte limit",
                    )
                        .into_response();
                }
                save_path = value;
            }
            "category" => {
                let value = match crate::multipart::read_bounded_multipart_text(
                    field,
                    MAX_API_CATEGORY_NAME_BYTES,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid category field: {error}"),
                        )
                            .into_response();
                    }
                };
                if value.len() > MAX_API_CATEGORY_NAME_BYTES {
                    return (
                        StatusCode::BAD_REQUEST,
                        "category exceeds the 256-byte limit",
                    )
                        .into_response();
                }
                category = value;
            }
            "start" => {
                let value = match crate::multipart::read_bounded_multipart_text(field, 5).await {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid start field: {error}"),
                        )
                            .into_response();
                    }
                };
                start = match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return (StatusCode::BAD_REQUEST, "start must be true or false")
                            .into_response();
                    }
                };
            }
            "magnet" => {
                if torrent_data.is_some() {
                    return (
                        StatusCode::BAD_REQUEST,
                        "provide exactly one of magnet or torrent",
                    )
                        .into_response();
                }
                let value = match crate::multipart::read_bounded_multipart_text(
                    field,
                    MAX_API_MAGNET_BYTES,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid magnet field: {error}"),
                        )
                            .into_response();
                    }
                };
                if value.len() > MAX_API_MAGNET_BYTES {
                    return (
                        StatusCode::BAD_REQUEST,
                        "magnet exceeds the 8192-byte limit",
                    )
                        .into_response();
                }
                magnet = Some(value);
            }
            "torrent" => {
                if magnet.is_some() {
                    return (
                        StatusCode::BAD_REQUEST,
                        "provide exactly one of magnet or torrent",
                    )
                        .into_response();
                }
                torrent_data = Some(
                    match crate::multipart::read_bounded_multipart_bytes(
                        field,
                        crate::multipart::MAX_MULTIPART_TORRENT_BYTES,
                    )
                    .await
                    {
                        Ok(value) => value,
                        Err(error) => {
                            return (
                                StatusCode::BAD_REQUEST,
                                format!("invalid torrent field: {error}"),
                            )
                                .into_response();
                        }
                    },
                );
            }
            _ => {
                return (StatusCode::BAD_REQUEST, "unsupported torrent-add field").into_response();
            }
        }
    }

    if magnet.is_some() == torrent_data.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "provide exactly one of magnet or torrent",
        )
            .into_response();
    }
    if torrent_data.as_ref().is_some_and(Vec::is_empty) {
        return (StatusCode::BAD_REQUEST, "torrent file must not be empty").into_response();
    }
    let capacity_category = category.clone();
    if let Err(error) =
        s.db.run_blocking("preflight_add_torrent_category", move |db| {
            crate::cache::categories::ensure_category_name_capacity(db, &capacity_category)
        })
        .await
    {
        tracing::warn!(component = "cache", operation = "preflight_add_torrent_category", result = "error", category = %crate::url_redaction::redact_display(&category), error = %crate::url_redaction::redact_display(&error), "torrent add rejected before backend mutation");
        return metadata_write_status(&error).into_response();
    }

    if let Some(m) = magnet {
        let m = m.trim();
        if m.is_empty() {
            return (StatusCode::BAD_REQUEST, "magnet must not be empty").into_response();
        }
        if s.backend
            .add_magnet(m, &save_path, &category, start)
            .await
            .is_err()
        {
            tracing::error!(
                component = "api",
                operation = "add_magnet",
                source = %redact_log_url(m),
                result = "error",
                "TorrentNG client magnet add failed"
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        return StatusCode::ACCEPTED.into_response();
    }
    if let Some(data) = torrent_data {
        if s.backend
            .add_torrent(&data, &save_path, &category, start)
            .await
            .is_err()
        {
            tracing::error!(
                component = "api",
                operation = "add_torrent",
                result = "error",
                "TorrentNG client torrent add failed"
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        return StatusCode::ACCEPTED.into_response();
    }
    (StatusCode::BAD_REQUEST, "missing torrent or magnet").into_response()
}

// --- Delete ---

pub async fn delete_torrent(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let delete_files = match q.get("delete_files") {
        Some(value) => match value.as_str() {
            "true" => true,
            "false" => false,
            _ => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => false,
    };
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.remove(&hash, delete_files).await {
        Ok(_) => {
            let cache_hash = hash.clone();
            if let Err(e) =
                s.db.run_blocking("delete_torrent", move |db| db.delete(&cache_hash))
                    .await
            {
                tracing::warn!(
                    component = "cache",
                    operation = "delete_torrent",
                    torrent = %crate::url_redaction::redact_display(&hash),
                    result = "error",
                    error = %crate::url_redaction::redact_display(&e),
                    "cache delete failed after TorrentNG client delete"
                );
                // The backend is already mutated, but reporting success here
                // would leave every cache consumer with a false view of the
                // torrent and suppress the only removal event. Surface the
                // partial failure so the caller can retry/reconcile it.
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            emit(&s, Event::TorrentRemoved { hash }).await;
            emit(&s, Event::TrackerHealthUpdated).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "api",
                operation = "delete_torrent",
                torrent = %crate::url_redaction::redact_display(&hash),
                result = "error",
                error = %crate::url_redaction::redact_display(&e),
                "TorrentNG client delete failed"
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
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.start(&hash).await {
        Ok(_) => {
            if let Err(error) = update_cached_lifecycle_state(&s, &hash, "start").await {
                tracing::error!(
                    component = "cache",
                    operation = "set_torrent_runtime_state",
                    torrent = %crate::url_redaction::redact_display(&hash),
                    action = "start",
                    result = "error",
                    error = %crate::url_redaction::redact_display(&error),
                    "TorrentNG client start succeeded but cache projection failed"
                );
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            emit_torrent_updated(&s, &hash).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "start", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "TorrentNG client start failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
pub async fn torrent_stop(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.stop(&hash).await {
        Ok(_) => {
            if let Err(error) = update_cached_lifecycle_state(&s, &hash, "stop").await {
                tracing::error!(
                    component = "cache",
                    operation = "set_torrent_runtime_state",
                    torrent = %crate::url_redaction::redact_display(&hash),
                    action = "stop",
                    result = "error",
                    error = %crate::url_redaction::redact_display(&error),
                    "TorrentNG client stop succeeded but cache projection failed"
                );
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            emit_torrent_updated(&s, &hash).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "stop", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "TorrentNG client stop failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
pub async fn torrent_recheck(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.recheck(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "recheck", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "TorrentNG client recheck failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
pub async fn torrent_reannounce(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.reannounce(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "reannounce", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "TorrentNG client reannounce failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Trackers ---

pub async fn torrent_trackers(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.list_trackers(&hash).await {
        Ok(trackers) => Json(serde_json::json!({ "trackers": trackers })).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_trackers", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "TorrentNG client tracker listing failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct TrackerEditItem {
    #[serde(deserialize_with = "deserialize_tracker_url")]
    pub orig_url: String,
    #[serde(deserialize_with = "deserialize_tracker_url")]
    pub new_url: String,
}

fn deserialize_tracker_url<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedJsonString::<MAX_API_TRACKER_URL_BYTES>::deserialize(deserializer).map(|value| value.0)
}

fn deserialize_tracker_list<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_strings::<D, MAX_API_TRACKER_MUTATIONS, MAX_API_TRACKER_URL_BYTES>(
        deserializer,
    )
}

fn deserialize_tracker_edits<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<TrackerEditItem>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<D, TrackerEditItem, MAX_API_TRACKER_MUTATIONS>(deserializer)
}

#[derive(Deserialize)]
pub struct PatchTrackersBody {
    #[serde(default, deserialize_with = "deserialize_tracker_list")]
    pub add: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_tracker_list")]
    pub remove: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_tracker_edits")]
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

    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };

    let mut failures = Vec::new();
    for url in add {
        if s.backend.add_tracker(&hash, url).await.is_err() {
            tracing::warn!(
                component = "api",
                operation = "add_tracker",
                torrent = %crate::url_redaction::redact_display(&hash),
                tracker = %redact_log_url(url),
                result = "error",
                "add tracker failed"
            );
            failures.push(tracker_failure_summary("add", url));
        }
    }
    for url in remove {
        if s.backend.remove_tracker(&hash, url).await.is_err() {
            tracing::warn!(
                component = "api",
                operation = "remove_tracker",
                torrent = %crate::url_redaction::redact_display(&hash),
                tracker = %redact_log_url(url),
                result = "error",
                "remove tracker failed"
            );
            failures.push(tracker_failure_summary("remove", url));
        }
    }
    for (orig_url, new_url) in edit {
        if s.backend
            .edit_tracker(&hash, orig_url, new_url)
            .await
            .is_err()
        {
            tracing::warn!(
                component = "api",
                operation = "edit_tracker",
                torrent = %crate::url_redaction::redact_display(&hash),
                tracker = %redact_log_url(orig_url),
                result = "error",
                "edit tracker failed"
            );
            failures.push(tracker_failure_summary("edit", orig_url));
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
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    match s.backend.list_files(&hash).await {
        Ok(files) => Json(serde_json::json!({ "files": files })).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_files", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "TorrentNG client file listing failed");
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
    #[serde(deserialize_with = "deserialize_file_priorities")]
    pub files: Vec<FilePriorityItem>,
}

fn deserialize_file_priorities<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<FilePriorityItem>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<D, FilePriorityItem, MAX_API_FILE_PRIORITY_UPDATES>(deserializer)
}

pub async fn set_file_priorities(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<SetFilePrioritiesBody>,
) -> impl IntoResponse {
    if body.files.is_empty() {
        return (StatusCode::BAD_REQUEST, "files must not be empty").into_response();
    }

    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };

    let mut failures = Vec::new();
    for item in &body.files {
        if let Err(e) = s
            .backend
            .set_file_priority(&hash, item.index, item.priority)
            .await
        {
            tracing::warn!(
                component = "api",
                operation = "set_file_priority",
                torrent = %crate::url_redaction::redact_display(&hash),
                file_index = item.index,
                priority = item.priority,
                result = "error",
                error = %crate::url_redaction::redact_display(&e),
                "TorrentNG client file priority update failed"
            );
            failures.push(format!("{}: {e}", item.index));
        }
    }
    if !failures.is_empty() {
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
    match s
        .db
        .run_blocking("list_categories", |db| db.list_categories())
        .await
    {
        Ok(cats) => Json(cats).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_categories", result = "error", error = %crate::url_redaction::redact_display(&e), "category listing failed");
            metadata_read_status(&e).into_response()
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
    if body.name.len() > MAX_API_CATEGORY_NAME_BYTES
        || body
            .save_path
            .as_deref()
            .is_some_and(|path| path.len() > MAX_API_CATEGORY_PATH_BYTES)
    {
        return (
            StatusCode::BAD_REQUEST,
            "category fields exceed input limits",
        )
            .into_response();
    }
    let name = body.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "category name must not be empty").into_response();
    }
    let save_path = body.save_path.as_deref().unwrap_or("");
    let category_name = name.to_owned();
    let category_path = save_path.to_owned();
    match s
        .db
        .run_blocking("upsert_category", move |db| {
            db.upsert_category(&category_name, &category_path)
        })
        .await
    {
        Ok(_) => {
            emit(&s, Event::CategoriesUpdated).await;
            Json(Category {
                name: name.to_owned(),
                save_path: save_path.to_owned(),
                torrent_count: 0,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "upsert_category", result = "error", category = %crate::url_redaction::redact_display(&name), error = %crate::url_redaction::redact_display(&e), "category upsert failed");
            metadata_write_status(&e).into_response()
        }
    }
}

pub async fn delete_category(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if name.len() > MAX_API_CATEGORY_NAME_BYTES {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let delete_name = name.clone();
    match s
        .db
        .run_blocking("delete_category", move |db| {
            db.delete_category(&delete_name)
        })
        .await
    {
        Ok(_) => {
            emit(&s, Event::CategoriesUpdated).await;
            emit(&s, Event::TrackerHealthUpdated).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_category", result = "error", category = %crate::url_redaction::redact_display(&name), error = %crate::url_redaction::redact_display(&e), "category delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Tags ---

pub async fn list_tags(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.run_blocking("list_tags", |db| db.list_tags()).await {
        Ok(tags) => Json(tags).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "list_tags", result = "error", error = %crate::url_redaction::redact_display(&e), "tag listing failed");
            metadata_read_status(&e).into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct TagBody {
    pub name: String,
}

pub async fn create_tag(State(s): State<AppState>, Json(body): Json<TagBody>) -> impl IntoResponse {
    if body.name.len() > MAX_API_TAG_BYTES {
        return (StatusCode::BAD_REQUEST, "tag name exceeds the input limit").into_response();
    }
    let name = body.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "tag name must not be empty").into_response();
    }
    let tag_name = name.to_owned();
    match s
        .db
        .run_blocking("ensure_tag", move |db| db.ensure_tag(&tag_name))
        .await
    {
        Ok(_) => {
            emit(&s, Event::TagsUpdated).await;
            StatusCode::CREATED.into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "create_tag", result = "error", tag = %crate::url_redaction::redact_display(&name), error = %crate::url_redaction::redact_display(&e), "tag create failed");
            metadata_write_status(&e).into_response()
        }
    }
}

pub async fn delete_tag(State(s): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    if name.len() > MAX_API_TAG_BYTES {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let delete_name = name.clone();
    match s
        .db
        .run_blocking("delete_tag", move |db| db.delete_tag(&delete_name))
        .await
    {
        Ok(_) => {
            emit(&s, Event::TagsUpdated).await;
            emit(&s, Event::TrackerHealthUpdated).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "delete_tag", result = "error", tag = %crate::url_redaction::redact_display(&name), error = %crate::url_redaction::redact_display(&e), "tag delete failed");
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
    if body.category.len() > MAX_API_CATEGORY_NAME_BYTES {
        return (StatusCode::BAD_REQUEST, "category exceeds the input limit").into_response();
    }
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    if !s.backend.capabilities().supports_categories {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }

    let category = body.category.trim();
    let capacity_category = category.to_owned();
    if let Err(error) =
        s.db.run_blocking("preflight_set_torrent_category", move |db| {
            crate::cache::categories::ensure_category_name_capacity(db, &capacity_category)
        })
        .await
    {
        tracing::warn!(component = "cache", operation = "preflight_set_category", result = "error", torrent = %crate::url_redaction::redact_display(&hash), category = %crate::url_redaction::redact_display(&category), error = %crate::url_redaction::redact_display(&error), "category update rejected before backend mutation");
        return metadata_write_status(&error).into_response();
    }
    if let Err(e) = s.backend.set_category(&hash, category).await {
        tracing::warn!(component = "backend", operation = "set_category", result = "error", torrent = %crate::url_redaction::redact_display(&hash), category = %crate::url_redaction::redact_display(&category), error = %crate::url_redaction::redact_display(&e), "backend category update failed");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let cache_hash = hash.clone();
    let cache_category = category.to_owned();
    if let Err(e) =
        s.db.run_blocking("set_torrent_category", move |db| {
            db.set_torrent_category(&cache_hash, &cache_category)
        })
        .await
    {
        tracing::error!(component = "cache", operation = "set_category", result = "error", torrent = %crate::url_redaction::redact_display(&hash), category = %crate::url_redaction::redact_display(&category), error = %crate::url_redaction::redact_display(&e), "cache category update failed after backend update");
        return metadata_write_status(&e).into_response();
    }
    emit_torrent_updated(&s, &hash).await;
    emit(&s, Event::CategoriesUpdated).await;
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct ModTagsBody {
    pub tags: BoundedTagList,
}

pub struct BoundedTagList {
    values: Vec<String>,
    valid: bool,
}

impl BoundedTagList {
    fn normalized(&self) -> Result<Vec<&str>, ()> {
        if !self.valid {
            return Err(());
        }
        bounded_normalized_tags(&self.values)
    }
}

impl<'de> Deserialize<'de> for BoundedTagList {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TagListVisitor;

        impl<'de> Visitor<'de> for TagListVisitor {
            type Value = BoundedTagList;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array of bounded tag strings")
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                let mut valid = true;
                for _ in 0..MAX_API_TAGS {
                    let Some(value) = sequence.next_element::<BoundedApiTag>()? else {
                        return Ok(BoundedTagList { values, valid });
                    };
                    match value.0 {
                        Some(value) => values.push(value),
                        None => valid = false,
                    }
                }
                if sequence.next_element::<IgnoredAny>()?.is_some() {
                    valid = false;
                    while sequence.next_element::<IgnoredAny>()?.is_some() {}
                }
                Ok(BoundedTagList { values, valid })
            }
        }

        struct BoundedApiTag(Option<String>);

        impl<'de> Deserialize<'de> for BoundedApiTag {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct TagVisitor;

                impl<'de> Visitor<'de> for TagVisitor {
                    type Value = BoundedApiTag;

                    fn expecting(
                        &self,
                        formatter: &mut std::fmt::Formatter<'_>,
                    ) -> std::fmt::Result {
                        formatter.write_str("a tag string")
                    }

                    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
                    where
                        E: de::Error,
                    {
                        Ok(BoundedApiTag(
                            (value.len() <= MAX_API_TAG_BYTES).then(|| value.to_owned()),
                        ))
                    }

                    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
                    where
                        E: de::Error,
                    {
                        Ok(BoundedApiTag(
                            (value.len() <= MAX_API_TAG_BYTES).then_some(value),
                        ))
                    }
                }

                deserializer.deserialize_str(TagVisitor)
            }
        }

        deserializer.deserialize_seq(TagListVisitor)
    }
}

struct BoundedJsonString<const MAX_BYTES: usize>(String);

impl<'de, const MAX_BYTES: usize> Deserialize<'de> for BoundedJsonString<MAX_BYTES> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StringVisitor<const MAX_BYTES: usize>;

        impl<'de, const MAX_BYTES: usize> Visitor<'de> for StringVisitor<MAX_BYTES> {
            type Value = BoundedJsonString<MAX_BYTES>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "a string of at most {MAX_BYTES} UTF-8 bytes")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_BYTES {
                    return Err(E::custom("string exceeds its byte limit"));
                }
                Ok(BoundedJsonString(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_BYTES {
                    return Err(E::custom("string exceeds its byte limit"));
                }
                Ok(BoundedJsonString(value))
            }
        }

        deserializer.deserialize_str(StringVisitor::<MAX_BYTES>)
    }
}

fn deserialize_bounded_vec<'de, D, T, const MAX_ITEMS: usize>(
    deserializer: D,
) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct VecVisitor<T, const MAX_ITEMS: usize>(std::marker::PhantomData<T>);

    impl<'de, T, const MAX_ITEMS: usize> Visitor<'de> for VecVisitor<T, MAX_ITEMS>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "an array with at most {MAX_ITEMS} entries")
        }

        fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            for _ in 0..MAX_ITEMS {
                let Some(value) = sequence.next_element::<T>()? else {
                    return Ok(values);
                };
                values.push(value);
            }
            if sequence.next_element::<IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("array exceeds its item limit"));
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(VecVisitor::<T, MAX_ITEMS>(std::marker::PhantomData))
}

fn deserialize_bounded_strings<'de, D, const MAX_ITEMS: usize, const MAX_BYTES: usize>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<D, BoundedJsonString<MAX_BYTES>, MAX_ITEMS>(deserializer)
        .map(|values| values.into_iter().map(|value| value.0).collect())
}

pub async fn add_torrent_tags(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<ModTagsBody>,
) -> impl IntoResponse {
    let tags = match body.tags.normalized() {
        Ok(tags) => tags,
        Err(()) => {
            return (
                StatusCode::BAD_REQUEST,
                "tags must be non-empty and within count/name limits",
            )
                .into_response()
        }
    };
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    if !s.backend.capabilities().supports_tags {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }

    let preflight_hash = hash.clone();
    let preflight_tags = tags.iter().map(|tag| (*tag).to_owned()).collect::<Vec<_>>();
    if let Err(error) =
        s.db.run_blocking("preflight_add_torrent_tags", move |db| {
            let tag_refs = preflight_tags
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            crate::cache::categories::ensure_torrent_tag_capacity(
                db,
                &preflight_hash,
                &tag_refs,
                true,
            )
        })
        .await
    {
        tracing::warn!(component = "cache", operation = "preflight_add_tags", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&error), "tag update rejected before backend mutation");
        return metadata_write_status(&error).into_response();
    }

    if let Err(e) = s.backend.add_tags(&hash, &tags).await {
        tracing::warn!(component = "api", operation = "add_tags", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "backend tag add failed");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let cache_hash = hash.clone();
    let cache_tags = tags.iter().map(|tag| (*tag).to_owned()).collect::<Vec<_>>();
    if let Err(e) =
        s.db.run_blocking("add_torrent_tags", move |db| {
            let tag_refs = cache_tags.iter().map(String::as_str).collect::<Vec<_>>();
            db.add_torrent_tags(&cache_hash, &tag_refs)
        })
        .await
    {
        tracing::error!(component = "cache", operation = "add_tags", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "cache tag add failed after backend update");
        return metadata_write_status(&e).into_response();
    }
    emit_torrent_updated(&s, &hash).await;
    emit(&s, Event::TagsUpdated).await;
    StatusCode::NO_CONTENT.into_response()
}

pub async fn remove_torrent_tags(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<ModTagsBody>,
) -> impl IntoResponse {
    let tags = match body.tags.normalized() {
        Ok(tags) => tags,
        Err(()) => {
            return (
                StatusCode::BAD_REQUEST,
                "tags must be non-empty and within count/name limits",
            )
                .into_response()
        }
    };
    let hash = match cached_torrent_hash(&s, &hash).await {
        Ok(hash) => hash,
        Err(status) => return status.into_response(),
    };
    if !s.backend.capabilities().supports_tags {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }

    if let Err(e) = s.backend.remove_tags(&hash, &tags).await {
        tracing::warn!(component = "api", operation = "remove_tags", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "backend tag removal failed");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let cache_hash = hash.clone();
    let cache_tags = tags.iter().map(|tag| (*tag).to_owned()).collect::<Vec<_>>();
    if let Err(e) =
        s.db.run_blocking("remove_torrent_tags", move |db| {
            let tag_refs = cache_tags.iter().map(String::as_str).collect::<Vec<_>>();
            db.remove_torrent_tags(&cache_hash, &tag_refs)
        })
        .await
    {
        tracing::error!(component = "cache", operation = "remove_tags", result = "error", torrent = %crate::url_redaction::redact_display(&hash), error = %crate::url_redaction::redact_display(&e), "cache tag removal failed after backend update");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    emit_torrent_updated(&s, &hash).await;
    emit(&s, Event::TagsUpdated).await;
    StatusCode::NO_CONTENT.into_response()
}

fn bounded_normalized_tags(tags: &[String]) -> Result<Vec<&str>, ()> {
    if tags.len() > MAX_API_TAGS || tags.iter().any(|tag| tag.len() > MAX_API_TAG_BYTES) {
        return Err(());
    }
    let normalized = normalized_nonempty(tags);
    if normalized.is_empty() {
        Err(())
    } else {
        Ok(normalized)
    }
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
    pub hashes: BoundedBulkHashes,
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

pub const MAX_BULK_TORRENTS: usize = 10_000;
const MAX_BULK_HASH_CHARS: usize = 256;
const MAX_BULK_CATEGORY_BYTES: usize = 256;
const MAX_BULK_SAVE_PATH_BYTES: usize = 4_096;

pub struct BoundedBulkHashes {
    values: Vec<String>,
    too_many: bool,
    invalid_hash: bool,
}

struct BoundedBulkHash(Option<String>);

impl<'de> Deserialize<'de> for BoundedBulkHash {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HashVisitor;

        impl<'de> Visitor<'de> for HashVisitor {
            type Value = BoundedBulkHash;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded torrent hash string")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(BoundedBulkHash(bounded_bulk_hash(value)))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                let bounded = if value.len() > MAX_BULK_HASH_CHARS * 4
                    || value.chars().count() > MAX_BULK_HASH_CHARS
                {
                    None
                } else {
                    Some(value)
                };
                Ok(BoundedBulkHash(bounded))
            }
        }

        deserializer.deserialize_str(HashVisitor)
    }
}

fn bounded_bulk_hash(value: &str) -> Option<String> {
    if value.len() > MAX_BULK_HASH_CHARS * 4 || value.chars().count() > MAX_BULK_HASH_CHARS {
        None
    } else {
        Some(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for BoundedBulkHashes {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HashesVisitor;

        impl<'de> Visitor<'de> for HashesVisitor {
            type Value = BoundedBulkHashes;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    formatter,
                    "an array with at most {MAX_BULK_TORRENTS} bounded torrent hashes"
                )
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                let mut invalid_hash = false;
                for _ in 0..MAX_BULK_TORRENTS {
                    let Some(hash) = sequence.next_element::<BoundedBulkHash>()? else {
                        return Ok(BoundedBulkHashes {
                            values,
                            too_many: false,
                            invalid_hash,
                        });
                    };
                    if let Some(hash) = hash.0 {
                        values.push(hash);
                    } else {
                        invalid_hash = true;
                    }
                }
                let too_many = sequence.next_element::<IgnoredAny>()?.is_some();
                if too_many {
                    while sequence.next_element::<IgnoredAny>()?.is_some() {}
                }
                Ok(BoundedBulkHashes {
                    values,
                    too_many,
                    invalid_hash,
                })
            }
        }

        deserializer.deserialize_seq(HashesVisitor)
    }
}

fn deduplicate_hashes(hashes: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    hashes
        .into_iter()
        .filter(|hash| seen.insert(hash.to_ascii_lowercase()))
        .collect()
}

async fn cached_torrent_hash(s: &AppState, requested: &str) -> Result<String, StatusCode> {
    let lookup = requested.to_owned();
    match s
        .db
        .run_blocking("canonicalize_torrent_hash", move |db| {
            db.canonical_hash(&lookup)
        })
        .await
    {
        Ok(Some(hash)) => Ok(hash),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::error!(
                component = "cache",
                operation = "canonicalize_torrent_hash",
                torrent = %crate::url_redaction::redact_display(&requested),
                result = "error",
                error = %crate::url_redaction::redact_display(&error),
                "failed to resolve torrent hash against the cache"
            );
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn cached_torrent_hashes(
    s: &AppState,
    requested: Vec<String>,
) -> Result<(Vec<String>, Vec<String>), StatusCode> {
    let result =
        s.db.run_blocking("canonicalize_bulk_torrent_hashes", move |db| {
            let mut found = Vec::with_capacity(requested.len());
            let mut missing = Vec::new();
            for hash in requested {
                match db.canonical_hash(&hash)? {
                    Some(canonical) => found.push(canonical),
                    None => missing.push(hash),
                }
            }
            Ok((deduplicate_hashes(found), missing))
        })
        .await;
    match result {
        Ok(hashes) => Ok(hashes),
        Err(error) => {
            tracing::error!(
                component = "cache",
                operation = "canonicalize_bulk_torrent_hashes",
                result = "error",
                error = %crate::url_redaction::redact_display(&error),
                "failed to resolve bulk torrent hashes against the cache"
            );
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn spawn_bulk_action(
    set: &mut tokio::task::JoinSet<(String, anyhow::Result<()>)>,
    s: &AppState,
    hash: String,
    action: &str,
    category: Option<&str>,
    save_path: Option<&str>,
) {
    let state = s.clone();
    let action = action.to_owned();
    let category = category.map(str::to_owned);
    let save_path = save_path.map(str::to_owned);
    set.spawn(async move {
        let res: anyhow::Result<()> = match action.as_str() {
            "start" => state.backend.start(&hash).await,
            "stop" => state.backend.stop(&hash).await,
            "recheck" => state.backend.recheck(&hash).await,
            "reannounce" => state.backend.reannounce(&hash).await,
            "set-category" => {
                let category = category.as_deref().expect("category was validated");
                let lookup_hash = hash.clone();
                match state
                    .db
                    .run_blocking("bulk_set_category_exists", move |db| {
                        db.exists(&lookup_hash)
                    })
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => return (hash, Err(anyhow::anyhow!("not found"))),
                    Err(e) => return (hash, Err(e)),
                }
                match state.backend.set_category(&hash, category).await {
                    Ok(()) => {
                        let cache_hash = hash.clone();
                        let cache_category = category.to_owned();
                        state
                            .db
                            .run_blocking("bulk_set_category_cache", move |db| {
                                db.set_torrent_category(&cache_hash, &cache_category)
                            })
                            .await
                            .map_err(|error| anyhow::anyhow!(error))
                    }
                    Err(e) => Err(e),
                }
            }
            "set-location" => {
                let save_path = save_path.as_deref().expect("save_path was validated");
                let lookup_hash = hash.clone();
                match state
                    .db
                    .run_blocking("bulk_set_location_exists", move |db| {
                        db.exists(&lookup_hash)
                    })
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => return (hash, Err(anyhow::anyhow!("not found"))),
                    Err(e) => return (hash, Err(e)),
                }
                match state.backend.set_location(&hash, save_path).await {
                    Ok(()) => {
                        let cache_hash = hash.clone();
                        let cache_path = save_path.to_owned();
                        if let Err(e) = state
                            .db
                            .run_blocking("bulk_set_location_cache", move |db| {
                                db.set_torrent_location(&cache_hash, &cache_path)
                            })
                            .await
                        {
                            return (hash, Err(e));
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
                if let Err(error) = update_cached_lifecycle_state(&state, &hash, &action).await {
                    (
                        hash,
                        Err(anyhow::anyhow!("cache projection failed: {error}")),
                    )
                } else {
                    emit_torrent_updated(&state, &hash).await;
                    if action == "set-category" {
                        emit(&state, Event::CategoriesUpdated).await;
                    }
                    (hash, Ok(()))
                }
            }
            Err(e) => (hash, Err(e)),
        }
    });
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
    if category.is_some_and(|value| value.len() > MAX_BULK_CATEGORY_BYTES) {
        return (
            StatusCode::BAD_REQUEST,
            format!("category must be at most {MAX_BULK_CATEGORY_BYTES} UTF-8 bytes"),
        )
            .into_response();
    }
    if save_path.is_some_and(|value| value.len() > MAX_BULK_SAVE_PATH_BYTES) {
        return (
            StatusCode::BAD_REQUEST,
            format!("save_path must be at most {MAX_BULK_SAVE_PATH_BYTES} UTF-8 bytes"),
        )
            .into_response();
    }

    if body.hashes.too_many {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("hashes must contain at most {MAX_BULK_TORRENTS} entries"),
        )
            .into_response();
    }
    if body.hashes.invalid_hash {
        return (
            StatusCode::BAD_REQUEST,
            format!("each hash must be at most {MAX_BULK_HASH_CHARS} characters"),
        )
            .into_response();
    }

    // Treat a repeated hash as one mutation target. The WebUI normally
    // supplies a Set, but qBittorrent/automation clients can submit the same
    // hash repeatedly; forwarding that list would invoke start/reannounce
    // multiple times for the same torrent.
    let requested_hashes = deduplicate_hashes(body.hashes.values);
    let (hashes, missing_hashes) = match cached_torrent_hashes(&s, requested_hashes).await {
        Ok(hashes) => hashes,
        Err(status) => return status.into_response(),
    };
    let missing_errors = missing_hashes
        .into_iter()
        .map(|hash| format!("{hash}: not found"))
        .collect::<Vec<_>>();

    if body.dry_run {
        return Json(BulkResult {
            applied: hashes,
            errors: missing_errors,
            dry_run: true,
        })
        .into_response();
    }

    if hashes.is_empty() {
        return Json(BulkResult {
            applied: Vec::new(),
            errors: missing_errors,
            dry_run: false,
        })
        .into_response();
    }

    if action == "set-category" && !s.backend.capabilities().supports_categories {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    if action == "set-location" && !s.backend.capabilities().supports_location_update {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    if action == "set-category" {
        let category_name = category
            .expect("set-category requires a category")
            .to_owned();
        if let Err(error) =
            s.db.run_blocking("preflight_bulk_set_category", move |db| {
                crate::cache::categories::ensure_category_name_capacity(db, &category_name)
            })
            .await
        {
            tracing::warn!(component = "cache", operation = "preflight_bulk_set_category", result = "error", error = %crate::url_redaction::redact_display(&error), "bulk category update rejected before backend mutation");
            return metadata_write_status(&error).into_response();
        }
    }

    // Prefer a backend's bulk-optimized path (e.g. rTorrent's
    // system.multicall, one round trip for the whole set) when it has one.
    // None means the backend has no such path -- fall through to the
    // per-hash concurrent loop below, which every backend supports.
    if action == "stop" || action == "recheck" {
        let fast = if action == "stop" {
            s.backend.stop_many(&hashes).await
        } else {
            s.backend.recheck_many(&hashes).await
        };
        if let Some(outcome) = fast {
            let mut applied = Vec::new();
            let mut errors = missing_errors.clone();
            match outcome {
                Ok(results) => {
                    // Batch the cache write into one transaction instead of
                    // one autocommit per hash -- with the RPC round trips no
                    // longer the bottleneck, thousands of individual fsyncs
                    // here would just become the new one.
                    let mut state_updates = Vec::new();
                    for (hash, res) in &results {
                        match res {
                            Ok(()) => {
                                if action == "stop" {
                                    state_updates.push((hash.clone(), 0i64, false, false));
                                }
                                applied.push(hash.clone());
                            }
                            Err(e) => errors.push(format!("{hash}: {e}")),
                        }
                    }
                    let state_updates_for_db = state_updates.clone();
                    if let Err(e) =
                        s.db.run_blocking("set_torrent_runtime_state_many", move |db| {
                            db.set_torrent_runtime_state_many(&state_updates_for_db)
                        })
                        .await
                    {
                        tracing::warn!(
                            component = "cache",
                            operation = "set_torrent_runtime_state_many",
                            count = state_updates.len(),
                            result = "error",
                            error = %crate::url_redaction::redact_display(&e),
                            "bulk torrent runtime cache update failed"
                        );
                        for (hash, _, _, _) in &state_updates {
                            applied.retain(|applied_hash| applied_hash != hash);
                            errors.push(format!("{hash}: cache projection failed: {e}"));
                        }
                    }
                    for hash in &applied {
                        emit_torrent_updated(&s, hash).await;
                    }
                }
                Err(e) => {
                    errors.extend(hashes.iter().map(|hash| format!("{hash}: {e}")));
                }
            }
            return Json(BulkResult {
                applied,
                errors,
                dry_run: false,
            })
            .into_response();
        }
    }

    // Keep both active backend calls and allocated task state bounded. A
    // semaphore limits active calls but still leaves one waiting task per
    // selected torrent, so feed the JoinSet from a fixed-size window.
    const BULK_CONCURRENCY: usize = 32;
    let mut set = tokio::task::JoinSet::new();
    let mut pending_hashes = hashes.into_iter();
    for _ in 0..BULK_CONCURRENCY {
        let Some(hash) = pending_hashes.next() else {
            break;
        };
        spawn_bulk_action(&mut set, &s, hash, &action, category, save_path);
    }

    let mut applied = Vec::new();
    let mut errors = missing_errors;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((hash, Ok(()))) => applied.push(hash),
            Ok((hash, Err(e))) => errors.push(format!("{hash}: {e}")),
            Err(join_err) => errors.push(format!("task panicked: {join_err}")),
        }
        if let Some(hash) = pending_hashes.next() {
            spawn_bulk_action(&mut set, &s, hash, &action, category, save_path);
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
    if !s.backend.capabilities().supports_runtime_user_agent {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    match s.backend.get_user_agent().await {
        Ok(ua) => Json(UserAgentResponse { user_agent: ua }).into_response(),
        Err(e) => {
            tracing::error!(component = "api", operation = "get_user_agent", result = "error", error = %crate::url_redaction::redact_display(&e), "user-agent lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn set_user_agent(
    State(s): State<AppState>,
    Json(body): Json<SetUserAgentBody>,
) -> impl IntoResponse {
    if !s.backend.capabilities().supports_runtime_user_agent {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    let ua = body.user_agent.trim().to_owned();
    if ua.is_empty() {
        return (StatusCode::BAD_REQUEST, "user_agent must not be empty").into_response();
    }
    match s.backend.set_user_agent(&ua).await {
        Ok(_) => {
            tracing::info!(
                component = s.backend.backend_type().as_str(),
                operation = "set_user_agent",
                user_agent_len = ua.len(),
                "user agent updated"
            );
            record_operator_event(
                &s,
                "settings_changed",
                "backend user agent updated",
                serde_json::json!({
                    "component": s.backend.backend_type().as_str(),
                    "operation": "set_user_agent",
                    "result": "updated",
                    "user_agent_len": ua.len(),
                }),
                "info",
            )
            .await;
            Json(UserAgentResponse { user_agent: ua }).into_response()
        }
        Err(e) => {
            tracing::error!(component = "api", operation = "set_user_agent", result = "error", error = %crate::url_redaction::redact_display(&e), "user-agent update failed");
            record_operator_event(
                &s,
                "rtorrent_user_agent_error",
                "backend user agent update failed",
                serde_json::json!({
                    "component": s.backend.backend_type().as_str(),
                    "operation": "set_user_agent",
                    "result": "error",
                    "error": e.to_string(),
                }),
                "warn",
            )
            .await;
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn redact_log_url(value: &str) -> String {
    crate::url_redaction::redact_log_url(value)
}

fn tracker_failure_summary(action: &str, url: &str) -> String {
    format!("{action} {}: backend operation failed", redact_log_url(url))
}

fn rss_magnet_failure_summary(rule_name: &str) -> String {
    format!("{rule_name}: magnet add failed")
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::{
        automation_state_write_status, escape_script_log_output, live_row_response,
        merge_rtorrent_overlay, metadata_read_status, metadata_write_status,
        parse_torrent_live_hashes, read_script_output, rss_magnet_failure_summary,
        rtorrent_overlay_is_writable, rtorrent_settings, statvfs_block_size,
        tracker_failure_summary, write_rtorrent_overlay, ApplyRssRuleBody, TestRssRuleBody,
        MAX_RSS_RULE_FIELD_BYTES, MAX_TORRENT_LIVE_HASH_QUERY_BYTES, MAX_TORRENT_LIVE_STATS,
    };
    use crate::cache::TorrentLiveRow;
    use std::collections::BTreeMap;
    use tokio::io::{duplex, AsyncWriteExt};

    #[test]
    fn tracker_failure_summaries_do_not_echo_tracker_credentials() {
        let summary = tracker_failure_summary(
            "remove",
            "https://user:password@tracker.example/announce/path-secret?passkey=query-secret#fragment-secret",
        );

        assert_eq!(
            summary,
            "remove https://tracker.example/: backend operation failed"
        );
        for secret in [
            "user",
            "password",
            "path-secret",
            "query-secret",
            "fragment-secret",
        ] {
            assert!(!summary.contains(secret), "{summary}");
        }
    }

    #[test]
    fn rss_magnet_failure_summary_does_not_echo_backend_error_or_magnet() {
        let summary = rss_magnet_failure_summary("Linux ISO");
        assert_eq!(summary, "Linux ISO: magnet add failed");
    }

    #[test]
    fn metadata_capacity_errors_map_to_bounded_service_statuses() {
        let error = anyhow::Error::new(crate::cache::categories::CategoryTagCapacityError::new(
            "category",
        ));
        assert_eq!(metadata_write_status(&error), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            metadata_read_status(&error),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn automation_state_capacity_errors_map_to_payload_too_large() {
        let error = anyhow::Error::new(crate::cache::AutomationStateCapacityError);
        assert_eq!(
            automation_state_write_status(&error),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[test]
    fn workflow_script_log_output_escapes_record_and_terminal_controls() {
        let output = escape_script_log_output(
            "ok café\nforged\r\n\u{1b}[31m\u{85}\u{2028}\u{2029}\u{202e}\u{2066}".as_bytes(),
        );

        assert_eq!(
            output,
            "ok café\\nforged\\r\\n\\u{1b}[31m\\u{85}\\u{2028}\\u{2029}\\u{202e}\\u{2066}"
        );
    }

    #[tokio::test]
    async fn workflow_script_output_is_bounded() {
        const MAX_OUTPUT: u64 = 64 * 1024;
        let (mut writer, reader) = duplex(1024);
        let writer_task =
            tokio::spawn(
                async move { writer.write_all(&vec![b'x'; MAX_OUTPUT as usize + 1]).await },
            );

        let error = read_script_output(reader, MAX_OUTPUT)
            .await
            .expect_err("output beyond the cap must fail");
        assert!(error.contains("exceeds"));
        writer_task
            .await
            .expect("writer task should not panic")
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workflow_script_output_overflow_kills_blocked_child_immediately() {
        use std::{
            process::Stdio,
            time::{Duration, Instant},
        };

        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "printf '0123456789'; exec sleep 30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn test script");
        let stdout = child.stdout.take().expect("script stdout");
        let stderr = child.stderr.take().expect("script stderr");
        let start = Instant::now();

        let error =
            super::wait_for_script_output(&mut child, stdout, stderr, 4, Duration::from_secs(10))
                .await
                .expect_err("excess output should fail");

        assert!(
            error.contains("exceeds 4 bytes"),
            "unexpected error: {error}"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(child.try_wait().expect("child status").is_some());
    }

    #[test]
    fn rtorrent_settings_patch_preserves_omitted_values_and_custom_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rtorrent.rc");
        let descriptors = rtorrent_settings();
        let mut initial = BTreeMap::new();
        initial.insert("max_uploads".to_owned(), "77".to_owned());
        write_rtorrent_overlay(&path, &descriptors, &initial, "custom.setting = yes").unwrap();

        let mut patch = BTreeMap::new();
        patch.insert("max_downloads".to_owned(), "88".to_owned());
        let (merged, custom) = merge_rtorrent_overlay(&path, &patch, None).unwrap();
        assert_eq!(merged.get("max_uploads"), Some(&"77".to_owned()));
        assert_eq!(merged.get("max_downloads"), Some(&"88".to_owned()));
        assert_eq!(custom, "custom.setting = yes");
    }

    #[test]
    fn rtorrent_overlay_writability_checks_the_actual_directory_and_cleans_probe() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rtorrent.rc");
        std::fs::write(&path, "keep this config").unwrap();

        assert!(rtorrent_overlay_is_writable(&path));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep this config");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        assert!(!rtorrent_overlay_is_writable(
            &directory.path().join("missing/rtorrent.rc")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rtorrent_overlay_writability_rejects_a_read_only_parent() {
        use std::os::unix::fs::PermissionsExt;

        // Root can create files despite mode bits, so this permission test is
        // meaningful only when the process is subject to ordinary DAC checks.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let original = std::fs::metadata(directory.path()).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_mode(0o500);
        std::fs::set_permissions(directory.path(), readonly).unwrap();
        let writable = rtorrent_overlay_is_writable(&directory.path().join("rtorrent.rc"));
        std::fs::set_permissions(directory.path(), original).unwrap();

        assert!(!writable, "existing read-only parent is not writable");
    }

    #[cfg(unix)]
    #[test]
    fn rtorrent_overlay_replacement_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rtorrent.rc");
        write_rtorrent_overlay(
            &path,
            &rtorrent_settings(),
            &BTreeMap::new(),
            "token = secret",
        )
        .unwrap();

        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn live_stats_zero_expired_rates() {
        let row = TorrentLiveRow {
            hash: "stale".to_owned(),
            size_bytes: 100,
            bytes_done: 25,
            down_rate: 1_000,
            up_rate: 2_000,
            updated_at: 1,
        };
        let response = live_row_response(row, 1_700_000_000_000);
        assert_eq!(response.download_rate, 0);
        assert_eq!(response.upload_rate, 0);
        assert_eq!(response.amount_left, 75);
    }

    #[test]
    fn statvfs_uses_fragment_size_and_only_falls_back_when_zero() {
        assert_eq!(statvfs_block_size(4_096, 8_192), 4_096);
        assert_eq!(statvfs_block_size(8_192, 4_096), 8_192);
        assert_eq!(statvfs_block_size(0, 8_192), 8_192);
    }

    #[test]
    fn live_stats_bounds_raw_query_and_counts_duplicates_before_deduplication() {
        let hash = "a".repeat(40);
        let within_limit = std::iter::repeat_n(hash.as_str(), MAX_TORRENT_LIVE_STATS)
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            parse_torrent_live_hashes(Some(&within_limit))
                .expect("the bounded duplicate list is accepted"),
            vec![hash.clone()]
        );

        let too_many = std::iter::repeat_n(hash.as_str(), MAX_TORRENT_LIVE_STATS + 1)
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse_torrent_live_hashes(Some(&too_many))
            .expect_err("duplicates count toward the request limit")
            .contains("at most"));

        let oversized_empty_segments = ",".repeat(MAX_TORRENT_LIVE_HASH_QUERY_BYTES + 1);
        assert!(parse_torrent_live_hashes(Some(&oversized_empty_segments))
            .expect_err("empty segments must not bypass the raw query cap")
            .contains("bytes"));
    }

    #[test]
    fn rss_sample_fields_are_bounded_during_json_deserialization() {
        let within_limit = "x".repeat(MAX_RSS_RULE_FIELD_BYTES);
        let title_only: TestRssRuleBody = serde_json::from_value(serde_json::json!({
            "title": within_limit,
        }))
        .expect("an absent optional link and an exact-limit title are accepted");
        assert_eq!(title_only.link, None);

        let oversized = "x".repeat(MAX_RSS_RULE_FIELD_BYTES + 1);
        assert!(
            serde_json::from_value::<TestRssRuleBody>(serde_json::json!({
                "title": oversized,
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<TestRssRuleBody>(serde_json::json!({
                "title": "valid",
                "link": oversized,
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ApplyRssRuleBody>(serde_json::json!({
                "title": "valid",
                "link": oversized,
            }))
            .is_err()
        );
    }

    #[test]
    fn live_stats_query_string_deserialization_enforces_its_byte_limit() {
        assert!(serde_json::from_str::<super::BoundedQueryString<3>>("\"abc\"").is_ok());
        assert!(serde_json::from_str::<super::BoundedQueryString<3>>("\"abcd\"").is_err());
    }
}
