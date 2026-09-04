use axum::{
    extract::{Multipart, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use rt_api_model::ChunkedVec;
use rt_metainfo::parse_magnet;
use serde::{
    ser::{SerializeMap, Serializer},
    Deserialize, Serialize,
};
use std::{
    collections::{hash_map::DefaultHasher, BTreeMap, BTreeSet, HashMap, HashSet},
    hash::{Hash, Hasher},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinSet;
use url::Url;

use rt_engine::{
    EngineGlobalLimits, EnginePeerSnapshot, EnginePieceState, EngineTorrentFile,
    EngineTorrentLimits, EngineTorrentMetadata, EngineTrackerSnapshot, OutboundTargetKind,
    QueueMove,
};
use rt_metrics::{MemoryClass, MemoryLease};

use crate::{
    model::{
        to_qbit_state, QbCategoryInfo, QbFileInfo, QbServerState, QbTorrentInfo,
        QbTorrentProperties, QbTrackerInfo,
    },
    state::{canonical_sort_key, AppState, JsonMap, TorrentSnapshotError},
};

// Live qBittorrent compatibility fields are backed by per-torrent engine
// commands. Keep those projections for interactive pages, but do not turn a
// full-list response into one actor round-trip per dormant torrent.
const QBIT_LIVE_PROJECTION_MAX_ENTRIES: usize = 200;
/// qBittorrent leaves `limit` optional, but returning an unbounded response
/// makes one legacy request capable of allocating and projecting the entire
/// session.  Clients that need more data can use explicit offset/limit pages
/// and the TorrentNG snapshot header.
const QBIT_DEFAULT_PAGE_SIZE: usize = 200;
const MAX_TORRENT_LIST_OFFSET: usize = 1_000_000;
const QBIT_LIST_INITIAL_CAPACITY: usize = 256;
const QBIT_LIMIT_PROJECTION_CONCURRENCY: usize = 64;
const QBIT_LIVE_PROJECTION_CONCURRENCY: usize = 64;

// These compatibility settings are deliberately separate from the engine's
// runtime settings.  They are qBittorrent WebUI state, not native transport
// configuration, but they still need to survive a daemon restart when the
// native engine is attached.
const SETTING_QBIT_PREFERENCES: &str = "qbit.preferences";
const SETTING_QBIT_COOKIES: &str = "qbit.cookies";
const SETTING_QBIT_API_KEY: &str = "qbit.api_key";
const SETTING_QBIT_RSS_ITEMS: &str = "qbit.rss_items";
const SETTING_QBIT_RSS_RULES: &str = "qbit.rss_rules";
const SETTING_QBIT_SEARCH_PLUGINS: &str = "qbit.search_plugins";
const MAX_QBIT_PREFERENCE_BYTES: usize = 1024 * 1024;
const MAX_QBIT_PREFERENCE_KEYS: usize = 512;
const MAX_QBIT_COOKIE_COUNT: usize = 4096;
const MAX_QBIT_RSS_ITEMS: usize = 4096;
const MAX_QBIT_RSS_RULES: usize = 1024;
const MAX_QBIT_SEARCH_PLUGINS: usize = 256;

async fn category_definitions(state: &AppState) -> Result<BTreeMap<String, String>, String> {
    if let Some(engine) = &state.engine {
        return engine.list_categories().await.map(|categories| {
            categories
                .into_iter()
                .map(|category| (category.name, category.save_path.unwrap_or_default()))
                .collect()
        });
    }
    Ok(state.categories.read().await.clone())
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

/// `POST /api/qb/v2/auth/login` — qBittorrent-compatible session probe.
pub async fn auth_login(State(state): State<AppState>, body: String) -> Response {
    let submitted = qbit_form_token(&body);
    let sid = if state.api_tokens.is_empty() {
        "torrentng".to_owned()
    } else {
        let Some(token) = submitted.filter(|token| token_allowed(&state, token)) else {
            return (
                StatusCode::FORBIDDEN,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "Forbidden.",
            )
                .into_response();
        };
        token
    };
    let cookie = format!(
        "SID={}; Max-Age=86400; HttpOnly; SameSite=Lax; Path=/",
        cookie_component_encode(&sid)
    );
    (
        StatusCode::OK,
        [(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap())],
        "Ok.",
    )
        .into_response()
}

fn token_allowed(state: &AppState, token: &str) -> bool {
    state.api_tokens.iter().any(|allowed| allowed == token)
}

fn qbit_form_token(body: &str) -> Option<String> {
    let mut username = None;
    let mut password = None;
    for pair in body.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let Some(key) = form_component_decode(key) else {
            continue;
        };
        if key != "password" && key != "username" {
            continue;
        }
        let Some(value) = form_component_decode(value) else {
            continue;
        };
        if key == "password" {
            password = Some(value);
        } else {
            username = Some(value);
        }
    }
    password.or(username)
}

fn form_component_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                out.push((high << 4) | low);
                index += 2;
            }
            b'%' => return None,
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn cookie_component_encode(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'0'..=b'9'
            | b'a'..=b'z'
            | b'A'..=b'Z'
            | b'!'
            | b'#'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'/'
            | b':'
            | b'<'
            | b'='
            | b'>'
            | b'?'
            | b'@'
            | b'['
            | b']'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~' => output.push(byte as char),
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

pub async fn auth_logout() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            HeaderValue::from_static("SID=; Max-Age=0; HttpOnly; SameSite=Lax; Path=/"),
        )],
    )
}

// ---------------------------------------------------------------------------
// App info
// ---------------------------------------------------------------------------

pub async fn app_version() -> impl IntoResponse {
    (StatusCode::OK, "v5.0.0")
}

pub async fn app_webapi_version() -> impl IntoResponse {
    (StatusCode::OK, "2.9.3")
}

pub async fn app_build_info() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "qt": "6.7.0",
            "libtorrent": "TorrentNG-native",
            "boost": "",
            "openssl": "",
            "bitness": 64,
        })),
    )
}

fn encode_qbit_json<T: Serialize>(value: &T, label: &str) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| format!("encoding {label}: {error}"))?;
    if bytes.len() > MAX_QBIT_PREFERENCE_BYTES {
        return Err(format!(
            "{label} exceeds the {} byte compatibility limit",
            MAX_QBIT_PREFERENCE_BYTES
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("encoding {label} as UTF-8: {error}"))
}

fn decode_qbit_preferences(raw: &str) -> Result<JsonMap, String> {
    decode_qbit_map(
        raw,
        "durable qBittorrent preferences",
        MAX_QBIT_PREFERENCE_KEYS,
    )
}

fn validate_qbit_preferences(preferences: &JsonMap) -> Result<(), String> {
    validate_qbit_map(
        preferences,
        "qBittorrent preferences",
        MAX_QBIT_PREFERENCE_KEYS,
    )
}

fn decode_qbit_map(raw: &str, label: &str, max_entries: usize) -> Result<JsonMap, String> {
    if raw.len() > MAX_QBIT_PREFERENCE_BYTES {
        return Err(format!(
            "{label} exceed the {MAX_QBIT_PREFERENCE_BYTES} byte compatibility limit"
        ));
    }
    let values = serde_json::from_str::<JsonMap>(raw)
        .map_err(|error| format!("decoding {label}: {error}"))?;
    validate_qbit_map(&values, label, max_entries)?;
    Ok(values)
}

fn validate_qbit_map(values: &JsonMap, label: &str, max_entries: usize) -> Result<(), String> {
    if values.len() > max_entries {
        return Err(format!("{label} contain more than {max_entries} entries"));
    }
    if values.keys().any(|key| key.len() > 256) {
        return Err(format!("{label} contain a key that is too long"));
    }
    // Serialize before accepting an update so deeply nested or otherwise
    // pathological JSON cannot be retained in the settings table.
    let _ = encode_qbit_json(values, label)?;
    Ok(())
}

async fn load_qbit_preferences(state: &AppState) -> Result<JsonMap, String> {
    if let Some(engine) = &state.engine {
        match engine
            .get_setting(SETTING_QBIT_PREFERENCES.to_owned())
            .await?
        {
            Some(raw) => decode_qbit_preferences(&raw),
            None => Ok(state.preference_overrides.read().await.clone()),
        }
    } else {
        Ok(state.preference_overrides.read().await.clone())
    }
}

async fn save_qbit_preferences(state: &AppState, preferences: JsonMap) -> Result<(), String> {
    validate_qbit_preferences(&preferences)?;
    if let Some(engine) = &state.engine {
        let encoded = encode_qbit_json(&preferences, "qBittorrent preferences")?;
        engine
            .set_setting(SETTING_QBIT_PREFERENCES.to_owned(), encoded)
            .await?;
    }
    *state.preference_overrides.write().await = preferences;
    Ok(())
}

fn decode_qbit_cookies(raw: &str) -> Result<Vec<serde_json::Value>, String> {
    if raw.len() > MAX_QBIT_PREFERENCE_BYTES {
        return Err("durable qBittorrent cookies exceed the compatibility limit".to_owned());
    }
    let cookies = serde_json::from_str::<Vec<serde_json::Value>>(raw)
        .map_err(|error| format!("decoding durable qBittorrent cookies: {error}"))?;
    if cookies.len() > MAX_QBIT_COOKIE_COUNT || cookies.iter().any(|cookie| !cookie.is_object()) {
        return Err("durable qBittorrent cookies contain an invalid entry or count".to_owned());
    }
    Ok(cookies)
}

async fn load_qbit_cookies(state: &AppState) -> Result<Vec<serde_json::Value>, String> {
    if let Some(engine) = &state.engine {
        match engine.get_setting(SETTING_QBIT_COOKIES.to_owned()).await? {
            Some(raw) => decode_qbit_cookies(&raw),
            None => Ok(state.app_cookies.read().await.clone()),
        }
    } else {
        Ok(state.app_cookies.read().await.clone())
    }
}

async fn save_qbit_cookies(
    state: &AppState,
    cookies: Vec<serde_json::Value>,
) -> Result<(), String> {
    if cookies.len() > MAX_QBIT_COOKIE_COUNT || cookies.iter().any(|cookie| !cookie.is_object()) {
        return Err("qBittorrent cookies contain an invalid entry or count".to_owned());
    }
    let encoded = encode_qbit_json(&cookies, "qBittorrent cookies")?;
    if let Some(engine) = &state.engine {
        engine
            .set_setting(SETTING_QBIT_COOKIES.to_owned(), encoded)
            .await?;
    }
    *state.app_cookies.write().await = cookies;
    Ok(())
}

async fn save_qbit_api_key(state: &AppState, key: Option<String>) -> Result<(), String> {
    if let Some(engine) = &state.engine {
        let encoded = serde_json::to_string(&key)
            .map_err(|error| format!("encoding qBittorrent API key: {error}"))?;
        engine
            .set_setting(SETTING_QBIT_API_KEY.to_owned(), encoded)
            .await?;
    }
    *state.api_key.write().await = key;
    Ok(())
}

async fn load_qbit_rss_items(state: &AppState) -> Result<JsonMap, String> {
    if let Some(engine) = &state.engine {
        match engine
            .get_setting(SETTING_QBIT_RSS_ITEMS.to_owned())
            .await?
        {
            Some(raw) => decode_qbit_map(&raw, "durable qBittorrent RSS items", MAX_QBIT_RSS_ITEMS),
            None => Ok(state.rss_items.read().await.clone()),
        }
    } else {
        Ok(state.rss_items.read().await.clone())
    }
}

async fn save_qbit_rss_items(state: &AppState, items: JsonMap) -> Result<(), String> {
    validate_qbit_map(&items, "qBittorrent RSS items", MAX_QBIT_RSS_ITEMS)?;
    if let Some(engine) = &state.engine {
        let encoded = encode_qbit_json(&items, "qBittorrent RSS items")?;
        engine
            .set_setting(SETTING_QBIT_RSS_ITEMS.to_owned(), encoded)
            .await?;
    }
    *state.rss_items.write().await = items;
    Ok(())
}

async fn load_qbit_rss_rules(state: &AppState) -> Result<JsonMap, String> {
    if let Some(engine) = &state.engine {
        match engine
            .get_setting(SETTING_QBIT_RSS_RULES.to_owned())
            .await?
        {
            Some(raw) => {
                let rules =
                    decode_qbit_map(&raw, "durable qBittorrent RSS rules", MAX_QBIT_RSS_RULES)?;
                validate_qbit_rss_rules(&rules)?;
                Ok(rules)
            }
            None => {
                let rules = state.rss_rules.read().await.clone();
                validate_qbit_rss_rules(&rules)?;
                Ok(rules)
            }
        }
    } else {
        let rules = state.rss_rules.read().await.clone();
        validate_qbit_rss_rules(&rules)?;
        Ok(rules)
    }
}

async fn save_qbit_rss_rules(state: &AppState, rules: JsonMap) -> Result<(), String> {
    validate_qbit_map(&rules, "qBittorrent RSS rules", MAX_QBIT_RSS_RULES)?;
    validate_qbit_rss_rules(&rules)?;
    if let Some(engine) = &state.engine {
        let encoded = encode_qbit_json(&rules, "qBittorrent RSS rules")?;
        engine
            .set_setting(SETTING_QBIT_RSS_RULES.to_owned(), encoded)
            .await?;
    }
    *state.rss_rules.write().await = rules;
    Ok(())
}

fn validate_qbit_rss_rules(rules: &JsonMap) -> Result<(), String> {
    for (name, rule) in rules {
        if name.trim().is_empty() {
            return Err("qBittorrent RSS rules contain an empty name".to_owned());
        }
        let rule = rule
            .as_object()
            .ok_or_else(|| format!("qBittorrent RSS rule {name:?} must be a JSON object"))?;
        for field in [
            "mustContain",
            "mustNotContain",
            "assignedCategory",
            "savePath",
            "tags",
        ] {
            if let Some(value) = rule.get(field) {
                if !value.is_string() {
                    return Err(format!(
                        "qBittorrent RSS rule {name:?} field {field} must be a string"
                    ));
                }
            }
        }
        for field in ["enabled", "addPaused"] {
            if let Some(value) = rule.get(field) {
                if !value.is_boolean() {
                    return Err(format!(
                        "qBittorrent RSS rule {name:?} field {field} must be a boolean"
                    ));
                }
            }
        }
        if let Some(feeds) = rule.get("affectedFeeds") {
            let feeds = feeds.as_array().ok_or_else(|| {
                format!("qBittorrent RSS rule {name:?} field affectedFeeds must be an array")
            })?;
            if feeds
                .iter()
                .any(|feed| feed.as_str().is_none_or(|feed| feed.trim().is_empty()))
            {
                return Err(format!(
                    "qBittorrent RSS rule {name:?} contains an invalid affected feed"
                ));
            }
        }
    }
    Ok(())
}

async fn load_qbit_search_plugins(state: &AppState) -> Result<JsonMap, String> {
    if let Some(engine) = &state.engine {
        match engine
            .get_setting(SETTING_QBIT_SEARCH_PLUGINS.to_owned())
            .await?
        {
            Some(raw) => {
                let plugins = decode_qbit_map(
                    &raw,
                    "durable qBittorrent search plugins",
                    MAX_QBIT_SEARCH_PLUGINS,
                )?;
                validate_qbit_search_plugins(&plugins)?;
                Ok(plugins)
            }
            None => {
                let plugins = state.search_plugins.read().await.clone();
                validate_qbit_search_plugins(&plugins)?;
                Ok(plugins)
            }
        }
    } else {
        let plugins = state.search_plugins.read().await.clone();
        validate_qbit_search_plugins(&plugins)?;
        Ok(plugins)
    }
}

async fn save_qbit_search_plugins(state: &AppState, plugins: JsonMap) -> Result<(), String> {
    validate_qbit_map(
        &plugins,
        "qBittorrent search plugins",
        MAX_QBIT_SEARCH_PLUGINS,
    )?;
    validate_qbit_search_plugins(&plugins)?;
    if let Some(engine) = &state.engine {
        let encoded = encode_qbit_json(&plugins, "qBittorrent search plugins")?;
        engine
            .set_setting(SETTING_QBIT_SEARCH_PLUGINS.to_owned(), encoded)
            .await?;
    }
    *state.search_plugins.write().await = plugins;
    Ok(())
}

fn validate_qbit_search_plugins(plugins: &JsonMap) -> Result<(), String> {
    for (name, plugin) in plugins {
        if name.trim().is_empty() {
            return Err("qBittorrent search plugins contain an empty name".to_owned());
        }
        let plugin = plugin
            .as_object()
            .ok_or_else(|| format!("qBittorrent search plugin {name:?} must be a JSON object"))?;
        for field in ["name", "fullName", "version", "url"] {
            if let Some(value) = plugin.get(field) {
                if !value.is_string() {
                    return Err(format!(
                        "qBittorrent search plugin {name:?} field {field} must be a string"
                    ));
                }
            }
        }
        if let Some(enabled) = plugin.get("enabled") {
            if !enabled.is_boolean() {
                return Err(format!(
                    "qBittorrent search plugin {name:?} field enabled must be a boolean"
                ));
            }
        }
        if let Some(categories) = plugin.get("supportedCategories") {
            let categories = categories.as_array().ok_or_else(|| {
                format!(
                    "qBittorrent search plugin {name:?} field supportedCategories must be an array"
                )
            })?;
            if categories.iter().any(|category| {
                category
                    .as_str()
                    .is_none_or(|category| category.trim().is_empty())
            }) {
                return Err(format!(
                    "qBittorrent search plugin {name:?} contains an invalid supported category"
                ));
            }
        }
    }
    Ok(())
}

fn qbit_backend_error(error: impl std::fmt::Display) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": {
                "code": "SERVICE_UNAVAILABLE",
                "message": error.to_string(),
            }
        })),
    )
        .into_response()
}

fn qbit_engine_error_status(error: impl std::fmt::Display) -> StatusCode {
    let message = error.to_string();
    if message.to_ascii_lowercase().contains("not found") {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

pub async fn app_preferences(State(state): State<AppState>) -> Response {
    let stored_preferences = match load_qbit_preferences(&state).await {
        Ok(preferences) => preferences,
        Err(error) => return qbit_backend_error(error),
    };
    let save_path = match default_save_path(&state, &stored_preferences).await {
        Ok(path) => path,
        Err(error) => return qbit_backend_error(error),
    };
    let mut preferences = qbit_preferences(save_path);
    if let Some(map) = preferences.as_object_mut() {
        if let Some(engine) = &state.engine {
            let features = match engine.network_features().await {
                Ok(features) => features,
                Err(_) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": {
                                "code": "SERVICE_UNAVAILABLE",
                                "message": "native engine network features are unavailable",
                            }
                        })),
                    )
                        .into_response()
                }
            };
            map.insert("dht".to_owned(), serde_json::Value::Bool(features.dht));
            map.insert("pex".to_owned(), serde_json::Value::Bool(features.pex));
        }
        let banned_ips = if let Some(engine) = &state.engine {
            match engine.banned_peers().await {
                Ok(peers) => peers,
                Err(_) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": {
                                "code": "SERVICE_UNAVAILABLE",
                                "message": "native engine peer-ban state is unavailable",
                            }
                        })),
                    )
                        .into_response()
                }
            }
            .into_iter()
            .map(|peer| peer.to_string())
            .collect::<Vec<_>>()
            .join("\n")
        } else {
            state
                .banned_peers
                .read()
                .await
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        map.insert(
            "banned_ips".to_owned(),
            serde_json::Value::String(banned_ips),
        );
        for (key, value) in &stored_preferences {
            // DHT/PEX are read from the engine above. Do not let a stale
            // compatibility override mask the authoritative runtime value.
            if matches!(key.as_str(), "dht" | "pex") {
                continue;
            }
            map.insert(key.clone(), value.clone());
        }
    }
    (StatusCode::OK, Json(preferences)).into_response()
}

pub async fn app_set_preferences(State(state): State<AppState>, body: String) -> Response {
    match qbit_preference_payload(&body) {
        Some(serde_json::Value::Object(updates)) => {
            if updates
                .iter()
                .filter(|(key, _)| matches!(key.as_str(), "dht" | "pex"))
                .any(|(_, value)| !value.is_boolean())
            {
                return StatusCode::BAD_REQUEST.into_response();
            }
            let _write = state.preference_write.lock().await;
            let mut stored_preferences = match load_qbit_preferences(&state).await {
                Ok(preferences) => preferences,
                Err(error) => return qbit_backend_error(error),
            };
            if let Some(engine) = &state.engine {
                if updates.contains_key("dht") || updates.contains_key("pex") {
                    let mut features = match engine.network_features().await {
                        Ok(features) => features,
                        Err(error) => return qbit_backend_error(error),
                    };
                    if let Some(value) = updates.get("dht").and_then(serde_json::Value::as_bool) {
                        features.dht = value;
                    }
                    if let Some(value) = updates.get("pex").and_then(serde_json::Value::as_bool) {
                        features.pex = value;
                    }
                    if let Err(error) = engine.update_network_features(features).await {
                        return qbit_backend_error(error);
                    }
                }
            }
            let mut stored_updates = updates;
            if state.engine.is_some() {
                // These two keys are applied to and read back from the native
                // engine. Keeping a second facade copy would create split
                // brain state after a restart or another control-plane write.
                stored_updates.remove("dht");
                stored_updates.remove("pex");
            }
            stored_preferences.extend(stored_updates);
            if let Err(error) = save_qbit_preferences(&state, stored_preferences).await {
                return qbit_backend_error(error);
            }
            StatusCode::OK.into_response()
        }
        Some(_) | None => StatusCode::BAD_REQUEST.into_response(),
    }
}

pub async fn app_shutdown(State(state): State<AppState>) -> impl IntoResponse {
    if let Some(shutdown) = state.shutdown {
        // `notify_one` retains the permit if the daemon's graceful-shutdown
        // future has not reached its select yet, so an early HTTP request
        // cannot be lost in a startup race.
        shutdown.notify_one();
    }
    StatusCode::OK
}

pub async fn app_send_test_email() -> impl IntoResponse {
    // No SMTP subsystem is configured by TorrentNG. Returning 200 here would
    // make qBittorrent report a successful side effect that never occurred.
    StatusCode::NOT_IMPLEMENTED
}

pub async fn app_get_cookies(State(state): State<AppState>) -> Response {
    let mut cookies = match load_qbit_cookies(&state).await {
        Ok(cookies) => cookies,
        Err(error) => return qbit_backend_error(error),
    };
    cookies.sort_by(|a, b| {
        a.get("host")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .cmp(
                b.get("host")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
            )
            .then_with(|| {
                a.get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .cmp(
                        b.get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default(),
                    )
            })
    });
    (StatusCode::OK, Json(cookies)).into_response()
}

pub async fn app_set_cookies(State(state): State<AppState>, body: String) -> Response {
    let Some(cookies) = qbit_cookie_payload(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let _write = state.preference_write.lock().await;
    match save_qbit_cookies(&state, cookies).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn app_rotate_api_key(State(state): State<AppState>) -> Response {
    let key = new_qbit_api_key();
    let _write = state.preference_write.lock().await;
    if let Err(error) = save_qbit_api_key(&state, Some(key.clone())).await {
        return qbit_backend_error(error);
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "apiKey": key,
        })),
    )
        .into_response()
}

pub async fn app_delete_api_key(State(state): State<AppState>) -> Response {
    let _write = state.preference_write.lock().await;
    match save_qbit_api_key(&state, None).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn app_network_interface_list() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(vec![serde_json::json!({
            "name": "Any interface",
            "value": "",
        })]),
    )
}

pub async fn app_network_interface_address_list() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(vec![serde_json::json!({
            "name": "All addresses",
            "value": "",
        })]),
    )
}

pub async fn app_default_save_path(State(state): State<AppState>) -> impl IntoResponse {
    match load_qbit_preferences(&state).await {
        Ok(preferences) => match default_save_path(&state, &preferences).await {
            Ok(path) => (StatusCode::OK, path).into_response(),
            Err(error) => qbit_backend_error(error),
        },
        Err(error) => qbit_backend_error(error),
    }
}

fn qbit_preferences(save_path: String) -> serde_json::Value {
    serde_json::json!({
        "locale": "en",
        "create_subfolder_enabled": false,
        "start_paused_enabled": false,
        "auto_delete_mode": 0,
        "preallocate_all": false,
        "incomplete_files_ext": false,
        "auto_tmm_enabled": false,
        "torrent_changed_tmm_enabled": false,
        "save_path_changed_tmm_enabled": false,
        "category_changed_tmm_enabled": false,
        "save_path": save_path,
        "temp_path_enabled": false,
        "temp_path": "",
        "scan_dirs": {},
        "download_in_scan_dirs": {},
        "export_dir_enabled": false,
        "export_dir": "",
        "export_dir_fin": "",
        "mail_notification_enabled": false,
        "mail_notification_sender": "",
        "mail_notification_email": "",
        "mail_notification_smtp": "",
        "mail_notification_ssl_enabled": false,
        "mail_notification_auth_enabled": false,
        "mail_notification_username": "",
        "mail_notification_password": "",
        "autorun_enabled": false,
        "autorun_program": "",
        "queueing_enabled": false,
        "max_active_downloads": -1,
        "max_active_torrents": -1,
        "max_active_uploads": -1,
        "dont_count_slow_torrents": false,
        "slow_torrent_dl_rate_threshold": 2,
        "slow_torrent_ul_rate_threshold": 2,
        "slow_torrent_inactive_timer": 60,
        "max_ratio_enabled": false,
        "max_ratio": -1.0,
        "max_ratio_act": 0,
        "max_seeding_time_enabled": false,
        "max_seeding_time": -1,
        "listen_port": 0,
        "upnp": false,
        "random_port": false,
        "dl_limit": 0,
        "up_limit": 0,
        "max_connec": -1,
        "max_connec_per_torrent": -1,
        "max_uploads": -1,
        "max_uploads_per_torrent": -1,
        "stop_tracker_timeout": 1,
        "enable_piece_extent_affinity": false,
        "bittorrent_protocol": 0,
        "limit_utp_rate": false,
        "limit_tcp_overhead": false,
        "limit_lan_peers": false,
        "alt_dl_limit": 0,
        "alt_up_limit": 0,
        "scheduler_enabled": false,
        "schedule_from_hour": 8,
        "schedule_from_min": 0,
        "schedule_to_hour": 20,
        "schedule_to_min": 0,
        "scheduler_days": 0,
        "dht": true,
        "dhtSameAsBT": true,
        "dht_port": 0,
        "pex": true,
        "lsd": false,
        "encryption": 0,
        "anonymous_mode": false,
        "proxy_type": -1,
        "proxy_ip": "",
        "proxy_port": 0,
        "proxy_peer_connections": false,
        "proxy_auth_enabled": false,
        "proxy_username": "",
        "proxy_password": "",
        "proxy_torrents_only": false,
        "ip_filter_enabled": false,
        "ip_filter_path": "",
        "ip_filter_trackers": false,
        "web_ui_domain_list": "*",
        "web_ui_address": "0.0.0.0",
        "web_ui_port": 8080,
        "web_ui_upnp": false,
        "web_ui_username": "admin",
        "web_ui_password": "",
        "web_ui_csrf_protection_enabled": true,
        "web_ui_clickjacking_protection_enabled": true,
        "web_ui_secure_cookie_enabled": false,
        "web_ui_max_auth_fail_count": 5,
        "web_ui_ban_duration": 3600,
        "web_ui_session_timeout": 3600,
        "web_ui_host_header_validation_enabled": false,
        "bypass_local_auth": false,
        "bypass_auth_subnet_whitelist_enabled": false,
        "bypass_auth_subnet_whitelist": "",
        "alternative_webui_enabled": false,
        "alternative_webui_path": "",
        "use_https": false,
        "ssl_key": "",
        "ssl_cert": "",
        "web_ui_https_key_path": "",
        "web_ui_https_cert_path": "",
        "dyndns_enabled": false,
        "dyndns_service": 0,
        "dyndns_username": "",
        "dyndns_password": "",
        "dyndns_domain": "",
        "rss_refresh_interval": 30,
        "rss_max_articles_per_feed": 50,
        "rss_processing_enabled": false,
        "rss_auto_downloading_enabled": false,
        "rss_download_repack_proper_episodes": true,
        "rss_smart_episode_filters": "",
        "add_trackers_enabled": false,
        "add_trackers": "",
        "web_ui_use_custom_http_headers_enabled": false,
        "web_ui_custom_http_headers": "",
        "announce_to_all_tiers": true,
        "announce_to_all_trackers": false,
        "async_io_threads": 10,
        "banned_ips": "",
        "checking_memory_use": 32,
        "current_interface_address": "",
        "current_network_interface": "",
        "disk_cache": -1,
        "disk_cache_ttl": 60,
        "embedded_tracker_port": 0,
        "enable_coalesce_read_write": false,
        "enable_embedded_tracker": false,
        "enable_multi_connections_from_same_ip": false,
        "enable_os_cache": true,
        "enable_upload_suggestions": false,
        "file_pool_size": 5000,
        "outgoing_ports_max": 0,
        "outgoing_ports_min": 0,
        "recheck_completed_torrents": false,
        "resolve_peer_countries": false,
        "save_resume_data_interval": 60,
        "send_buffer_low_watermark": 10,
        "send_buffer_watermark": 500,
        "send_buffer_watermark_factor": 50,
        "socket_backlog_size": 30,
        "upload_choking_algorithm": 1,
        "upload_slots_behavior": 0,
        "upnp_lease_duration": 0,
        "utp_tcp_mixed_mode": 0,
    })
}

fn qbit_preference_payload(body: &str) -> Option<serde_json::Value> {
    if body.len() > MAX_QBIT_PREFERENCE_BYTES {
        return None;
    }
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).ok();
    }
    parse_form_body(trimmed)
        .remove("json")
        .and_then(|json| serde_json::from_str(&json).ok())
}

fn qbit_cookie_payload(body: &str) -> Option<Vec<serde_json::Value>> {
    if body.len() > MAX_QBIT_PREFERENCE_BYTES {
        return None;
    }
    let trimmed = body.trim();
    let value = if trimmed.starts_with('[') {
        serde_json::from_str(trimmed).ok()?
    } else if trimmed.starts_with('{') {
        serde_json::from_str(trimmed)
            .ok()
            .and_then(|value: serde_json::Value| value.get("cookies").cloned())?
    } else {
        parse_form_body(trimmed)
            .remove("cookies")
            .and_then(|json| serde_json::from_str(&json).ok())?
    };
    match value {
        serde_json::Value::Array(cookies) => Some(
            if cookies.len() > MAX_QBIT_COOKIE_COUNT
                || cookies.iter().any(|cookie| !cookie.is_object())
            {
                return None;
            } else {
                cookies
            },
        ),
        _ => None,
    }
}

fn new_qbit_api_key() -> String {
    let mut bytes = [0_u8; 32];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut bytes);
    format!("tng_{}", hex::encode(bytes))
}

// ---------------------------------------------------------------------------
// Torrents
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TorrentsInfoQuery {
    pub filter: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub sort: Option<String>,
    pub reverse: Option<bool>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub hashes: Option<String>,
    /// Optional TorrentNG snapshot generation for stable offset pagination.
    /// qBittorrent clients ignore the response header, while native clients
    /// can use it to pin multiple pages to one immutable registry view.
    pub snapshot: Option<u64>,
}

pub async fn torrents_info(
    State(state): State<AppState>,
    Query(q): Query<TorrentsInfoQuery>,
) -> impl IntoResponse {
    if let Err((status, message)) = validate_qbit_filter(q.filter.as_deref()) {
        return (status, message).into_response();
    }
    if let Err((status, message)) = validate_qbit_sort(q.sort.as_deref()) {
        return (status, message).into_response();
    }
    if q.offset.unwrap_or(0) > MAX_TORRENT_LIST_OFFSET {
        return (
            StatusCode::BAD_REQUEST,
            format!("offset is limited to {MAX_TORRENT_LIST_OFFSET}"),
        )
            .into_response();
    }
    let snapshot = match state.torrent_snapshot(q.snapshot).await {
        Ok(snapshot) => snapshot,
        Err(TorrentSnapshotError::Expired { revision }) => {
            return (
                StatusCode::GONE,
                format!("torrent snapshot {revision} expired; restart pagination"),
            )
                .into_response();
        }
    };
    let hashes = match q.hashes.as_deref().map(str::trim) {
        None | Some("") | Some("all") => None,
        Some(raw) => match strict_hashes_from_str(raw) {
            Some(values) => Some(values.into_iter().collect::<HashSet<_>>()),
            None => {
                return (StatusCode::BAD_REQUEST, "invalid hashes filter").into_response();
            }
        },
    };
    let sort = canonical_sort_key(q.sort.as_deref());
    let order = snapshot.ordered_indices(Some(sort), |left, right| match sort {
        "hash" => left.info_hash.cmp(&right.info_hash),
        "size" => left.total_length.cmp(&right.total_length),
        "progress" => torrent_progress(
            left.total_length,
            left.amount_left,
            left.completed_at.is_some(),
        )
        .total_cmp(&torrent_progress(
            right.total_length,
            right.amount_left,
            right.completed_at.is_some(),
        )),
        "ratio" => left.stats.ratio().total_cmp(&right.stats.ratio()),
        "added_on" => left.added_at.cmp(&right.added_at),
        "completion_on" => left
            .completed_at
            .unwrap_or(0)
            .cmp(&right.completed_at.unwrap_or(0)),
        "category" => left.category.cmp(&right.category),
        "state" => to_qbit_state(left.state.as_str()).cmp(to_qbit_state(right.state.as_str())),
        _ => left.name.cmp(&right.name),
    });
    let (indexed_states, completed_only) = indexed_qbit_filter(q.filter.as_deref());
    let candidates = snapshot.candidate_indices(
        hashes.as_ref(),
        &indexed_states,
        completed_only,
        q.category.as_deref(),
        q.tag.as_deref(),
    );
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(QBIT_DEFAULT_PAGE_SIZE).clamp(1, 5_000);
    let descending = q.reverse.unwrap_or(false);
    let mut skipped = 0;
    // Keep the initial allocation independent of the caller's page size. The
    // response remains bounded by `limit`, but a hostile limit must not flow
    // directly into an allocation site.
    let mut selected = Vec::with_capacity(QBIT_LIST_INITIAL_CAPACITY);
    let indices: Box<dyn Iterator<Item = &usize>> = if descending {
        Box::new(order.iter().rev())
    } else {
        Box::new(order.iter())
    };
    for index in indices {
        if candidates
            .as_ref()
            .is_some_and(|candidate| candidate.binary_search(index).is_err())
        {
            continue;
        }
        let entry = snapshot
            .entries
            .get(*index)
            .expect("snapshot index is valid");
        if !qbit_entry_matches(entry, &q, hashes.as_ref()) {
            continue;
        }
        if skipped < offset {
            skipped += 1;
            continue;
        }
        selected.push(*index);
        if selected.len() == limit {
            break;
        }
    }
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(
            &state,
            estimate_qbit_torrent_info_snapshot_bytes(selected.len()),
        )
        .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) => return qbit_api_snapshot_budget_exhausted(),
            Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
        }
    } else {
        None
    };
    let active_rechecks = match active_recheck_hashes(&state).await {
        Ok(active_rechecks) => active_rechecks,
        Err(error) => return qbit_backend_unavailable(&error),
    };
    let include_live = selected.len() <= QBIT_LIVE_PROJECTION_MAX_ENTRIES;
    let infos = if include_live {
        let entries = selected
            .iter()
            .map(|index| {
                snapshot
                    .entries
                    .get(*index)
                    .expect("snapshot index is valid")
                    .clone()
            })
            .collect();
        match load_qbit_live_projections(&state, entries, active_rechecks).await {
            Ok(infos) => infos,
            Err(error) => return qbit_backend_unavailable(&error),
        }
    } else {
        let mut infos = Vec::with_capacity(selected.len());
        for index in selected {
            let info = match qbit_torrent_info(
                &state,
                snapshot
                    .entries
                    .get(index)
                    .expect("snapshot index is valid"),
                &active_rechecks,
                false,
            )
            .await
            {
                Ok(info) => info,
                Err(error) => return qbit_backend_unavailable(&error),
            };
            infos.push(info);
        }
        infos
    };
    state
        .api_metrics
        .record_estimated_response_bytes(estimate_qbit_torrent_info_snapshot_bytes(infos.len()));

    let mut response = (StatusCode::OK, Json(infos)).into_response();
    if let Ok(value) = HeaderValue::from_str(&snapshot.revision.to_string()) {
        response.headers_mut().insert(
            header::HeaderName::from_static("x-torrentng-snapshot"),
            value,
        );
    }
    response
}

fn indexed_qbit_filter(filter: Option<&str>) -> (Vec<&'static str>, bool) {
    match filter.map(str::trim) {
        Some("downloading") => (vec!["downloading"], false),
        Some("seeding" | "uploading") => (vec!["seeding"], false),
        Some("completed") => (Vec::new(), true),
        Some("paused") => (vec!["paused", "stopped"], false),
        Some("active" | "resumed") => (
            vec![
                "metadata_pending",
                "checking",
                "seeding",
                "downloading",
                "queued",
            ],
            false,
        ),
        Some("errored") => (vec!["error"], false),
        Some("checking") => (vec!["checking"], false),
        _ => (Vec::new(), false),
    }
}

fn validate_qbit_filter(filter: Option<&str>) -> Result<(), (StatusCode, &'static str)> {
    let Some(filter) = filter.map(str::trim) else {
        return Ok(());
    };
    if filter.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "qBittorrent filter must not be empty",
        ));
    }
    if matches!(
        filter,
        "stalled" | "stalled_uploading" | "stalled_downloading" | "moving" | "missingFiles"
    ) {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "qBittorrent filter is not runtime-backed by the native engine",
        ));
    }
    if !matches!(
        filter,
        "all"
            | "downloading"
            | "seeding"
            | "completed"
            | "paused"
            | "active"
            | "inactive"
            | "resumed"
            | "checking"
            | "errored"
            | "uploading"
    ) {
        return Err((
            StatusCode::BAD_REQUEST,
            "unknown qBittorrent torrent filter",
        ));
    }
    Ok(())
}

fn validate_qbit_sort(sort: Option<&str>) -> Result<(), (StatusCode, &'static str)> {
    if matches!(sort.map(str::trim), Some("dlspeed" | "upspeed")) {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "qBittorrent speed sorting is not runtime-backed by the native engine",
        ));
    }
    Ok(())
}

fn qbit_entry_matches(
    entry: &rt_session::TorrentEntry,
    query: &TorrentsInfoQuery,
    hashes: Option<&HashSet<String>>,
) -> bool {
    if let Some(hashes) = hashes {
        if !hashes.contains(&entry.info_hash) {
            return false;
        }
    }
    if let Some(category) = query.category.as_deref() {
        if entry.category.as_deref() != Some(category) {
            return false;
        }
    }
    if let Some(tag) = query.tag.as_deref() {
        if !tag.is_empty() && !entry.tags.iter().any(|entry_tag| entry_tag == tag) {
            return false;
        }
    }
    if let Some(filter) = query.filter.as_deref() {
        let qb_state = to_qbit_state(entry.state.as_str());
        match filter {
            "all" => {}
            "downloading" if qb_state != "downloading" => return false,
            "seeding" | "uploading" if qb_state != "uploading" => return false,
            "completed" if entry.completed_at.is_none() => return false,
            "paused" if !matches!(qb_state, "pausedUP" | "pausedDL") => return false,
            "active" | "resumed"
                if !matches!(
                    entry.state,
                    rt_session::TorrentState::MetadataPending
                        | rt_session::TorrentState::Checking
                        | rt_session::TorrentState::Seeding
                        | rt_session::TorrentState::Downloading
                        | rt_session::TorrentState::Queued
                ) =>
            {
                return false
            }
            "inactive"
                if matches!(
                    entry.state,
                    rt_session::TorrentState::MetadataPending
                        | rt_session::TorrentState::Checking
                        | rt_session::TorrentState::Seeding
                        | rt_session::TorrentState::Downloading
                        | rt_session::TorrentState::Queued
                ) =>
            {
                return false
            }
            "checking" if entry.state != rt_session::TorrentState::Checking => return false,
            "errored" if entry.state != rt_session::TorrentState::Error => return false,
            _ => {}
        }
    }
    true
}

/// `POST /api/qb/v2/torrents/add`.
pub async fn torrents_add(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Fails.").into_response();
    };

    let mut save_path = String::new();
    let mut paused = false;
    let mut stopped = false;
    let mut torrent_blobs = Vec::new();
    let mut urls = String::new();
    let mut category = String::new();
    let mut tags = Vec::<String>::new();
    let mut skip_checking = false;
    let mut content_layout: Option<String> = None;
    let mut auto_tmm: Option<bool> = None;
    let mut ratio_limit: Option<f64> = None;
    let mut seeding_time_limit: Option<i64> = None;

    loop {
        let Some(field) = (match multipart.next_field().await {
            Ok(field) => field,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("invalid multipart body: {error}"),
                )
                    .into_response()
            }
        }) else {
            break;
        };
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("savepath") => {
                save_path = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid savepath field: {error}"),
                        )
                            .into_response()
                    }
                };
            }
            Some("paused") => {
                paused = match field.text().await {
                    Ok(value) => match parse_qbit_bool(value.trim()) {
                        Some(value) => value,
                        None => return StatusCode::BAD_REQUEST.into_response(),
                    },
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid paused field: {error}"),
                        )
                            .into_response()
                    }
                };
            }
            Some("stopped") => {
                stopped = match field.text().await {
                    Ok(value) => match parse_qbit_bool(value.trim()) {
                        Some(value) => value,
                        None => return StatusCode::BAD_REQUEST.into_response(),
                    },
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid stopped field: {error}"),
                        )
                            .into_response()
                    }
                };
            }
            Some("urls") => {
                urls = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid urls field: {error}"),
                        )
                            .into_response()
                    }
                };
            }
            Some("category") => {
                category = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid category field: {error}"),
                        )
                            .into_response()
                    }
                };
            }
            Some("tags") => {
                let value = match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid tags field: {error}"),
                        )
                            .into_response()
                    }
                };
                tags = match strict_tag_values(&value, true) {
                    Ok(tags) => tags,
                    Err(()) => return (StatusCode::BAD_REQUEST, "Fails.").into_response(),
                };
            }
            Some("skip_checking") => {
                skip_checking = match field.text().await {
                    Ok(value) => match parse_qbit_bool(value.trim()) {
                        Some(value) => value,
                        None => return StatusCode::BAD_REQUEST.into_response(),
                    },
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid skip_checking field: {error}"),
                        )
                            .into_response()
                    }
                };
            }
            Some("contentLayout") => {
                content_layout = Some(match field.text().await {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid contentLayout field: {error}"),
                        )
                            .into_response()
                    }
                });
            }
            Some("autoTMM") | Some("useAutoTMM") => {
                auto_tmm = Some(match field.text().await {
                    Ok(value) => match parse_qbit_bool(value.trim()) {
                        Some(value) => value,
                        None => return StatusCode::BAD_REQUEST.into_response(),
                    },
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid autoTMM field: {error}"),
                        )
                            .into_response()
                    }
                });
            }
            Some("ratioLimit") => {
                ratio_limit = Some(match field.text().await {
                    Ok(value) => match value.parse::<f64>() {
                        Ok(value)
                            if value.is_finite()
                                && (value >= 0.0 || value == -1.0 || value == -2.0) =>
                        {
                            value
                        }
                        _ => return StatusCode::BAD_REQUEST.into_response(),
                    },
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid ratioLimit field: {error}"),
                        )
                            .into_response()
                    }
                });
            }
            Some("seedingTimeLimit") => {
                seeding_time_limit = Some(match field.text().await {
                    Ok(value) => match value.parse::<i64>() {
                        Ok(value) if value >= 0 || value == -1 || value == -2 => value,
                        _ => return StatusCode::BAD_REQUEST.into_response(),
                    },
                    Err(error) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("invalid seedingTimeLimit field: {error}"),
                        )
                            .into_response()
                    }
                });
            }
            Some("torrents") => match field.bytes().await {
                Ok(bytes) => torrent_blobs.push(bytes.to_vec()),
                Err(error) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!("invalid torrent field: {error}"),
                    )
                        .into_response()
                }
            },
            Some(name) => {
                // Do not silently discard a qBit add option. If it has no
                // native engine contract, accepting the request would create
                // a torrent with policy different from the caller's request.
                let _ = field.text().await;
                tracing::info!(
                    component = "api",
                    operation = "add_torrent",
                    field = %name,
                    result = "unsupported",
                    "qBit add option has no native engine contract"
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

    if skip_checking
        || content_layout.as_deref().is_some_and(|layout| {
            !layout.trim().is_empty() && !layout.eq_ignore_ascii_case("Original")
        })
        || auto_tmm == Some(true)
    {
        return (StatusCode::NOT_IMPLEMENTED, "Fails.").into_response();
    }

    if save_path.trim().is_empty() {
        let preferences = match load_qbit_preferences(&state).await {
            Ok(preferences) => preferences,
            Err(error) => return qbit_backend_error(error),
        };
        save_path = match default_save_path(&state, &preferences).await {
            Ok(path) => path,
            Err(error) => return qbit_backend_error(error),
        };
    }

    let url_values = if urls.trim().is_empty() {
        Vec::new()
    } else {
        let values = urls.lines().map(str::trim).collect::<Vec<_>>();
        if values.iter().any(|url| url.is_empty()) {
            return (StatusCode::BAD_REQUEST, "Fails.").into_response();
        }
        values
    };
    let mut added_hashes = Vec::new();
    for url in url_values {
        if url
            .get(.."magnet:".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("magnet:"))
        {
            let magnet = match parse_magnet(url) {
                Ok(magnet) => magnet,
                Err(e) => {
                    tracing::error!(
                        component = "api",
                        operation = "add_magnet",
                        source = "magnet:redacted",
                        result = "error",
                        error = %e,
                        "qBit magnet parse failed"
                    );
                    rollback_qbit_added_torrents(engine, &added_hashes).await;
                    return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                }
            };
            let save_path = if save_path.trim().is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(save_path.clone()))
            };
            let hash = match engine
                .add_magnet_with_labels(
                    magnet,
                    save_path,
                    paused || stopped,
                    Some(category.clone()),
                    tags.clone(),
                )
                .await
            {
                Ok(hash) => hash,
                Err(e) => {
                    tracing::error!(
                        component = "api",
                        operation = "add_magnet",
                        result = "error",
                        error = %e,
                        "qBit magnet add failed"
                    );
                    rollback_qbit_added_torrents(engine, &added_hashes).await;
                    return (StatusCode::BAD_REQUEST, "Fails.").into_response();
                }
            };
            if ratio_limit.is_some_and(|value| value >= 0.0)
                || seeding_time_limit.is_some_and(|value| value >= 0)
            {
                let mut limits = match engine.torrent_limits(hash.clone()).await {
                    Ok(limits) => limits,
                    Err(error) => {
                        rollback_qbit_added_torrents(engine, &added_hashes).await;
                        let _ = engine.remove_torrent(hash, false).await;
                        return qbit_engine_error_status(error).into_response();
                    }
                };
                if let Some(value) = ratio_limit {
                    limits.seed_ratio_limit = (value >= 0.0).then_some(value);
                }
                if let Some(value) = seeding_time_limit {
                    limits.seed_idle_limit = (value >= 0).then_some(value);
                }
                if let Err(error) = engine.update_torrent_limits(hash.clone(), limits).await {
                    rollback_qbit_added_torrents(engine, &added_hashes).await;
                    let _ = engine.remove_torrent(hash, false).await;
                    return qbit_engine_error_status(error).into_response();
                }
            }
            added_hashes.push(hash);
            continue;
        }
        match fetch_torrent_url(url, &state.egress_policy).await {
            Ok(raw) => torrent_blobs.push(raw),
            Err(e) => {
                tracing::error!(
                    component = "api",
                    operation = "add_torrent_url",
                    source = %redact_log_url(url),
                    result = "error",
                    error = %e,
                    "qBit torrent URL fetch failed"
                );
                rollback_qbit_added_torrents(engine, &added_hashes).await;
                return (StatusCode::BAD_REQUEST, "Fails.").into_response();
            }
        }
    }

    if torrent_blobs.is_empty() {
        if !added_hashes.is_empty() {
            return (StatusCode::OK, "Ok.").into_response();
        }
        return (StatusCode::BAD_REQUEST, "Fails.").into_response();
    }

    let save_path = if save_path.trim().is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(save_path))
    };
    let start_paused = paused || stopped;

    for raw in torrent_blobs {
        let hash = match engine
            .add_torrent_raw_with_labels(
                raw,
                save_path.clone(),
                start_paused,
                Some(category.clone()),
                tags.clone(),
            )
            .await
        {
            Ok(hash) => hash,
            Err(e) => {
                tracing::error!(
                    component = "api",
                    operation = "add_torrent",
                    result = "error",
                    error = %e,
                    "qBit torrent add failed"
                );
                rollback_qbit_added_torrents(engine, &added_hashes).await;
                return (StatusCode::BAD_REQUEST, "Fails.").into_response();
            }
        };
        if ratio_limit.is_some_and(|value| value >= 0.0)
            || seeding_time_limit.is_some_and(|value| value >= 0)
        {
            let mut limits = match engine.torrent_limits(hash.clone()).await {
                Ok(limits) => limits,
                Err(error) => {
                    rollback_qbit_added_torrents(engine, &added_hashes).await;
                    let _ = engine.remove_torrent(hash, false).await;
                    return qbit_engine_error_status(error).into_response();
                }
            };
            if let Some(value) = ratio_limit {
                limits.seed_ratio_limit = (value >= 0.0).then_some(value);
            }
            if let Some(value) = seeding_time_limit {
                limits.seed_idle_limit = (value >= 0).then_some(value);
            }
            if let Err(error) = engine.update_torrent_limits(hash.clone(), limits).await {
                rollback_qbit_added_torrents(engine, &added_hashes).await;
                let _ = engine.remove_torrent(hash, false).await;
                return qbit_engine_error_status(error).into_response();
            }
        }
        added_hashes.push(hash);
    }

    (StatusCode::OK, "Ok.").into_response()
}

async fn rollback_qbit_added_torrents(engine: &rt_engine::EngineHandle, hashes: &[String]) {
    for hash in hashes {
        if let Err(error) = engine.remove_torrent(hash.clone(), false).await {
            tracing::error!(
                component = "api",
                operation = "add_torrent_rollback",
                torrent = %hash,
                result = "error",
                error = %error,
                "qBit bulk add rollback failed"
            );
        }
    }
}

/// `POST /api/qb/v2/torrents/pause` — pause by hashes (pipe-separated or "all").
pub async fn torrents_pause(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let Some(engine) = &state.engine else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    for hash in hashes {
        if let Err(error) = engine.pause_torrent(hash).await {
            return qbit_engine_error_status(error);
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/resume`.
pub async fn torrents_resume(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let Some(engine) = &state.engine else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    for hash in hashes {
        if let Err(error) = engine.resume_torrent(hash).await {
            return qbit_engine_error_status(error);
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/start`.
pub async fn torrents_start(State(state): State<AppState>, body: String) -> impl IntoResponse {
    torrents_resume(State(state), body).await
}

/// `POST /api/qb/v2/torrents/stop`.
pub async fn torrents_stop(State(state): State<AppState>, body: String) -> impl IntoResponse {
    torrents_pause(State(state), body).await
}

/// `POST /api/qb/v2/torrents/delete`.
pub async fn torrents_delete(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let delete_files = match params.get("deleteFiles") {
        Some(value) => match parse_qbit_bool(value) {
            Some(value) => value,
            None => return StatusCode::BAD_REQUEST,
        },
        None => false,
    };
    let Some(engine) = &state.engine else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    for hash in hashes {
        if let Err(error) = engine.remove_torrent(hash, delete_files).await {
            return qbit_engine_error_status(error);
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/reannounce`.
pub async fn torrents_reannounce(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let Some(engine) = &state.engine else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    for hash in hashes {
        if let Err(error) = engine.reannounce_torrent(hash).await {
            return qbit_engine_error_status(error);
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/recheck`.
pub async fn torrents_recheck(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let Some(engine) = &state.engine else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    for hash in hashes {
        if let Err(error) = engine.recheck_torrent(hash).await {
            return qbit_engine_error_status(error);
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/filePrio`.
pub async fn torrents_file_prio(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").and_then(|hash| normalize_api_text(hash)) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(raw_file_ids) = params.get("id").or_else(|| params.get("ids")) else {
        return StatusCode::BAD_REQUEST;
    };
    let file_ids = match strict_numeric_list(raw_file_ids) {
        Ok(file_ids) => file_ids,
        Err(()) => return StatusCode::BAD_REQUEST,
    };
    let Some(priority) = params
        .get("priority")
        .and_then(|value| value.parse::<i64>().ok())
    else {
        return StatusCode::BAD_REQUEST;
    };
    if !(0..=2).contains(&priority) {
        return StatusCode::BAD_REQUEST;
    }
    let Some(engine) = &state.engine else {
        return StatusCode::NOT_IMPLEMENTED;
    };
    match engine
        .update_file_priorities(hash, file_ids, priority)
        .await
    {
        Ok(()) => StatusCode::OK,
        Err(error) => qbit_engine_error_status(error),
    }
}

/// `POST /api/qb/v2/torrents/increasePrio`.
pub async fn torrents_increase_prio(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_queue_order(&state, &body, QueueMove::Up).await
}

/// `POST /api/qb/v2/torrents/decreasePrio`.
pub async fn torrents_decrease_prio(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_queue_order(&state, &body, QueueMove::Down).await
}

/// `POST /api/qb/v2/torrents/topPrio`.
pub async fn torrents_top_prio(State(state): State<AppState>, body: String) -> impl IntoResponse {
    update_queue_order(&state, &body, QueueMove::Top).await
}

/// `POST /api/qb/v2/torrents/bottomPrio`.
pub async fn torrents_bottom_prio(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_queue_order(&state, &body, QueueMove::Bottom).await
}

#[derive(Debug, Deserialize)]
pub struct HashQuery {
    pub hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HashesQuery {
    pub hashes: Option<String>,
}

/// `GET /api/qb/v2/torrents/trackers`.
pub async fn torrents_trackers(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return (StatusCode::BAD_REQUEST, Json(Vec::<QbTrackerInfo>::new()));
    };
    let exists = {
        let reg = state.registry.read().await;
        reg.get(&hash).is_some()
    };
    let Some(engine) = &state.engine else {
        return if exists {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Vec::<QbTrackerInfo>::new()),
            )
        } else {
            (StatusCode::NOT_FOUND, Json(Vec::<QbTrackerInfo>::new()))
        };
    };
    match engine.torrent_trackers(hash.clone()).await {
        Ok(trackers) => {
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_tracker_snapshot_bytes(trackers.len()),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(Vec::<QbTrackerInfo>::new()),
                    )
                }
            };
            (
                StatusCode::OK,
                Json(qbit_trackers_from_snapshots(&trackers)),
            )
        }
        Err(_) => match engine.torrent_metadata(hash).await {
            Ok(meta) => {
                let _lease = match reserve_qbit_api_snapshot(
                    &state,
                    estimate_qbit_tracker_snapshot_bytes(meta.trackers.len()),
                )
                .await
                {
                    Ok(Some(lease)) => Some(lease),
                    Ok(None) | Err(_) => {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(Vec::<QbTrackerInfo>::new()),
                        )
                    }
                };
                let trackers = meta
                    .trackers
                    .into_iter()
                    .enumerate()
                    .map(|(idx, url)| QbTrackerInfo {
                        url,
                        status: 0,
                        tier: qbit_i32(i64::try_from(idx).unwrap_or(i64::MAX)),
                        num_peers: -1,
                        num_seeds: -1,
                        num_leeches: -1,
                        num_downloaded: -1,
                        msg: String::new(),
                    })
                    .collect();
                (StatusCode::OK, Json(trackers))
            }
            Err(_) if exists => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Vec::<QbTrackerInfo>::new()),
            ),
            Err(_) => (StatusCode::NOT_FOUND, Json(Vec::<QbTrackerInfo>::new())),
        },
    }
}

/// `POST /api/qb/v2/torrents/addTrackers`.
pub async fn torrents_add_trackers(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").and_then(|hash| normalize_api_text(hash)) else {
        return StatusCode::BAD_REQUEST;
    };
    let urls = match params
        .get("urls")
        .and_then(|urls| strict_tracker_values(urls).ok())
    {
        Some(urls) if !urls.is_empty() => urls,
        _ => return StatusCode::BAD_REQUEST,
    };
    let mut trackers = match current_tracker_urls(&state, &hash).await {
        Ok(trackers) => trackers,
        Err(error) => return qbit_engine_error_status(error),
    };
    for url in urls {
        if !trackers.contains(&url) {
            trackers.push(url);
        }
    }
    update_torrent_trackers(&state, &hash, trackers).await
}

/// `POST /api/qb/v2/torrents/editTracker`.
pub async fn torrents_edit_tracker(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").and_then(|hash| normalize_api_text(hash)) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(original_url) = params
        .get("origUrl")
        .and_then(|url| normalize_api_text(url))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_url) = params.get("newUrl").and_then(|url| normalize_api_text(url)) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut trackers = match current_tracker_urls(&state, &hash).await {
        Ok(trackers) => trackers,
        Err(error) => return qbit_engine_error_status(error),
    };
    let Some(slot) = trackers.iter_mut().find(|url| **url == original_url) else {
        return StatusCode::NOT_FOUND;
    };
    *slot = new_url;
    trackers = normalize_tracker_values(trackers);
    update_torrent_trackers(&state, &hash, trackers).await
}

/// `POST /api/qb/v2/torrents/removeTrackers`.
pub async fn torrents_remove_trackers(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").and_then(|hash| normalize_api_text(hash)) else {
        return StatusCode::BAD_REQUEST;
    };
    let remove = match params
        .get("urls")
        .and_then(|urls| strict_tracker_values(urls).ok())
    {
        Some(remove) if !remove.is_empty() => remove,
        _ => return StatusCode::BAD_REQUEST,
    };
    let trackers = match current_tracker_urls(&state, &hash).await {
        Ok(trackers) => trackers,
        Err(error) => return qbit_engine_error_status(error),
    }
    .into_iter()
    .filter(|url| !remove.contains(url))
    .collect::<Vec<_>>();
    update_torrent_trackers(&state, &hash, trackers).await
}

/// `POST /api/qb/v2/torrents/addPeers`.
pub async fn torrents_add_peers(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes", "hash"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let peers = match params
        .get("peers")
        .and_then(|peers| strict_peer_addrs(peers).ok())
    {
        Some(peers) if !peers.is_empty() => peers,
        _ => return StatusCode::BAD_REQUEST,
    };
    let Some(engine) = &state.engine else {
        return StatusCode::NOT_IMPLEMENTED;
    };
    for hash in hashes {
        if let Err(error) = engine.add_peers(hash, peers.clone()).await {
            return qbit_engine_error_status(error);
        }
    }
    StatusCode::OK
}

/// `GET /api/qb/v2/torrents/files`.
pub async fn torrents_files(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return (StatusCode::BAD_REQUEST, Json(Vec::<QbFileInfo>::new()));
    };
    let exists = {
        let reg = state.registry.read().await;
        reg.get(&hash).is_some()
    };
    let Some(engine) = &state.engine else {
        return if exists {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Vec::<QbFileInfo>::new()),
            )
        } else {
            (StatusCode::NOT_FOUND, Json(Vec::<QbFileInfo>::new()))
        };
    };
    let entry = {
        let reg = state.registry.read().await;
        reg.get(&hash)
    };
    match engine.torrent_metadata(hash).await {
        Ok(meta) => {
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_metadata_snapshot_bytes(
                    meta.files.len(),
                    meta.piece_count,
                    meta.webseeds.len(),
                ),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(Vec::<QbFileInfo>::new()),
                    )
                }
            };
            let Some(entry) = entry else {
                // Metadata can outlive the registry entry during removal or
                // actor teardown. Do not turn that race into a successful
                // empty file list: the caller asked for a torrent that no
                // longer has an authoritative projection.
                return (StatusCode::NOT_FOUND, Json(Vec::<QbFileInfo>::new()));
            };
            (StatusCode::OK, Json(qbit_file_infos(&entry, &meta)))
        }
        Err(_) if exists => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Vec::<QbFileInfo>::new()),
        ),
        Err(_) => (StatusCode::NOT_FOUND, Json(Vec::<QbFileInfo>::new())),
    }
}

/// `GET /api/qb/v2/torrents/webseeds`.
pub async fn torrents_webseeds(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash.filter(|hash| !hash.trim().is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(Vec::<String>::new()));
    };
    let exists = {
        let reg = state.registry.read().await;
        reg.get(&hash).is_some()
    };
    let Some(engine) = &state.engine else {
        return if exists {
            (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new()))
        } else {
            (StatusCode::NOT_FOUND, Json(Vec::<String>::new()))
        };
    };
    match engine.torrent_metadata(hash).await {
        Ok(meta) => {
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_metadata_snapshot_bytes(0, 0, meta.webseeds.len()),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new()))
                }
            };
            (StatusCode::OK, Json(meta.webseeds))
        }
        Err(_) if exists => (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new())),
        Err(_) => (StatusCode::NOT_FOUND, Json(Vec::<String>::new())),
    }
}

/// `GET /api/qb/v2/torrents/pieceStates`.
pub async fn torrents_piece_states(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash.filter(|hash| !hash.trim().is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(Vec::<i32>::new()));
    };
    let entry = {
        let reg = state.registry.read().await;
        reg.get(&hash)
    };
    let Some(_) = entry else {
        return (StatusCode::NOT_FOUND, Json(Vec::<i32>::new()));
    };
    let Some(engine) = &state.engine else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<i32>::new()));
    };
    match engine.torrent_metadata(hash).await {
        Ok(meta) => {
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_metadata_snapshot_bytes(0, meta.piece_states.len(), 0),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<i32>::new()))
                }
            };
            let states = meta
                .piece_states
                .into_iter()
                .map(|state| match state {
                    EnginePieceState::Missing => 0,
                    EnginePieceState::Partial => 1,
                    EnginePieceState::Complete => 2,
                })
                .collect();
            (StatusCode::OK, Json(states))
        }
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<i32>::new())),
    }
}

/// `GET /api/qb/v2/torrents/pieceHashes`.
pub async fn torrents_piece_hashes(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash.filter(|hash| !hash.trim().is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(Vec::<String>::new()));
    };
    let exists = {
        let reg = state.registry.read().await;
        reg.get(&hash).is_some()
    };
    let Some(engine) = &state.engine else {
        return if exists {
            (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new()))
        } else {
            (StatusCode::NOT_FOUND, Json(Vec::<String>::new()))
        };
    };
    match engine.torrent_metadata(hash).await {
        Ok(meta) => {
            let _lease = match reserve_qbit_api_snapshot(
                &state,
                estimate_qbit_metadata_snapshot_bytes(0, meta.piece_hashes.len(), 0),
            )
            .await
            {
                Ok(Some(lease)) => Some(lease),
                Ok(None) | Err(_) => {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new()))
                }
            };
            (StatusCode::OK, Json(meta.piece_hashes))
        }
        Err(_) if exists => (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new())),
        Err(_) => (StatusCode::NOT_FOUND, Json(Vec::<String>::new())),
    }
}

/// `GET /api/qb/v2/torrents/export`.
pub async fn torrents_export(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> Response {
    let Some(hash) = q.hash else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let exists = {
        let reg = state.registry.read().await;
        reg.get(&hash).is_some()
    };
    if !exists {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(engine) = &state.engine else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match engine.torrent_blob(hash).await {
        Ok(raw) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/x-bittorrent")],
            raw,
        )
            .into_response(),
        Err(error) => qbit_engine_error_status(error).into_response(),
    }
}

/// `GET /api/qb/v2/torrents/properties`.
pub async fn torrents_properties(
    State(state): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return (
            StatusCode::BAD_REQUEST,
            Json(default_torrent_properties(String::new())),
        );
    };

    let entry = {
        let reg = state.registry.read().await;
        reg.get(&hash)
    };
    let Some(entry) = entry else {
        return (
            StatusCode::NOT_FOUND,
            Json(default_torrent_properties(String::new())),
        );
    };
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(&state, estimate_qbit_properties_snapshot_bytes()).await {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(default_torrent_properties(String::new())),
                )
            }
        }
    } else {
        None
    };

    let meta = if let Some(engine) = &state.engine {
        match engine.torrent_metadata(hash.clone()).await {
            Ok(meta) => Some(meta),
            Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(default_torrent_properties(String::new())),
                )
            }
        }
    } else {
        None
    };
    let (piece_size, pieces_num) = meta
        .as_ref()
        .map(|meta| (qbit_i64(meta.piece_length), qbit_usize(meta.piece_count)))
        .unwrap_or((0, 0));

    let limits = match get_torrent_limits_result(&state, &hash).await {
        Ok(limits) => limits,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(default_torrent_properties(String::new())),
            )
        }
    };
    let swarm = if let Some(engine) = &state.engine {
        match engine.torrent_peers(hash.clone()).await {
            Ok(peers) => qbit_swarm_from_peers(&peers),
            Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(default_torrent_properties(String::new())),
                )
            }
        }
    } else {
        QbitSwarmProjection::default()
    };
    let now = unix_now();
    let time_elapsed = now.saturating_sub(qbit_i64(entry.added_at));
    let seeding_time = entry
        .completed_at
        .map(|completed| now.saturating_sub(qbit_i64(completed)))
        .unwrap_or(0);
    let dl_speed = swarm.download_rate;
    let up_speed = swarm.upload_rate;
    let eta = if entry.amount_left == 0 {
        0
    } else if dl_speed > 0 {
        (qbit_i64(entry.amount_left) / dl_speed).max(0)
    } else {
        -1
    };
    let pieces_have = pieces_have(
        entry.total_length,
        entry.amount_left,
        piece_size,
        pieces_num,
    );
    let props = QbTorrentProperties {
        save_path: format!("{}/", entry.save_path.trim_end_matches('/')),
        creation_date: meta
            .as_ref()
            .and_then(|meta| meta.creation_date)
            .unwrap_or(qbit_i64(entry.added_at)),
        piece_size,
        comment: meta
            .as_ref()
            .and_then(|meta| meta.comment.clone())
            .unwrap_or_default(),
        total_wasted: 0,
        total_uploaded: qbit_i64(entry.stats.uploaded),
        total_uploaded_session: qbit_i64(entry.stats.uploaded),
        total_downloaded: qbit_i64(entry.stats.downloaded),
        total_downloaded_session: qbit_i64(entry.stats.downloaded),
        up_limit: limits.upload_limit.unwrap_or(-1),
        dl_limit: limits.download_limit.unwrap_or(-1),
        time_elapsed,
        seeding_time,
        nb_connections: qbit_i64(swarm.seeds as u64 + swarm.leechers as u64),
        nb_connections_limit: limits.max_connections.unwrap_or(-1),
        share_ratio: entry.stats.ratio(),
        addition_date: qbit_i64(entry.added_at),
        completion_date: entry.completed_at.map(qbit_i64).unwrap_or(-1),
        created_by: meta
            .as_ref()
            .and_then(|meta| meta.created_by.clone())
            .unwrap_or_default(),
        dl_speed_avg: 0,
        dl_speed,
        eta,
        last_seen: -1,
        peers: qbit_i64(swarm.leechers as u64),
        peers_total: qbit_i64(swarm.seeds as u64 + swarm.leechers as u64),
        pieces_have,
        pieces_num,
        reannounce: -1,
        seeds: qbit_i64(swarm.seeds as u64),
        seeds_total: qbit_i64(swarm.seeds as u64),
        total_size: qbit_i64(entry.total_length),
        up_speed_avg: 0,
        up_speed,
    };
    (StatusCode::OK, Json(props))
}

/// `GET /api/qb/v2/torrents/categories`.
pub async fn torrents_categories(State(state): State<AppState>) -> impl IntoResponse {
    let mut categories = serde_json::Map::new();
    let stored = match category_definitions(&state).await {
        Ok(stored) => stored,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::Value::Object(serde_json::Map::new())),
            )
        }
    };
    for (category, save_path) in stored {
        let info = QbCategoryInfo {
            name: category.clone(),
            save_path: format!("{}/", save_path.trim_end_matches('/')),
        };
        categories.insert(category, serde_json::to_value(info).unwrap());
    }
    let snapshot = match state.torrent_snapshot(None).await {
        Ok(snapshot) => snapshot,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::Value::Object(serde_json::Map::new())),
            )
        }
    };
    for facet in snapshot.category_facets() {
        if facet.name.is_empty() || categories.contains_key(&facet.name) {
            continue;
        }
        let info = QbCategoryInfo {
            name: facet.name.clone(),
            save_path: format!(
                "{}/",
                facet
                    .save_path
                    .as_deref()
                    .unwrap_or_default()
                    .trim_end_matches('/')
            ),
        };
        categories.insert(facet.name, serde_json::to_value(info).unwrap());
    }
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(
            &state,
            estimate_qbit_label_snapshot_bytes(categories.len()),
        )
        .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::Value::Object(serde_json::Map::new())),
                )
            }
        }
    } else {
        None
    };
    (StatusCode::OK, Json(serde_json::Value::Object(categories)))
}

/// `GET /api/qb/v2/torrents/tags`.
pub async fn torrents_tags(State(state): State<AppState>) -> impl IntoResponse {
    let mut tags = BTreeSet::new();
    if let Some(engine) = &state.engine {
        match engine.list_tags().await {
            Ok(global_tags) => tags.extend(global_tags),
            Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new())),
        }
    } else {
        tags.extend(state.tags.read().await.iter().cloned());
    }
    let snapshot = match state.torrent_snapshot(None).await {
        Ok(snapshot) => snapshot,
        Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::<String>::new())),
    };
    tags.extend(
        snapshot
            .tag_facets()
            .into_iter()
            .filter(|(tag, _)| !tag.is_empty())
            .map(|(tag, _)| tag),
    );
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(&state, estimate_qbit_label_snapshot_bytes(tags.len()))
            .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, Json(Vec::new())),
        }
    } else {
        None
    };
    (StatusCode::OK, Json(tags.into_iter().collect::<Vec<_>>()))
}

/// `POST /api/qb/v2/torrents/rename`.
pub async fn torrents_rename(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").and_then(|hash| normalize_api_text(hash)) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(name) = params.get("name").and_then(|name| normalize_api_text(name)) else {
        return StatusCode::BAD_REQUEST;
    };
    update_torrent_fields(&state, &hash, Some(name), None).await
}

/// `POST /api/qb/v2/torrents/renameFile`.
pub async fn torrents_rename_file(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").and_then(|hash| normalize_api_text(hash)) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(file_id) = params
        .get("id")
        .or_else(|| params.get("file_id"))
        .and_then(|id| id.parse::<u32>().ok())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(name) = params
        .get("name")
        .or_else(|| params.get("newName"))
        .and_then(|name| normalize_api_text(name))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(engine) = &state.engine else {
        return StatusCode::NOT_IMPLEMENTED;
    };
    match engine.rename_file_path(hash, file_id, name).await {
        Ok(()) => StatusCode::OK,
        Err(error) => qbit_engine_error_status(error),
    }
}

/// `POST /api/qb/v2/torrents/renameFolder`.
pub async fn torrents_rename_folder(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(hash) = params.get("hash").and_then(|hash| normalize_api_text(hash)) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(old_path) = params
        .get("oldPath")
        .or_else(|| params.get("old_path"))
        .and_then(|path| normalize_api_text(path))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_path) = params
        .get("newPath")
        .or_else(|| params.get("new_path"))
        .and_then(|path| normalize_api_text(path))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(engine) = &state.engine else {
        return StatusCode::NOT_IMPLEMENTED;
    };
    match engine.rename_folder_path(hash, old_path, new_path).await {
        Ok(()) => StatusCode::OK,
        Err(error) => qbit_engine_error_status(error),
    }
}

/// `POST /api/qb/v2/torrents/setLocation`.
pub async fn torrents_set_location(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let Some(location) = params
        .get("location")
        .and_then(|location| normalize_api_text(location))
    else {
        return StatusCode::BAD_REQUEST;
    };
    for hash in hashes {
        let status = update_torrent_fields(
            &state,
            &hash,
            None,
            Some(std::path::PathBuf::from(location.clone())),
        )
        .await;
        if status != StatusCode::OK {
            return status;
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setSavePath`.
pub async fn torrents_set_save_path(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    torrents_set_location(State(state), body).await
}

/// `POST /api/qb/v2/torrents/createCategory`.
pub async fn torrents_create_category(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(category) = params
        .get("category")
        .and_then(|category| normalize_api_text(category))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let save_path = params
        .get("savePath")
        .and_then(|save_path| normalize_api_text(save_path))
        .unwrap_or_default();
    if let Some(engine) = &state.engine {
        if let Err(error) = engine
            .create_category(
                category.clone(),
                (!save_path.is_empty()).then_some(save_path.clone()),
            )
            .await
        {
            return qbit_engine_error_status(error);
        }
    }
    state.categories.write().await.insert(category, save_path);
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/editCategory`.
pub async fn torrents_edit_category(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(category) = params
        .get("category")
        .and_then(|category| normalize_api_text(category))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_category) = params
        .get("newCategory")
        .and_then(|category| normalize_api_text(category))
    else {
        return StatusCode::BAD_REQUEST;
    };
    let save_path = params
        .get("savePath")
        .and_then(|save_path| normalize_api_text(save_path));
    if let Some(engine) = &state.engine {
        if let Err(error) = engine
            .rename_category(category.clone(), new_category.clone(), save_path.clone())
            .await
        {
            return qbit_engine_error_status(error);
        }
    } else {
        let hashes = {
            let reg = state.registry.read().await;
            reg.iter()
                .filter(|entry| entry.category.as_deref() == Some(category.as_str()))
                .map(|entry| entry.info_hash.clone())
                .collect::<Vec<_>>()
        };
        for hash in hashes {
            let status = update_torrent_category(&state, &hash, Some(new_category.clone())).await;
            if status != StatusCode::OK {
                return status;
            }
        }
    }
    let mut categories = state.categories.write().await;
    if let Some(old_save_path) = categories.remove(&category) {
        categories.insert(new_category, save_path.unwrap_or(old_save_path));
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/removeCategories`.
pub async fn torrents_remove_categories(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let categories = match required_text_list(&params, "categories") {
        Ok(categories) => categories,
        Err(status) => return status,
    };
    if let Some(engine) = &state.engine {
        if let Err(error) = engine.remove_categories(categories.clone()).await {
            return qbit_engine_error_status(error);
        }
    } else {
        let hashes = {
            let reg = state.registry.read().await;
            reg.iter()
                .filter(|entry| {
                    entry
                        .category
                        .as_ref()
                        .is_some_and(|category| categories.contains(category))
                })
                .map(|entry| entry.info_hash.clone())
                .collect::<Vec<_>>()
        };
        for hash in hashes {
            let status = update_torrent_category(&state, &hash, None).await;
            if status != StatusCode::OK {
                return status;
            }
        }
    }
    let mut stored = state.categories.write().await;
    for category in categories {
        stored.remove(&category);
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/createTags`.
pub async fn torrents_create_tags(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let tags = match required_strict_tag_list(&params, "tags", false) {
        Ok(tags) => tags,
        Err(status) => return status,
    };
    if let Some(engine) = &state.engine {
        if let Err(error) = engine.create_tags(tags).await {
            return qbit_engine_error_status(error);
        }
    } else {
        state.tags.write().await.extend(tags);
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/deleteTags`.
pub async fn torrents_delete_tags(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let remove_tags = match required_strict_tag_list(&params, "tags", false) {
        Ok(tags) => tags,
        Err(status) => return status,
    };
    if let Some(engine) = &state.engine {
        if let Err(error) = engine.remove_tags(remove_tags.clone()).await {
            return qbit_engine_error_status(error);
        }
    } else {
        let hashes = {
            let reg = state.registry.read().await;
            reg.iter()
                .filter(|entry| entry.tags.iter().any(|tag| remove_tags.contains(tag)))
                .map(|entry| entry.info_hash.clone())
                .collect::<Vec<_>>()
        };
        for hash in hashes {
            let status = update_torrent_tags(&state, &hash, Vec::new(), remove_tags.clone()).await;
            if status != StatusCode::OK {
                return status;
            }
        }
    }
    let mut stored = state.tags.write().await;
    for tag in remove_tags {
        stored.remove(&tag);
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setCategory`.
pub async fn torrents_set_category(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let Some(category) = params.get("category").cloned() else {
        return StatusCode::BAD_REQUEST;
    };
    let category_save_path = if category.trim().is_empty() {
        None
    } else {
        let categories = match category_definitions(&state).await {
            Ok(categories) => categories,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
        };
        categories.get(&category).cloned()
    };
    if let Some(engine) = &state.engine {
        for hash in &hashes {
            let category = if category.trim().is_empty() {
                None
            } else {
                Some(category.clone())
            };
            if let Err(error) = engine
                .update_torrent_labels(hash.clone(), Some(category), Vec::new(), Vec::new())
                .await
            {
                return qbit_engine_error_status(error);
            }
            if let Some(save_path) = &category_save_path {
                if let Err(error) = engine
                    .update_torrent_fields(
                        hash.clone(),
                        None,
                        Some(std::path::PathBuf::from(save_path)),
                    )
                    .await
                {
                    return qbit_engine_error_status(error);
                }
            }
        }
    } else {
        let mut reg = state.registry.write().await;
        for hash in &hashes {
            let Some(mut e) = reg.get_mut(hash) else {
                return StatusCode::NOT_FOUND;
            };
            e.category = if category.is_empty() {
                None
            } else {
                Some(category.clone())
            };
            if let Some(save_path) = &category_save_path {
                e.save_path = save_path.clone();
            }
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/addTags`.
pub async fn torrents_add_tags(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let new_tags = match required_strict_tag_list(&params, "tags", false) {
        Ok(tags) => tags,
        Err(status) => return status,
    };
    if let Some(engine) = &state.engine {
        for hash in &hashes {
            if let Err(error) = engine
                .update_torrent_labels(hash.clone(), None, new_tags.clone(), Vec::new())
                .await
            {
                return qbit_engine_error_status(error);
            }
        }
    } else {
        let mut reg = state.registry.write().await;
        for hash in &hashes {
            let Some(mut e) = reg.get_mut(hash) else {
                return StatusCode::NOT_FOUND;
            };
            for tag in &new_tags {
                if !e.tags.contains(tag) {
                    e.tags.push(tag.clone());
                }
            }
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setTags`.
pub async fn torrents_set_tags(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let new_tags = match required_strict_tag_list(&params, "tags", true) {
        Ok(tags) => tags,
        Err(status) => return status,
    };
    for hash in hashes {
        let old_tags = {
            let reg = state.registry.read().await;
            reg.get(&hash)
                .map(|entry| entry.tags.clone())
                .unwrap_or_default()
        };
        let status = update_torrent_tags(&state, &hash, new_tags.clone(), old_tags).await;
        if status != StatusCode::OK {
            return status;
        }
    }
    state.tags.write().await.extend(new_tags);
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/removeTags`.
pub async fn torrents_remove_tags(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let remove_tags = match required_strict_tag_list(&params, "tags", false) {
        Ok(tags) => tags,
        Err(status) => return status,
    };
    if let Some(engine) = &state.engine {
        for hash in &hashes {
            if let Err(error) = engine
                .update_torrent_labels(hash.clone(), None, Vec::new(), remove_tags.clone())
                .await
            {
                return qbit_engine_error_status(error);
            }
        }
    } else {
        let mut reg = state.registry.write().await;
        for hash in &hashes {
            let Some(mut e) = reg.get_mut(hash) else {
                return StatusCode::NOT_FOUND;
            };
            e.tags.retain(|tag| !remove_tags.contains(tag));
        }
    }
    let still_used = {
        let reg = state.registry.read().await;
        remove_tags
            .iter()
            .filter(|tag| {
                reg.iter()
                    .any(|entry| entry.tags.iter().any(|used| used == *tag))
            })
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    };
    let mut global = state.tags.write().await;
    for tag in remove_tags {
        if !still_used.contains(&tag) {
            global.remove(&tag);
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setDownloadLimit`.
pub async fn torrents_set_download_limit(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_limit_field(State(state), body, LimitField::Download).await
}

/// `POST /api/qb/v2/torrents/setUploadLimit`.
pub async fn torrents_set_upload_limit(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_limit_field(State(state), body, LimitField::Upload).await
}

/// `GET /api/qb/v2/torrents/downloadLimit`.
pub async fn torrents_download_limit(
    State(state): State<AppState>,
    Query(q): Query<HashesQuery>,
) -> impl IntoResponse {
    torrent_limit_map(&state, q.hashes, LimitField::Download).await
}

/// `GET /api/qb/v2/torrents/uploadLimit`.
pub async fn torrents_upload_limit(
    State(state): State<AppState>,
    Query(q): Query<HashesQuery>,
) -> impl IntoResponse {
    torrent_limit_map(&state, q.hashes, LimitField::Upload).await
}

/// `POST /api/qb/v2/torrents/setShareLimits`.
pub async fn torrents_set_share_limits(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let ratio = match params.get("ratioLimit") {
        Some(value) => match value.parse::<f64>() {
            Ok(value) if value.is_finite() && (value >= 0.0 || value == -1.0 || value == -2.0) => {
                (value >= 0.0).then_some(value)
            }
            _ => return StatusCode::BAD_REQUEST,
        },
        None => None,
    };
    let seeding_time = match params.get("seedingTimeLimit") {
        Some(value) => match value.parse::<i64>() {
            Ok(value) if value >= 0 => Some(value),
            Ok(-1 | -2) => None,
            _ => return StatusCode::BAD_REQUEST,
        },
        None => None,
    };
    if !params.contains_key("ratioLimit") && !params.contains_key("seedingTimeLimit") {
        return StatusCode::BAD_REQUEST;
    }
    for hash in hashes {
        let mut limits = match get_torrent_limits_result(&state, &hash).await {
            Ok(limits) => limits,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
        };
        limits.seed_ratio_limit = ratio;
        limits.seed_idle_limit = seeding_time;
        let status = update_torrent_limits(&state, &hash, limits).await;
        if status != StatusCode::OK {
            return status;
        }
    }
    StatusCode::OK
}

/// `POST /api/qb/v2/torrents/setForceStart`.
pub async fn torrents_set_force_start(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::ForceStart).await
}

/// `POST /api/qb/v2/torrents/setSuperSeeding`.
pub async fn torrents_set_super_seeding(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::SuperSeeding).await
}

/// `POST /api/qb/v2/torrents/setAutoTMM`.
pub async fn torrents_set_auto_tmm(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::AutoTmm).await
}

/// `POST /api/qb/v2/torrents/setAutoManagement`.
pub async fn torrents_set_auto_management(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::AutoManagement).await
}

/// `POST /api/qb/v2/torrents/toggleSequentialDownload`.
pub async fn torrents_toggle_sequential_download(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::Sequential).await
}

/// `POST /api/qb/v2/torrents/toggleFirstLastPiecePrio`.
pub async fn torrents_toggle_first_last_piece_prio(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_bool_limit_field(State(state), body, BoolLimitField::FirstLast).await
}

/// `POST /api/qb/v2/transfer/setDownloadLimit`.
pub async fn transfer_set_download_limit(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_global_limit(&state, &body, LimitField::Download).await
}

/// `POST /api/qb/v2/transfer/setUploadLimit`.
pub async fn transfer_set_upload_limit(
    State(state): State<AppState>,
    body: String,
) -> impl IntoResponse {
    update_global_limit(&state, &body, LimitField::Upload).await
}

pub async fn transfer_speed_limits_mode(State(state): State<AppState>) -> impl IntoResponse {
    match global_limits_result(&state).await {
        Ok(limits) => (
            StatusCode::OK,
            if limits.speed_limits_mode { "1" } else { "0" },
        )
            .into_response(),
        Err(error) => qbit_backend_unavailable(&error),
    }
}

pub async fn transfer_toggle_speed_limits_mode(State(state): State<AppState>) -> impl IntoResponse {
    let mut limits = match global_limits_result(&state).await {
        Ok(limits) => limits,
        Err(error) => return qbit_backend_unavailable(&error),
    };
    limits.speed_limits_mode = !limits.speed_limits_mode;
    if let Some(engine) = &state.engine {
        match engine.update_global_limits(limits.clone()).await {
            Ok(()) => {}
            Err(error) => return qbit_engine_error_status(error).into_response(),
        }
    }
    *state.global_limits.write().await = limits;
    StatusCode::OK.into_response()
}

/// `GET /api/qb/v2/transfer/downloadLimit`.
pub async fn transfer_download_limit(State(state): State<AppState>) -> impl IntoResponse {
    match global_limits_result(&state).await {
        Ok(limits) => (StatusCode::OK, limits.download_limit.to_string()).into_response(),
        Err(error) => qbit_backend_unavailable(&error),
    }
}

/// `GET /api/qb/v2/transfer/uploadLimit`.
pub async fn transfer_upload_limit(State(state): State<AppState>) -> impl IntoResponse {
    match global_limits_result(&state).await {
        Ok(limits) => (StatusCode::OK, limits.upload_limit.to_string()).into_response(),
        Err(error) => qbit_backend_unavailable(&error),
    }
}

// ---------------------------------------------------------------------------
// Sync & Transfer
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SyncMaindataQuery {
    pub rid: Option<i64>,
}

#[derive(Debug, Serialize)]
struct SyncMaindataResponse {
    rid: i64,
    full_update: bool,
    torrents: SyncTorrentMap,
    torrents_removed: Vec<String>,
    server_state: QbServerState,
}

#[derive(Debug)]
struct SyncTorrentMap {
    infos: Vec<QbTorrentInfo>,
}

impl Serialize for SyncTorrentMap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.infos.len()))?;
        for info in &self.infos {
            map.serialize_entry(&info.hash, info)?;
        }
        map.end()
    }
}

pub async fn sync_maindata(
    State(state): State<AppState>,
    Query(q): Query<SyncMaindataQuery>,
) -> impl IntoResponse {
    let (current_revision, requested_revision) = {
        let registry = state.registry.read().await;
        (
            registry.revision(),
            q.rid.filter(|rid| *rid > 0).map(|rid| rid as u64),
        )
    };
    let unchanged_empty_registry = q
        .rid
        .is_some_and(|rid| rid > 0 && rid == qbit_registry_rid(current_revision))
        && current_revision == 0;
    let empty_entries = Arc::new(ChunkedVec::from_vec(Vec::<rt_session::TorrentEntry>::new()));
    let (revision, full_update, entries, torrents_removed) = if requested_revision
        .is_some_and(|requested| requested == current_revision)
        || unchanged_empty_registry
    {
        (current_revision, false, empty_entries, Vec::new())
    } else {
        let delta = if let Some(requested_revision) =
            requested_revision.filter(|requested| *requested <= current_revision)
        {
            let registry = state.registry.read().await;
            registry.changes_since(requested_revision).map(|changes| {
                let mut changed = HashSet::new();
                let mut removed = HashSet::new();
                for change in changes {
                    if change.removed {
                        changed.remove(&change.info_hash);
                        removed.insert(change.info_hash);
                    } else {
                        removed.remove(&change.info_hash);
                        changed.insert(change.info_hash);
                    }
                }
                let entries = changed
                    .into_iter()
                    .filter_map(|hash| registry.get(&hash))
                    .collect::<Vec<_>>();
                let mut removed = removed
                    .into_iter()
                    .filter(|hash| registry.get(hash).is_none())
                    .collect::<Vec<_>>();
                removed.sort_unstable();
                (
                    registry.revision(),
                    Arc::new(ChunkedVec::from_vec(entries)),
                    removed,
                )
            })
        } else {
            None
        };
        if let Some((revision, entries, removed)) = delta {
            (revision, false, entries, removed)
        } else {
            // Full updates use the shared snapshot cache. This remains
            // O(N) when the registry changes, but repeated qBit polling
            // no longer clones every TorrentEntry independently of the
            // native/SSE snapshot consumers.
            let snapshot = match state.torrent_snapshot(None).await {
                Ok(snapshot) => snapshot,
                Err(TorrentSnapshotError::Expired { revision }) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        format!("failed to build torrent snapshot at revision {revision}"),
                    )
                        .into_response();
                }
            };
            (snapshot.revision, true, snapshot.entries, Vec::new())
        }
    };
    let torrent_count = entries.len();
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(
            &state,
            estimate_qbit_maindata_snapshot_bytes(torrent_count),
        )
        .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) => return qbit_api_snapshot_budget_exhausted(),
            Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
        }
    } else {
        None
    };
    let active_rechecks = if entries.is_empty() {
        HashSet::new()
    } else {
        match active_recheck_hashes(&state).await {
            Ok(active_rechecks) => active_rechecks,
            Err(error) => return qbit_backend_unavailable(&error),
        }
    };
    let mut infos = Vec::with_capacity(entries.len());
    let include_live = entries.len() <= QBIT_LIVE_PROJECTION_MAX_ENTRIES;
    if include_live {
        let live_entries = entries.iter().cloned().collect();
        infos = match load_qbit_live_projections(&state, live_entries, active_rechecks).await {
            Ok(infos) => infos,
            Err(error) => return qbit_backend_unavailable(&error),
        };
    } else {
        for entry in entries.iter() {
            let info = match qbit_torrent_info(&state, entry, &active_rechecks, false).await {
                Ok(info) => info,
                Err(error) => return qbit_backend_unavailable(&error),
            };
            infos.push(info);
        }
    }
    state
        .api_metrics
        .record_estimated_response_bytes(estimate_qbit_maindata_snapshot_bytes(infos.len()));
    let rid = qbit_registry_rid(revision);
    let (alltime_dl, alltime_ul, session_rates, connected_peers, queued_io_jobs) =
        if let Some(engine) = &state.engine {
            match engine.stats().await {
                Ok(stats) => (
                    qbit_i64(stats.bytes_downloaded),
                    qbit_i64(stats.bytes_uploaded),
                    QbitSwarmProjection {
                        download_rate: stats.download_rate,
                        upload_rate: stats.upload_rate,
                        ..Default::default()
                    },
                    qbit_i64(stats.connected_peers),
                    qbit_i64(stats.storage_jobs_queue_depth),
                ),
                Err(error) => return qbit_backend_unavailable(&error),
            }
        } else {
            let (alltime_dl, alltime_ul) = infos.iter().fold((0_i64, 0_i64), |(dl, ul), info| {
                (
                    dl.saturating_add(info.downloaded),
                    ul.saturating_add(info.uploaded),
                )
            });
            (
                alltime_dl,
                alltime_ul,
                qbit_session_rates_from_infos(&infos),
                qbit_i64(
                    infos
                        .iter()
                        .map(|info| info.num_leechs as u64 + info.num_seeds as u64)
                        .sum(),
                ),
                0,
            )
        };
    let global_ratio = if alltime_dl > 0 {
        alltime_ul as f64 / alltime_dl as f64
    } else {
        0.0
    };
    let limits = match global_limits_result(&state).await {
        Ok(limits) => limits,
        Err(error) => return qbit_backend_unavailable(&error),
    };
    let free_space_on_disk = if let Some(engine) = &state.engine {
        match engine.list_storage_roots().await {
            Ok(roots) => roots
                .into_iter()
                .filter(|root| root.ok)
                .map(|root| root.available_bytes)
                .max()
                .map(qbit_i64)
                .unwrap_or(0),
            Err(error) => return qbit_backend_unavailable(&error),
        }
    } else {
        0
    };
    let resp = SyncMaindataResponse {
        rid,
        full_update,
        torrents: SyncTorrentMap { infos },
        torrents_removed,
        server_state: QbServerState {
            dl_info_speed: session_rates.download_rate,
            dl_info_data: alltime_dl,
            up_info_speed: session_rates.upload_rate,
            up_info_data: alltime_ul,
            alltime_dl,
            alltime_ul,
            average_time_queue: 0,
            connection_status: "connected".into(),
            free_space_on_disk,
            global_ratio,
            queued_io_jobs,
            queueing: false,
            read_cache_hits: "0".into(),
            read_cache_overload: "0".into(),
            refresh_interval: 1500,
            total_buffers_size: 0,
            total_peer_connections: connected_peers,
            total_queued_size: 0,
            total_wasted_session: 0,
            dl_rate_limit: limits.download_limit,
            up_rate_limit: limits.upload_limit,
            use_alt_speed_limits: limits.speed_limits_mode,
            write_cache_overload: "0".into(),
        },
    };
    (StatusCode::OK, Json(resp)).into_response()
}

fn qbit_registry_rid(revision: u64) -> i64 {
    // qBittorrent clients use zero as "no previous response". Keep the
    // empty registry distinguishable from that sentinel.
    (revision.min(i64::MAX as u64) as i64).max(1)
}

pub async fn sync_torrent_peers(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(hash) = q
        .get("hash")
        .filter(|hash| !hash.trim().is_empty())
        .cloned()
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "code": "BAD_REQUEST",
                    "message": "hash is required",
                }
            })),
        );
    };
    let peers = if let Some(engine) = &state.engine {
        match engine.torrent_peers(hash).await {
            Ok(peers) => peers,
            Err(error) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "rid": 1,
                        "full_update": true,
                        "peers": {},
                        "peers_removed": [],
                        "show_flags": true,
                        "error": {
                            "code": "SERVICE_UNAVAILABLE",
                            "message": error,
                        },
                    })),
                );
            }
        }
    } else {
        Vec::new()
    };
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(&state, estimate_qbit_peer_snapshot_bytes(peers.len()))
            .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "rid": 1,
                        "full_update": true,
                        "peers": {},
                        "peers_removed": [],
                        "show_flags": true,
                    })),
                )
            }
        }
    } else {
        None
    };
    let rid = qbit_peer_rid(&peers);
    let full_update = q
        .get("rid")
        .and_then(|rid| rid.parse::<i64>().ok())
        .map(|requested| requested != rid)
        .unwrap_or(true);
    let peer_map = if full_update {
        qbit_peer_map(&peers)
    } else {
        serde_json::Map::new()
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "rid": rid,
            "full_update": full_update,
            "peers": peer_map,
            "peers_removed": [],
            "show_flags": true,
        })),
    )
}

fn qbit_peer_rid(peers: &[EnginePeerSnapshot]) -> i64 {
    let mut hasher = DefaultHasher::new();
    let mut peers = peers.iter().collect::<Vec<_>>();
    peers.sort_by_key(|peer| peer.addr);
    for peer in peers {
        peer.addr.hash(&mut hasher);
        peer.client.hash(&mut hasher);
        peer.download_rate.hash(&mut hasher);
        peer.upload_rate.hash(&mut hasher);
        peer.downloaded.hash(&mut hasher);
        peer.uploaded.hash(&mut hasher);
        ((peer.progress * 1_000_000.0).round() as i64).hash(&mut hasher);
    }
    (hasher.finish() & 0x7fff_ffff) as i64 + 1
}

fn qbit_peer_map(peers: &[EnginePeerSnapshot]) -> serde_json::Map<String, serde_json::Value> {
    peers
        .iter()
        .map(|peer| {
            let key = peer.addr.to_string();
            let value = serde_json::json!({
                "client": peer.client,
                "connection": "BT",
                "country": "",
                "country_code": "",
                "dl_speed": peer.download_rate,
                "downloaded": peer.downloaded,
                "files": "",
                "flags": "",
                "flags_desc": "",
                "ip": peer.addr.ip().to_string(),
                "peer_id_client": peer.client,
                "port": peer.addr.port(),
                "progress": peer.progress,
                "relevance": peer.progress,
                "up_speed": peer.upload_rate,
                "uploaded": peer.uploaded,
            });
            (key, value)
        })
        .collect()
}

pub async fn transfer_info(State(state): State<AppState>) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return qbit_backend_unavailable("native engine is unavailable");
    };
    if !engine.is_alive() {
        return qbit_backend_unavailable("native engine is not alive");
    }
    let engine_stats = match engine.stats().await {
        Ok(stats) => stats,
        Err(error) => {
            return qbit_backend_unavailable(&format!("engine stats unavailable: {error}"))
        }
    };
    let limits = match global_limits_result(&state).await {
        Ok(limits) => limits,
        Err(error) => return qbit_backend_unavailable(&error),
    };
    let free_space = match engine.list_storage_roots().await {
        Ok(roots) => roots
            .into_iter()
            .filter(|root| root.ok)
            .map(|root| root.available_bytes)
            .max()
            .and_then(|bytes| i64::try_from(bytes).ok()),
        Err(_) => None,
    };
    let Some(free_space) = free_space else {
        return qbit_backend_unavailable("no healthy storage root is available");
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "dl_info_speed": engine_stats.download_rate,
            "dl_info_data": engine_stats.bytes_downloaded.min(i64::MAX as u64) as i64,
            "up_info_speed": engine_stats.upload_rate,
            "up_info_data": engine_stats.bytes_uploaded.min(i64::MAX as u64) as i64,
            "connection_status": "connected",
            "free_space_on_disk": free_space,
            "dl_rate_limit": limits.download_limit,
            "up_rate_limit": limits.upload_limit,
            "use_alt_speed_limits": limits.speed_limits_mode,
        })),
    )
        .into_response()
}

fn qbit_backend_unavailable(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": {
                "code": "SERVICE_UNAVAILABLE",
                "message": message,
            }
        })),
    )
        .into_response()
}

pub async fn transfer_ban_peers(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let peers = match params
        .get("peers")
        .and_then(|peers| strict_peer_addrs(peers).ok())
    {
        Some(peers) if !peers.is_empty() => peers,
        _ => return StatusCode::BAD_REQUEST,
    };
    let Some(engine) = &state.engine else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    if let Err(error) = engine.ban_peers(peers.clone()).await {
        return qbit_engine_error_status(error);
    }
    let Ok(current) = engine.banned_peers().await else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    // The engine may reject entries once its bounded durable policy set is
    // full. Keep the compatibility facade aligned with that authoritative
    // admission result instead of claiming every requested peer was banned.
    *state.banned_peers.write().await = current.into_iter().collect();
    StatusCode::OK
}

#[derive(Debug, Deserialize)]
pub struct LogMainQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    last_known_id: Option<i64>,
    #[serde(default)]
    normal: Option<bool>,
    #[serde(default)]
    info: Option<bool>,
    #[serde(default)]
    warning: Option<bool>,
    #[serde(default)]
    critical: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct QbLogEntry {
    id: i64,
    message: String,
    timestamp: i64,
    #[serde(rename = "type")]
    kind: i64,
}

pub async fn log_main(
    State(state): State<AppState>,
    Query(query): Query<LogMainQuery>,
) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(Vec::<QbLogEntry>::new()));
    };
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let levels = query.included_levels();
    match engine
        .session_events_filtered(None, None, levels, query.last_known_id, limit)
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
                        .filter(|entry| query.includes_type(entry.kind))
                        .collect(),
                ),
            ),
            Err(error) => {
                tracing::warn!(
                    component = "api",
                    operation = "log_main",
                    result = "error",
                    error = %error,
                    "failed to project session event"
                );
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(Vec::<QbLogEntry>::new()),
                )
            }
        },
        Err(e) => {
            tracing::warn!(
                component = "api",
                operation = "log_main",
                result = "error",
                error = %e,
                "failed to read session events"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Vec::<QbLogEntry>::new()),
            )
        }
    }
}

impl LogMainQuery {
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

    fn included_levels(&self) -> Vec<String> {
        let any_filter = self.normal.is_some()
            || self.info.is_some()
            || self.warning.is_some()
            || self.critical.is_some();
        if !any_filter {
            return Vec::new();
        }

        let mut levels = Vec::new();
        if self.normal.unwrap_or(false) || self.info.unwrap_or(false) {
            levels.push("info".to_owned());
        }
        if self.warning.unwrap_or(false) {
            levels.push("warn".to_owned());
            levels.push("warning".to_owned());
        }
        if self.critical.unwrap_or(false) {
            levels.push("error".to_owned());
            levels.push("critical".to_owned());
        }
        levels
    }
}

fn qbit_log_entry(row: rt_db::SessionEventRow) -> Result<QbLogEntry, String> {
    let event_id = row
        .event_id
        .ok_or_else(|| "session event is missing event_id".to_owned())?;
    let message = row.message.unwrap_or_else(|| row.kind.clone());
    Ok(QbLogEntry {
        id: event_id,
        message,
        timestamp: row.occurred_at,
        kind: qbit_log_type(&row.kind, &row.payload)?,
    })
}

fn qbit_log_type(kind: &str, payload: &str) -> Result<i64, String> {
    let value = serde_json::from_str::<serde_json::Value>(payload)
        .map_err(|error| format!("session event payload is invalid JSON: {error}"))?;
    let lower_kind = kind.to_ascii_lowercase();
    if lower_kind.contains("error") || lower_kind.contains("failed") {
        return Ok(4);
    }
    if lower_kind.contains("warn") {
        return Ok(2);
    }
    Ok(
        match value
            .get("level")
            .and_then(|v| v.as_str())
            .map(str::to_ascii_lowercase)
        {
            Some(level) if level == "error" || level == "critical" => 4,
            Some(level) if level == "warn" || level == "warning" => 2,
            _ => 1,
        },
    )
}

pub async fn log_peers(State(state): State<AppState>) -> Response {
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(Vec::<serde_json::Value>::new())).into_response();
    };
    // The engine already owns the promoted-task index. Query that bounded
    // runtime set in parallel; dormant registry rows cannot have peers and
    // must not turn this compatibility endpoint into a full scan plus one
    // sequential actor round-trip per torrent.
    let peer_snapshots = match engine.active_torrent_peers().await {
        Ok(peer_snapshots) => peer_snapshots,
        Err(error) => return qbit_backend_unavailable(&error),
    };
    let peer_count = peer_snapshots
        .iter()
        .map(|(_, peers)| peers.len())
        .sum::<usize>();
    let _lease = match reserve_qbit_api_snapshot(
        &state,
        estimate_qbit_peer_snapshot_bytes(peer_count),
    )
    .await
    {
        Ok(Some(lease)) => Some(lease),
        Ok(None) | Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Vec::<serde_json::Value>::new()),
            )
                .into_response()
        }
    };
    let mut entries = Vec::with_capacity(peer_count);
    for (hash, peers) in peer_snapshots {
        entries.extend(
            peers
                .into_iter()
                .map(|peer| qbit_peer_log_entry(&hash, &peer)),
        );
    }
    (StatusCode::OK, Json(entries)).into_response()
}

fn qbit_peer_log_entry(info_hash: &str, peer: &EnginePeerSnapshot) -> serde_json::Value {
    serde_json::json!({
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

pub async fn search_status(State(state): State<AppState>) -> Response {
    let plugins = match load_qbit_search_plugins(&state).await {
        Ok(plugins) => plugins,
        Err(error) => return qbit_backend_error(error),
    };
    let jobs = state.search_jobs.read().await;
    let running = jobs
        .values()
        .any(|job| job.get("status").and_then(|v| v.as_str()) == Some("Running"));
    let plugins = plugins.values().cloned().collect::<Vec<_>>();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": if running { "Running" } else { "Stopped" },
            "plugins": plugins,
        })),
    )
        .into_response()
}

pub async fn search_plugins(State(state): State<AppState>) -> Response {
    let plugins = match load_qbit_search_plugins(&state).await {
        Ok(plugins) => plugins,
        Err(error) => return qbit_backend_error(error),
    };
    (
        StatusCode::OK,
        Json(plugins.values().cloned().collect::<Vec<_>>()),
    )
        .into_response()
}

pub async fn search_categories(State(state): State<AppState>) -> Response {
    let plugins = match load_qbit_search_plugins(&state).await {
        Ok(plugins) => plugins,
        Err(error) => return qbit_backend_error(error),
    };
    let mut categories = BTreeSet::new();
    for plugin in plugins.values() {
        if plugin.get("enabled").and_then(serde_json::Value::as_bool) == Some(false) {
            continue;
        }
        let Some(values) = plugin
            .get("supportedCategories")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for category in values.iter().filter_map(serde_json::Value::as_str) {
            let category = category.trim();
            if !category.is_empty() {
                categories.insert(category.to_owned());
            }
        }
    }
    (
        StatusCode::OK,
        Json(categories.into_iter().collect::<Vec<_>>()),
    )
        .into_response()
}

pub async fn search_install_plugin(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let sources = match required_strict_qbit_list(&params, "sources") {
        Ok(sources) => sources,
        Err(status) => return status.into_response(),
    };
    let _write = state.preference_write.lock().await;
    let mut plugins = match load_qbit_search_plugins(&state).await {
        Ok(plugins) => plugins,
        Err(error) => return qbit_backend_error(error),
    };
    for source in sources {
        let name = plugin_name_from_source(&source);
        plugins.insert(name.clone(), search_plugin_value(&name, &source, true));
    }
    match save_qbit_search_plugins(&state, plugins).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn search_uninstall_plugin(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let names = match required_strict_qbit_list(&params, "names") {
        Ok(names) => names,
        Err(status) => return status.into_response(),
    };
    let _write = state.preference_write.lock().await;
    let mut plugins = match load_qbit_search_plugins(&state).await {
        Ok(plugins) => plugins,
        Err(error) => return qbit_backend_error(error),
    };
    for name in names {
        plugins.remove(&name);
    }
    match save_qbit_search_plugins(&state, plugins).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn search_enable_plugin(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let enabled = match params
        .get("enable")
        .and_then(|value| parse_qbit_bool(value))
    {
        Some(enabled) => enabled,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let names = match required_strict_qbit_list(&params, "names") {
        Ok(names) => names,
        Err(status) => return status.into_response(),
    };
    let _write = state.preference_write.lock().await;
    let mut plugins = match load_qbit_search_plugins(&state).await {
        Ok(plugins) => plugins,
        Err(error) => return qbit_backend_error(error),
    };
    for name in names {
        let entry = plugins
            .entry(name.clone())
            .or_insert_with(|| search_plugin_value(&name, "", enabled));
        if let Some(map) = entry.as_object_mut() {
            map.insert("enabled".into(), enabled.into());
        }
    }
    match save_qbit_search_plugins(&state, plugins).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn search_update_plugins() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn search_start(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let mut next_id = state.next_search_id.write().await;
    let id = *next_id;
    *next_id += 1;
    drop(next_id);

    let job = serde_json::json!({
        "id": id,
        "pattern": params.get("pattern").cloned().unwrap_or_default(),
        "plugins": params.get("plugins").cloned().unwrap_or_else(|| "all".to_owned()),
        "category": params.get("category").cloned().unwrap_or_else(|| "all".to_owned()),
        "status": "Stopped",
        "total": 0,
        "results": [],
    });
    state.search_jobs.write().await.insert(id.to_string(), job);
    (StatusCode::OK, Json(serde_json::json!({ "id": id })))
}

pub async fn search_stop(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(id) = params.get("id").filter(|id| !id.is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut jobs = state.search_jobs.write().await;
    let Some(job) = jobs.get_mut(id) else {
        return StatusCode::NOT_FOUND;
    };
    if let Some(map) = job.as_object_mut() {
        map.insert("status".into(), "Stopped".into());
    }
    StatusCode::OK
}

#[derive(Debug, Deserialize)]
pub struct SearchResultsQuery {
    id: Option<i64>,
    limit: Option<usize>,
    offset: Option<usize>,
}

pub async fn search_results(
    State(state): State<AppState>,
    Query(query): Query<SearchResultsQuery>,
) -> Response {
    let jobs = state.search_jobs.read().await;
    let job = match query.id {
        Some(id) => jobs.get(&id.to_string()),
        None => jobs.iter().next_back().map(|(_, job)| job),
    };
    let Some(job) = job else {
        if query.id.is_some() {
            return StatusCode::NOT_FOUND.into_response();
        }
        return (
            StatusCode::OK,
            Json(serde_json::json!({
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
        let offset = query.offset.unwrap_or(0);
        let limit = query.limit.unwrap_or(results.len().saturating_sub(offset));
        let sliced = results
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        map.insert("results".into(), serde_json::Value::Array(sliced));
    }
    (StatusCode::OK, Json(response)).into_response()
}

pub async fn search_delete(State(state): State<AppState>, body: String) -> impl IntoResponse {
    let params = parse_form_body(&body);
    let Some(id) = params.get("id").filter(|id| !id.is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    if state.search_jobs.write().await.remove(id).is_none() {
        return StatusCode::NOT_FOUND;
    }
    StatusCode::OK
}

pub async fn rss_items(State(state): State<AppState>) -> Response {
    match load_qbit_rss_items(&state).await {
        Ok(items) => (StatusCode::OK, Json(serde_json::Value::Object(items))).into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn rss_rules(State(state): State<AppState>) -> Response {
    match load_qbit_rss_rules(&state).await {
        Ok(rules) => (StatusCode::OK, Json(serde_json::Value::Object(rules))).into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn rss_matching_articles(State(state): State<AppState>) -> Response {
    let rules = match load_qbit_rss_rules(&state).await {
        Ok(rules) => rules,
        Err(error) => return qbit_backend_error(error),
    };
    let names = rules
        .keys()
        .cloned()
        .map(serde_json::Value::String)
        .collect::<Vec<_>>();
    (StatusCode::OK, Json(names)).into_response()
}

pub async fn rss_add_folder(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let Some(path) = params.get("path").filter(|p| !p.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let _write = state.preference_write.lock().await;
    let mut items = match load_qbit_rss_items(&state).await {
        Ok(items) => items,
        Err(error) => return qbit_backend_error(error),
    };
    items.insert(
        path.clone(),
        serde_json::json!({
            "uid": path,
            "name": rss_leaf_name(path),
            "type": "folder",
            "isLoading": false,
            "hasError": false,
            "articles": [],
        }),
    );
    match save_qbit_rss_items(&state, items).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn rss_add_feed(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let Some(url) = params.get("url").filter(|u| !u.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let path = params
        .get("path")
        .filter(|p| !p.is_empty())
        .cloned()
        .unwrap_or_else(|| url.clone());
    let _write = state.preference_write.lock().await;
    let mut items = match load_qbit_rss_items(&state).await {
        Ok(items) => items,
        Err(error) => return qbit_backend_error(error),
    };
    items.insert(
        path.clone(),
        serde_json::json!({
            "uid": path,
            "name": rss_leaf_name(&path),
            "type": "feed",
            "url": url,
            "isLoading": false,
            "hasError": false,
            "articles": [],
        }),
    );
    match save_qbit_rss_items(&state, items).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn rss_remove_item(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let Some(path) = params.get("path").filter(|path| !path.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let _write = state.preference_write.lock().await;
    let mut items = match load_qbit_rss_items(&state).await {
        Ok(items) => items,
        Err(error) => return qbit_backend_error(error),
    };
    items.remove(path);
    if let Err(error) = save_qbit_rss_items(&state, items).await {
        return qbit_backend_error(error);
    }
    StatusCode::OK.into_response()
}

pub async fn rss_move_item(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let Some(item_path) = params.get("itemPath").filter(|path| !path.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(dest_path) = params.get("destPath").filter(|path| !path.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let _write = state.preference_write.lock().await;
    let mut items = match load_qbit_rss_items(&state).await {
        Ok(items) => items,
        Err(error) => return qbit_backend_error(error),
    };
    let Some(mut item) = items.remove(item_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if item_path != dest_path && items.contains_key(dest_path) {
        return StatusCode::CONFLICT.into_response();
    }
    if let Some(map) = item.as_object_mut() {
        map.insert("uid".into(), dest_path.clone().into());
        map.insert("name".into(), rss_leaf_name(dest_path).into());
    }
    items.insert(dest_path.clone(), item);
    match save_qbit_rss_items(&state, items).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn rss_mark_as_read(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let Some(item_path) = params.get("itemPath").filter(|path| !path.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let _write = state.preference_write.lock().await;
    let mut items = match load_qbit_rss_items(&state).await {
        Ok(items) => items,
        Err(error) => return qbit_backend_error(error),
    };
    let Some(item) = items.get_mut(item_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(map) = item.as_object_mut() {
        map.insert("read".into(), true.into());
    }
    if let Err(error) = save_qbit_rss_items(&state, items).await {
        return qbit_backend_error(error);
    }
    StatusCode::OK.into_response()
}

pub async fn rss_refresh_item(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let Some(item_path) = params.get("itemPath").filter(|path| !path.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let _write = state.preference_write.lock().await;
    let mut items = match load_qbit_rss_items(&state).await {
        Ok(items) => items,
        Err(error) => return qbit_backend_error(error),
    };
    let Some(item) = items.get_mut(item_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(map) = item.as_object_mut() {
        map.insert("lastBuildDate".into(), now_secs().into());
    }
    if let Err(error) = save_qbit_rss_items(&state, items).await {
        return qbit_backend_error(error);
    }
    StatusCode::OK.into_response()
}

pub async fn rss_set_rule(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let Some(name) = params
        .get("ruleName")
        .map(String::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(raw_rule) = params.get("ruleDef") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(rule) = serde_json::from_str::<serde_json::Value>(raw_rule) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let candidate = std::iter::once((name.to_owned(), rule.clone())).collect::<JsonMap>();
    if let Err(error) = validate_qbit_rss_rules(&candidate) {
        tracing::warn!(
            component = "api",
            operation = "set_rss_rule",
            result = "bad_request",
            error = %error,
            "qBittorrent RSS rule validation failed"
        );
        return StatusCode::BAD_REQUEST.into_response();
    }
    let _write = state.preference_write.lock().await;
    let mut rules = match load_qbit_rss_rules(&state).await {
        Ok(rules) => rules,
        Err(error) => return qbit_backend_error(error),
    };
    rules.insert(name.to_owned(), rule);
    match save_qbit_rss_rules(&state, rules).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn rss_rename_rule(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let Some(rule_name) = params.get("ruleName").filter(|name| !name.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(new_rule_name) = params.get("newRuleName").filter(|name| !name.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let _write = state.preference_write.lock().await;
    let mut rules = match load_qbit_rss_rules(&state).await {
        Ok(rules) => rules,
        Err(error) => return qbit_backend_error(error),
    };
    let Some(rule) = rules.remove(rule_name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if rule_name != new_rule_name && rules.contains_key(new_rule_name) {
        return StatusCode::CONFLICT.into_response();
    }
    rules.insert(new_rule_name.clone(), rule);
    match save_qbit_rss_rules(&state, rules).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => qbit_backend_error(error),
    }
}

pub async fn rss_remove_rule(State(state): State<AppState>, body: String) -> Response {
    let params = parse_form_body(&body);
    let Some(rule_name) = params.get("ruleName").filter(|name| !name.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let _write = state.preference_write.lock().await;
    let mut rules = match load_qbit_rss_rules(&state).await {
        Ok(rules) => rules,
        Err(error) => return qbit_backend_error(error),
    };
    rules.remove(rule_name);
    if let Err(error) = save_qbit_rss_rules(&state, rules).await {
        return qbit_backend_error(error);
    }
    StatusCode::OK.into_response()
}

fn search_plugin_value(name: &str, source: &str, enabled: bool) -> serde_json::Value {
    serde_json::json!({
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

fn rss_leaf_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
        .to_owned()
}

fn required_strict_qbit_list(
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

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| qbit_i64(d.as_secs()))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn required_hashes(
    params: &HashMap<String, String>,
    keys: &[&str],
) -> Result<Vec<String>, StatusCode> {
    let raw = keys.iter().find_map(|key| params.get(*key));
    raw.and_then(|raw| strict_hashes_from_str(raw))
        .ok_or(StatusCode::BAD_REQUEST)
}

async fn required_resolved_hashes(
    state: &AppState,
    params: &HashMap<String, String>,
    keys: &[&str],
) -> Result<Vec<String>, StatusCode> {
    let requested = required_hashes(params, keys)?;
    Ok(resolve_hashes(state, requested).await)
}

fn strict_hashes_from_str(raw: &str) -> Option<Vec<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw == "all" {
        return Some(vec!["all".to_owned()]);
    }
    let mut hashes = Vec::new();
    for hash in raw.split('|') {
        let hash = hash.trim();
        if hash.is_empty() || hash == "all" {
            return None;
        }
        hashes.push(hash.to_ascii_lowercase());
    }
    Some(hashes)
}

fn required_text_list(
    params: &HashMap<String, String>,
    key: &str,
) -> Result<Vec<String>, StatusCode> {
    let raw = params.get(key).ok_or(StatusCode::BAD_REQUEST)?;
    let values = raw.split('|').map(str::trim).collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| value.is_empty()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(values.into_iter().map(str::to_owned).collect())
}

fn required_strict_tag_list(
    params: &HashMap<String, String>,
    key: &str,
    allow_empty: bool,
) -> Result<Vec<String>, StatusCode> {
    let raw = params.get(key).ok_or(StatusCode::BAD_REQUEST)?;
    strict_tag_values(raw, allow_empty).map_err(|()| StatusCode::BAD_REQUEST)
}

fn strict_numeric_list(raw: &str) -> Result<Vec<u32>, ()> {
    let values = raw.split('|').collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
        return Err(());
    }
    values
        .into_iter()
        .map(|value| value.trim().parse::<u32>().map_err(|_| ()))
        .collect()
}

fn torrent_progress(total_length: u64, amount_left: u64, complete: bool) -> f64 {
    // `completed_at` is historical metadata and can survive a recheck that
    // discovers missing files/pieces. `amount_left` is the live transfer
    // invariant; never report a torrent as complete while it still has bytes
    // outstanding.
    if complete && amount_left == 0 {
        return 1.0;
    }
    if total_length == 0 {
        return 0.0;
    }
    let done = total_length.saturating_sub(amount_left);
    (done as f64 / total_length as f64).clamp(0.0, 1.0)
}

fn pieces_have(total_length: u64, amount_left: u64, piece_size: i64, pieces_num: i64) -> i64 {
    if pieces_num <= 0 || piece_size <= 0 {
        return 0;
    }
    // A stale completion timestamp must not make a partially missing torrent
    // report every piece as present.
    if amount_left == 0 {
        return pieces_num;
    }
    let done = total_length.saturating_sub(amount_left);
    let have = i64::try_from(done.div_ceil(piece_size as u64)).unwrap_or(i64::MAX);
    have.clamp(0, pieces_num)
}

fn default_torrent_properties(save_path: String) -> QbTorrentProperties {
    QbTorrentProperties {
        save_path,
        creation_date: -1,
        piece_size: 0,
        comment: String::new(),
        total_wasted: 0,
        total_uploaded: 0,
        total_uploaded_session: 0,
        total_downloaded: 0,
        total_downloaded_session: 0,
        up_limit: -1,
        dl_limit: -1,
        time_elapsed: 0,
        seeding_time: 0,
        nb_connections: 0,
        nb_connections_limit: -1,
        share_ratio: 0.0,
        addition_date: -1,
        completion_date: -1,
        created_by: String::new(),
        dl_speed_avg: 0,
        dl_speed: 0,
        eta: -1,
        last_seen: -1,
        peers: 0,
        peers_total: 0,
        pieces_have: 0,
        pieces_num: 0,
        reannounce: -1,
        seeds: 0,
        seeds_total: 0,
        total_size: 0,
        up_speed_avg: 0,
        up_speed: 0,
    }
}

fn qbit_file_infos(
    entry: &rt_session::TorrentEntry,
    meta: &EngineTorrentMetadata,
) -> Vec<QbFileInfo> {
    let completed = qbit_file_completed_bytes(entry, &meta.files);
    meta.files
        .iter()
        .zip(completed)
        .map(|(file, completed)| QbFileInfo {
            index: file.index,
            name: file.path.clone(),
            size: qbit_i64(file.length),
            priority: file.priority.clamp(0, 2) as u8,
            progress: qbit_file_progress(file, completed),
        })
        .collect()
}

fn qbit_file_completed_bytes(
    entry: &rt_session::TorrentEntry,
    files: &[EngineTorrentFile],
) -> Vec<u64> {
    let done = entry.total_length.saturating_sub(entry.amount_left);
    let mut offset = 0u64;
    files
        .iter()
        .map(|file| {
            let file_start = offset;
            offset = offset.saturating_add(file.length);
            done.saturating_sub(file_start).min(file.length)
        })
        .collect()
}

fn qbit_file_progress(file: &EngineTorrentFile, completed: u64) -> f64 {
    if file.length == 0 || !file.wanted {
        return 1.0;
    }
    ((completed as f64) / (file.length as f64)).clamp(0.0, 1.0)
}

async fn torrent_limit_map(
    state: &AppState,
    hashes: Option<String>,
    field: LimitField,
) -> (StatusCode, Json<serde_json::Value>) {
    let requested = match hashes.as_deref().map(str::trim) {
        None | Some("") => Vec::new(),
        Some(raw) => match strict_hashes_from_str(raw) {
            Some(values) => values,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::Value::Object(serde_json::Map::new())),
                );
            }
        },
    };
    let hashes = resolve_hashes(state, requested).await;
    let reg = state.registry.read().await;
    let entries = reg
        .iter()
        .filter(|entry| hashes.is_empty() || hashes.contains(&entry.info_hash))
        .map(|entry| entry.info_hash.clone())
        .collect::<Vec<_>>();
    drop(reg);
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(
            state,
            estimate_qbit_limit_map_snapshot_bytes(entries.len()),
        )
        .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::Value::Object(serde_json::Map::new())),
                )
            }
        }
    } else {
        None
    };
    let projected = match load_qbit_limit_projections(state, &entries).await {
        Ok(projected) => projected,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::Value::Object(serde_json::Map::new())),
            )
        }
    };
    let mut limits = serde_json::Map::new();
    for hash in entries {
        let Some(projected) = projected.get(&hash) else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::Value::Object(serde_json::Map::new())),
            );
        };
        let value = match field {
            LimitField::Download => projected.download_limit.unwrap_or(0),
            LimitField::Upload => projected.upload_limit.unwrap_or(0),
        };
        limits.insert(hash, serde_json::json!(value));
    }
    (StatusCode::OK, Json(serde_json::Value::Object(limits)))
}

#[derive(Debug, Clone, Copy)]
enum LimitField {
    Download,
    Upload,
}

#[derive(Debug, Clone, Copy)]
enum BoolLimitField {
    Sequential,
    FirstLast,
    ForceStart,
    SuperSeeding,
    AutoTmm,
    AutoManagement,
}

async fn update_limit_field(
    State(state): State<AppState>,
    body: String,
    field: LimitField,
) -> StatusCode {
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let limit = match params
        .get("limit")
        .and_then(|value| value.parse::<i64>().ok())
    {
        Some(value) if value >= 0 => (value > 0).then_some(value),
        _ => return StatusCode::BAD_REQUEST,
    };
    for hash in hashes {
        let mut limits = match get_torrent_limits_result(&state, &hash).await {
            Ok(limits) => limits,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
        };
        match field {
            LimitField::Download => limits.download_limit = limit,
            LimitField::Upload => limits.upload_limit = limit,
        }
        let status = update_torrent_limits(&state, &hash, limits).await;
        if status != StatusCode::OK {
            return status;
        }
    }
    StatusCode::OK
}

async fn update_global_limit(state: &AppState, body: &str, field: LimitField) -> StatusCode {
    let params = parse_form_body(body);
    let limit = match params
        .get("limit")
        .and_then(|value| value.parse::<i64>().ok())
    {
        Some(value) if value >= 0 => value,
        _ => return StatusCode::BAD_REQUEST,
    };
    let mut limits = match global_limits_result(state).await {
        Ok(limits) => limits,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
    };
    match field {
        LimitField::Download => limits.download_limit = limit,
        LimitField::Upload => limits.upload_limit = limit,
    }
    if let Some(engine) = &state.engine {
        match engine.update_global_limits(limits.clone()).await {
            Ok(()) => {}
            Err(error) => return qbit_engine_error_status(error),
        }
    }
    *state.global_limits.write().await = limits;
    StatusCode::OK
}

async fn update_bool_limit_field(
    State(state): State<AppState>,
    body: String,
    field: BoolLimitField,
) -> StatusCode {
    if state.engine.is_some()
        && matches!(
            field,
            BoolLimitField::ForceStart | BoolLimitField::AutoTmm | BoolLimitField::AutoManagement
        )
    {
        // These flags are persisted for migration/inspection, but TorrentNG
        // has no queue or automatic-save-path manager that could make them
        // true runtime operations. Returning success here would be a
        // compatibility lie and would leave clients believing their policy
        // took effect.
        return StatusCode::NOT_IMPLEMENTED;
    }
    let params = parse_form_body(&body);
    let hashes = match required_resolved_hashes(&state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let requested = match params.get("value").or_else(|| params.get("enable")) {
        Some(value) => match parse_qbit_bool(value) {
            Some(value) => Some(value),
            None => return StatusCode::BAD_REQUEST,
        },
        None => None,
    };
    for hash in hashes {
        let mut limits = match get_torrent_limits_result(&state, &hash).await {
            Ok(limits) => limits,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
        };
        let current = match field {
            BoolLimitField::Sequential => limits.sequential_download,
            BoolLimitField::FirstLast => limits.first_last_piece_prio,
            BoolLimitField::ForceStart => limits.force_start,
            BoolLimitField::SuperSeeding => limits.super_seeding,
            BoolLimitField::AutoTmm => limits.auto_tmm,
            BoolLimitField::AutoManagement => limits.auto_management,
        };
        let value = requested.unwrap_or(!current);
        match field {
            BoolLimitField::Sequential => limits.sequential_download = value,
            BoolLimitField::FirstLast => limits.first_last_piece_prio = value,
            BoolLimitField::ForceStart => limits.force_start = value,
            BoolLimitField::SuperSeeding => limits.super_seeding = value,
            BoolLimitField::AutoTmm => limits.auto_tmm = value,
            BoolLimitField::AutoManagement => limits.auto_management = value,
        }
        let status = update_torrent_limits(&state, &hash, limits).await;
        if status != StatusCode::OK {
            return status;
        }
    }
    StatusCode::OK
}

async fn global_limits_result(state: &AppState) -> Result<EngineGlobalLimits, String> {
    if let Some(engine) = &state.engine {
        let limits = engine
            .global_limits()
            .await
            .map_err(|error| error.to_string())?;
        *state.global_limits.write().await = limits.clone();
        return Ok(limits);
    }
    Ok(state.global_limits.read().await.clone())
}

async fn reserve_qbit_api_snapshot(
    state: &AppState,
    bytes: u64,
) -> Result<Option<MemoryLease>, String> {
    let Some(engine) = &state.engine else {
        return Ok(None);
    };
    engine.reserve_memory(MemoryClass::ApiSnapshot, bytes).await
}

fn qbit_api_snapshot_budget_exhausted() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "api snapshot memory budget exhausted",
    )
        .into_response()
}

fn estimate_qbit_torrent_info_snapshot_bytes(torrent_count: usize) -> u64 {
    (torrent_count as u64).saturating_mul(2048)
}

fn estimate_qbit_maindata_snapshot_bytes(torrent_count: usize) -> u64 {
    // /sync/maindata wraps torrent info in a keyed map plus server state.
    16 * 1024 + (torrent_count as u64).saturating_mul(2304)
}

fn estimate_qbit_metadata_snapshot_bytes(
    file_count: usize,
    piece_count: usize,
    webseed_count: usize,
) -> u64 {
    16 * 1024
        + (file_count as u64).saturating_mul(512)
        + (piece_count as u64).saturating_mul(96)
        + (webseed_count as u64).saturating_mul(256)
}

fn estimate_qbit_tracker_snapshot_bytes(tracker_count: usize) -> u64 {
    8 * 1024 + (tracker_count as u64).saturating_mul(512)
}

fn qbit_trackers_from_snapshots(trackers: &[EngineTrackerSnapshot]) -> Vec<QbTrackerInfo> {
    trackers
        .iter()
        .map(|tracker| QbTrackerInfo {
            url: tracker.announce.clone(),
            status: qbit_tracker_status_code(&tracker.status),
            tier: qbit_i32(tracker.tier),
            num_peers: qbit_i32(
                tracker
                    .seeders
                    .unwrap_or(0)
                    .saturating_add(tracker.leechers.unwrap_or(0)),
            ),
            num_seeds: qbit_i32(tracker.seeders.unwrap_or(-1)),
            num_leeches: qbit_i32(tracker.leechers.unwrap_or(-1)),
            num_downloaded: qbit_i32(tracker.completed.unwrap_or(-1)),
            msg: qbit_tracker_message(tracker),
        })
        .collect()
}

fn qbit_tracker_status_code(status: &str) -> i32 {
    match status {
        "working" => 2,
        "warning" => 3,
        "error" => 4,
        "pending" => 1,
        _ => 0,
    }
}

fn qbit_tracker_message(tracker: &EngineTrackerSnapshot) -> String {
    tracker
        .failure_reason
        .clone()
        .filter(|message| !message.is_empty())
        .or_else(|| {
            tracker
                .warning_message
                .clone()
                .filter(|message| !message.is_empty())
        })
        .unwrap_or_default()
}

fn estimate_qbit_label_snapshot_bytes(item_count: usize) -> u64 {
    8 * 1024 + (item_count as u64).saturating_mul(256)
}

fn estimate_qbit_limit_map_snapshot_bytes(torrent_count: usize) -> u64 {
    8 * 1024 + (torrent_count as u64).saturating_mul(192)
}

fn estimate_qbit_properties_snapshot_bytes() -> u64 {
    32 * 1024
}

fn estimate_qbit_peer_snapshot_bytes(peer_count: usize) -> u64 {
    8 * 1024 + (peer_count as u64).saturating_mul(1024)
}

fn qbit_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn qbit_i32(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

fn qbit_usize(value: usize) -> i64 {
    qbit_i64(value as u64)
}

async fn queue_priority(state: &AppState, hash: &str) -> Result<i32, String> {
    let Some(engine) = &state.engine else {
        return Ok(0);
    };
    engine
        .queue_priority(hash.to_owned())
        .await
        .map_err(|error| error.to_string())
}

async fn update_queue_order(state: &AppState, body: &str, queue_move: QueueMove) -> StatusCode {
    let params = parse_form_body(body);
    let hashes = match required_resolved_hashes(state, &params, &["hashes"]).await {
        Ok(hashes) => hashes,
        Err(status) => return status,
    };
    let Some(engine) = &state.engine else {
        return StatusCode::NOT_IMPLEMENTED;
    };
    match engine.update_queue_order(hashes, queue_move).await {
        Ok(()) => StatusCode::OK,
        Err(error) => qbit_engine_error_status(error),
    }
}

async fn get_torrent_limits_result(
    state: &AppState,
    hash: &str,
) -> Result<EngineTorrentLimits, String> {
    if let Some(engine) = &state.engine {
        return engine
            .torrent_limits(hash.to_owned())
            .await
            .map_err(|error| error.to_string());
    }
    let canonical_hash = state
        .registry
        .read()
        .await
        .get(hash)
        .map(|entry| entry.info_hash.clone())
        .unwrap_or_else(|| hash.to_owned());
    Ok(state
        .torrent_limits
        .read()
        .await
        .get(&canonical_hash)
        .cloned()
        .unwrap_or_default())
}

/// Read-only qBittorrent limit maps are full-list compatibility endpoints.
/// Fetch their per-torrent engine projections in bounded batches so one
/// client request does not serialize thousands of actor round trips.
async fn load_qbit_limit_projections(
    state: &AppState,
    hashes: &[String],
) -> Result<HashMap<String, EngineTorrentLimits>, String> {
    let mut result = HashMap::with_capacity(hashes.len());
    for batch in hashes.chunks(QBIT_LIMIT_PROJECTION_CONCURRENCY) {
        let mut tasks = JoinSet::new();
        for hash in batch {
            let state = state.clone();
            let hash = hash.clone();
            tasks.spawn(async move {
                let limits = get_torrent_limits_result(&state, &hash).await?;
                Ok::<_, String>((hash, limits))
            });
        }
        while let Some(task) = tasks.join_next().await {
            let (hash, limits) = task
                .map_err(|error| format!("qBittorrent limit projection task failed: {error}"))??;
            result.insert(hash, limits);
        }
    }
    Ok(result)
}

async fn update_torrent_limits(
    state: &AppState,
    hash: &str,
    limits: EngineTorrentLimits,
) -> StatusCode {
    if let Some(engine) = &state.engine {
        return match engine.update_torrent_limits(hash.to_owned(), limits).await {
            Ok(()) => StatusCode::OK,
            Err(error) => qbit_engine_error_status(error),
        };
    }
    let canonical_hash = {
        let reg = state.registry.read().await;
        let Some(entry) = reg.get(hash) else {
            return StatusCode::NOT_FOUND;
        };
        entry.info_hash.clone()
    };
    state
        .torrent_limits
        .write()
        .await
        .insert(canonical_hash, limits);
    StatusCode::OK
}

fn parse_qbit_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

async fn active_recheck_hashes(state: &AppState) -> Result<HashSet<String>, String> {
    let Some(engine) = &state.engine else {
        return Ok(HashSet::new());
    };
    let jobs = engine.list_jobs().await?;
    let hashes = jobs
        .into_iter()
        .filter(|job| {
            job.kind == "recheck_torrent"
                && !matches!(
                    job.state.as_str(),
                    "completed" | "failed" | "cancelled" | "canceled"
                )
        })
        .flat_map(|job| job.affected_torrents)
        .collect::<Vec<_>>();
    let registry = state.registry.read().await;
    Ok(hashes
        .into_iter()
        .filter_map(|hash| registry.get(&hash).map(|entry| entry.info_hash.clone()))
        .collect())
}

fn qbit_state_with_recheck(entry_state: &str, active_recheck: bool) -> String {
    if active_recheck {
        "checkingDL".to_owned()
    } else {
        to_qbit_state(entry_state).to_owned()
    }
}

/// Project the small interactive qBittorrent page without serializing all
/// per-torrent engine reads.  The result remains in registry/snapshot order,
/// while the bounded batches prevent a slow actor from creating an unbounded
/// task fan-out or turning one page into a convoy of round trips.
async fn load_qbit_live_projections(
    state: &AppState,
    entries: Vec<rt_session::TorrentEntry>,
    active_rechecks: HashSet<String>,
) -> Result<Vec<QbTorrentInfo>, String> {
    let entries = Arc::new(entries);
    let active_rechecks = Arc::new(active_rechecks);
    let mut infos = std::iter::repeat_with(|| None)
        .take(entries.len())
        .collect::<Vec<Option<QbTorrentInfo>>>();

    for batch_start in (0..entries.len()).step_by(QBIT_LIVE_PROJECTION_CONCURRENCY) {
        let batch_end = (batch_start + QBIT_LIVE_PROJECTION_CONCURRENCY).min(entries.len());
        let mut tasks = JoinSet::new();
        for index in batch_start..batch_end {
            let state = state.clone();
            let entries = Arc::clone(&entries);
            let active_rechecks = Arc::clone(&active_rechecks);
            tasks.spawn(async move {
                let info =
                    qbit_torrent_info(&state, &entries[index], active_rechecks.as_ref(), true)
                        .await?;
                Ok::<_, String>((index, info))
            });
        }
        while let Some(task) = tasks.join_next().await {
            let (index, info) =
                task.map_err(|error| format!("qBittorrent live projection task failed: {error}"))??;
            infos[index] = Some(info);
        }
    }

    infos
        .into_iter()
        .enumerate()
        .map(|(index, info)| {
            info.ok_or_else(|| format!("qBittorrent live projection {index} was not produced"))
        })
        .collect()
}

async fn qbit_torrent_info(
    state: &AppState,
    e: &rt_session::TorrentEntry,
    active_rechecks: &HashSet<String>,
    include_live: bool,
) -> Result<QbTorrentInfo, String> {
    let progress = torrent_progress(e.total_length, e.amount_left, e.completed_at.is_some());
    let (tracker, trackers_count) = if include_live && state.engine.is_some() {
        qbit_tracker_projection(state, &e.info_hash).await?
    } else {
        (String::new(), 0)
    };
    let swarm = if include_live {
        qbit_swarm_projection(state, &e.info_hash).await?
    } else {
        QbitSwarmProjection::default()
    };
    let priority = if include_live {
        queue_priority(state, &e.info_hash).await?
    } else {
        0
    };
    let limits = if include_live {
        get_torrent_limits_result(state, &e.info_hash).await?
    } else {
        EngineTorrentLimits::default()
    };
    Ok(QbTorrentInfo {
        hash: e.info_hash.clone(),
        name: e.name.clone(),
        state: qbit_state_with_recheck(e.state.as_str(), active_rechecks.contains(&e.info_hash)),
        size: qbit_i64(e.total_length),
        total_size: qbit_i64(e.total_length),
        downloaded: qbit_i64(e.stats.downloaded),
        downloaded_session: qbit_i64(e.stats.downloaded),
        uploaded: qbit_i64(e.stats.uploaded),
        uploaded_session: qbit_i64(e.stats.uploaded),
        ratio: e.stats.ratio(),
        save_path: format!("{}/", e.save_path.trim_end_matches('/')),
        content_path: format!(
            "{}/{}",
            e.save_path.trim_end_matches('/'),
            e.name.trim_start_matches('/')
        ),
        root_path: format!(
            "{}/{}",
            e.save_path.trim_end_matches('/'),
            e.name.trim_start_matches('/')
        ),
        category: e.category.clone().unwrap_or_default(),
        tags: e.tags.join(","),
        added_on: qbit_i64(e.added_at),
        completion_on: e.completed_at.map(qbit_i64).unwrap_or(-1),
        last_activity: qbit_i64(e.added_at),
        seen_complete: e.completed_at.map(qbit_i64).unwrap_or(-1),
        time_active: 0,
        seeding_time: 0,
        num_leechs: swarm.leechers,
        num_seeds: swarm.seeds,
        dlspeed: swarm.download_rate,
        upspeed: swarm.upload_rate,
        dl_limit: limits.download_limit.unwrap_or(-1),
        up_limit: limits.upload_limit.unwrap_or(-1),
        eta: -1,
        progress,
        priority,
        amount_left: qbit_i64(e.amount_left),
        auto_tmm: limits.auto_tmm || limits.auto_management,
        seq_dl: limits.sequential_download,
        f_l_piece_prio: limits.first_last_piece_prio,
        force_start: limits.force_start,
        super_seeding: limits.super_seeding,
        ratio_limit: limits.seed_ratio_limit.unwrap_or(-1.0),
        seeding_time_limit: limits.seed_idle_limit.unwrap_or(-1),
        tracker,
        trackers_count,
        magnet_uri: qbit_magnet_uri(&e.info_hash),
        infohash_v1: if e.info_hash.len() == 40 {
            e.info_hash.clone()
        } else {
            String::new()
        },
        infohash_v2: if e.info_hash.len() == 64 {
            e.info_hash.clone()
        } else {
            String::new()
        },
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct QbitSwarmProjection {
    seeds: u32,
    leechers: u32,
    download_rate: i64,
    upload_rate: i64,
}

fn qbit_session_rates_from_infos(infos: &[QbTorrentInfo]) -> QbitSwarmProjection {
    infos
        .iter()
        .fold(QbitSwarmProjection::default(), |mut projection, info| {
            projection.download_rate = projection.download_rate.saturating_add(info.dlspeed);
            projection.upload_rate = projection.upload_rate.saturating_add(info.upspeed);
            projection
        })
}

async fn qbit_swarm_projection(
    state: &AppState,
    info_hash: &str,
) -> Result<QbitSwarmProjection, String> {
    let Some(engine) = &state.engine else {
        return Ok(QbitSwarmProjection::default());
    };
    engine
        .torrent_peers(info_hash.to_owned())
        .await
        .map(|peers| qbit_swarm_from_peers(&peers))
        .map_err(|error| error.to_string())
}

fn qbit_swarm_from_peers(peers: &[EnginePeerSnapshot]) -> QbitSwarmProjection {
    let mut projection = QbitSwarmProjection::default();
    for peer in peers {
        if peer.progress >= 1.0 {
            projection.seeds = projection.seeds.saturating_add(1);
        } else {
            projection.leechers = projection.leechers.saturating_add(1);
        }
        projection.download_rate = projection.download_rate.saturating_add(peer.download_rate);
        projection.upload_rate = projection.upload_rate.saturating_add(peer.upload_rate);
    }
    projection
}

fn qbit_magnet_uri(info_hash: &str) -> String {
    if info_hash.len() == 64 && info_hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
        format!("magnet:?xt=urn:btmh:1220{}", info_hash.to_ascii_lowercase())
    } else {
        format!("magnet:?xt=urn:btih:{info_hash}")
    }
}

fn redact_log_url(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("magnet:?") {
        return "magnet:redacted".to_owned();
    }
    match Url::parse(value) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            let sensitive = [
                "token", "apikey", "api_key", "passkey", "auth", "password", "cookie", "session",
            ];
            let pairs = url
                .query_pairs()
                .map(|(key, value)| {
                    if sensitive
                        .iter()
                        .any(|needle| key.to_ascii_lowercase().contains(needle))
                    {
                        (key.into_owned(), "redacted".to_owned())
                    } else {
                        (key.into_owned(), value.into_owned())
                    }
                })
                .collect::<Vec<_>>();
            url.set_query(None);
            if !pairs.is_empty() {
                let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                for (key, value) in pairs {
                    serializer.append_pair(&key, &value);
                }
                url.set_query(Some(&serializer.finish()));
            }
            url.to_string()
        }
        Err(_) => "url:invalid".to_owned(),
    }
}

async fn qbit_tracker_projection(
    state: &AppState,
    info_hash: &str,
) -> Result<(String, u32), String> {
    if let Some(engine) = &state.engine {
        let trackers = engine.torrent_trackers(info_hash.to_owned()).await?;
        let projection = qbit_tracker_projection_from_snapshots(&trackers);
        if projection.1 > 0 {
            return Ok(projection);
        }
        // Older durable rows may not have tracker detail rows yet. Use the
        // metadata projection only after the authoritative detail query has
        // succeeded; a failed metadata read must not become an empty 200.
        let meta = engine.torrent_metadata(info_hash.to_owned()).await?;
        let projection = (
            meta.trackers.first().cloned().unwrap_or_default(),
            u32::try_from(meta.trackers.len()).unwrap_or(u32::MAX),
        );
        if projection.1 > 0 {
            state
                .tracker_projection_cache
                .write()
                .await
                .insert(info_hash.to_owned(), projection.clone());
        }
        return Ok(projection);
    };
    if let Some(cached) = state
        .tracker_projection_cache
        .read()
        .await
        .get(info_hash)
        .cloned()
    {
        return Ok(cached);
    }
    Ok((String::new(), 0))
}

fn qbit_tracker_projection_from_snapshots(trackers: &[EngineTrackerSnapshot]) -> (String, u32) {
    let Some(first) = trackers.iter().min_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| a.announce.cmp(&b.announce))
    }) else {
        return (String::new(), 0);
    };
    (
        first.announce.clone(),
        u32::try_from(trackers.len()).unwrap_or(u32::MAX),
    )
}

#[cfg(test)]
fn sync_rid_for_infos(infos: &[QbTorrentInfo]) -> i64 {
    sync_rid_for_infos_and_trackers(infos, &[])
}

#[cfg(test)]
fn sync_rid_for_infos_and_trackers(
    infos: &[QbTorrentInfo],
    tracker_digests: &[(String, u64)],
) -> i64 {
    let mut hasher = DefaultHasher::new();
    let mut infos = infos.iter().collect::<Vec<_>>();
    infos.sort_by(|a, b| a.hash.cmp(&b.hash));
    for info in infos {
        info.hash.hash(&mut hasher);
        info.name.hash(&mut hasher);
        info.state.hash(&mut hasher);
        info.size.hash(&mut hasher);
        info.downloaded.hash(&mut hasher);
        info.uploaded.hash(&mut hasher);
        info.amount_left.hash(&mut hasher);
        info.save_path.hash(&mut hasher);
        info.category.hash(&mut hasher);
        info.tags.hash(&mut hasher);
        info.added_on.hash(&mut hasher);
        info.completion_on.hash(&mut hasher);
        info.tracker.hash(&mut hasher);
        info.trackers_count.hash(&mut hasher);
        info.progress.to_bits().hash(&mut hasher);
        info.ratio.to_bits().hash(&mut hasher);
    }
    let mut tracker_digests = tracker_digests.iter().collect::<Vec<_>>();
    tracker_digests.sort_by(|a, b| a.0.cmp(&b.0));
    for (hash, digest) in tracker_digests {
        hash.hash(&mut hasher);
        digest.hash(&mut hasher);
    }
    let rid = (hasher.finish() & 0x7fff_ffff_ffff_ffff) as i64;
    rid.max(1)
}

fn parse_form_body(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

fn strict_tag_values(tags: &str, allow_empty: bool) -> Result<Vec<String>, ()> {
    if tags.trim().is_empty() {
        return if allow_empty { Ok(Vec::new()) } else { Err(()) };
    }
    let values = tags.split(',').map(str::trim).collect::<Vec<_>>();
    if values.iter().any(|value| value.is_empty()) {
        return Err(());
    }
    Ok(values.into_iter().map(str::to_owned).collect())
}

#[cfg(test)]
fn split_tracker_values(values: &str) -> Vec<String> {
    let normalized = values.replace("\r\n", "\n").replace('\r', "\n");
    normalize_tracker_values(
        normalized
            .split(['|', '\n'])
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    )
}

fn strict_tracker_values(values: &str) -> Result<Vec<String>, ()> {
    let normalized = values.replace("\r\n", "\n").replace('\r', "\n");
    let values = normalized
        .split(['|', '\n'])
        .map(str::trim)
        .collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| value.is_empty()) {
        return Err(());
    }
    Ok(normalize_tracker_values(
        values.into_iter().map(str::to_owned).collect(),
    ))
}

fn normalize_tracker_values(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim().to_owned();
        if !value.is_empty() && !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

#[cfg(test)]
fn parse_peer_addrs(values: &str) -> Vec<SocketAddr> {
    values
        .split('|')
        .filter_map(|peer| peer.trim().parse::<SocketAddr>().ok())
        .collect()
}

fn strict_peer_addrs(values: &str) -> Result<Vec<SocketAddr>, ()> {
    let values = values.split('|').collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
        return Err(());
    }
    values
        .into_iter()
        .map(|peer| peer.trim().parse::<SocketAddr>().map_err(|_| ()))
        .collect()
}

fn normalize_api_text(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

async fn update_torrent_category(
    state: &AppState,
    hash: &str,
    category: Option<String>,
) -> StatusCode {
    if let Some(engine) = &state.engine {
        let category = Some(category);
        return match engine
            .update_torrent_labels(hash.to_owned(), category, Vec::new(), Vec::new())
            .await
        {
            Ok(()) => StatusCode::OK,
            Err(error) => qbit_engine_error_status(error),
        };
    }
    let mut reg = state.registry.write().await;
    let Some(mut entry) = reg.get_mut(hash) else {
        return StatusCode::NOT_FOUND;
    };
    entry.category = category;
    StatusCode::OK
}

async fn update_torrent_tags(
    state: &AppState,
    hash: &str,
    add_tags: Vec<String>,
    remove_tags: Vec<String>,
) -> StatusCode {
    if let Some(engine) = &state.engine {
        return match engine
            .update_torrent_labels(hash.to_owned(), None, add_tags, remove_tags)
            .await
        {
            Ok(()) => StatusCode::OK,
            Err(error) => qbit_engine_error_status(error),
        };
    }
    let mut reg = state.registry.write().await;
    let Some(mut entry) = reg.get_mut(hash) else {
        return StatusCode::NOT_FOUND;
    };
    for tag in add_tags {
        if !entry.tags.contains(&tag) {
            entry.tags.push(tag);
        }
    }
    if !remove_tags.is_empty() {
        entry.tags.retain(|tag| !remove_tags.contains(tag));
    }
    StatusCode::OK
}

async fn update_torrent_fields(
    state: &AppState,
    hash: &str,
    name: Option<String>,
    save_path: Option<std::path::PathBuf>,
) -> StatusCode {
    if let Some(engine) = &state.engine {
        return match engine
            .update_torrent_fields(hash.to_owned(), name, save_path)
            .await
        {
            Ok(()) => StatusCode::OK,
            Err(error) => qbit_engine_error_status(error),
        };
    }
    let mut reg = state.registry.write().await;
    let Some(mut entry) = reg.get_mut(hash) else {
        return StatusCode::NOT_FOUND;
    };
    if let Some(name) = name.and_then(|name| normalize_api_text(&name)) {
        entry.name = name;
    }
    if let Some(save_path) = save_path {
        entry.save_path = save_path.to_string_lossy().to_string();
    }
    StatusCode::OK
}

async fn current_tracker_urls(state: &AppState, hash: &str) -> Result<Vec<String>, String> {
    let Some(engine) = &state.engine else {
        return Ok(Vec::new());
    };
    engine
        .torrent_metadata(hash.to_owned())
        .await
        .map(|meta| meta.trackers)
        .map_err(|error| error.to_string())
}

async fn update_torrent_trackers(
    state: &AppState,
    hash: &str,
    trackers: Vec<String>,
) -> StatusCode {
    let Some(engine) = &state.engine else {
        return StatusCode::NOT_IMPLEMENTED;
    };
    match engine
        .update_torrent_trackers(hash.to_owned(), trackers)
        .await
    {
        Ok(()) => {
            state.tracker_projection_cache.write().await.remove(hash);
            StatusCode::OK
        }
        Err(error) => qbit_engine_error_status(error),
    }
}

async fn fetch_torrent_url(
    raw_url: &str,
    egress_policy: &rt_engine::OutboundEgressPolicy,
) -> Result<Vec<u8>, String> {
    const MAX_TORRENT_BYTES: usize = 16 * 1024 * 1024;

    let url = Url::parse(raw_url).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https torrent URLs are supported".to_owned());
    }
    let client = egress_policy
        .http_client(
            OutboundTargetKind::Webseed,
            &url,
            Duration::from_secs(30),
            "TorrentNG/qBittorrent",
        )
        .await
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_TORRENT_BYTES as u64)
    {
        return Err("torrent response is too large".to_owned());
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .map(|length| length.min(MAX_TORRENT_BYTES as u64) as usize)
            .unwrap_or_default(),
    );
    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len().saturating_add(chunk.len()) > MAX_TORRENT_BYTES {
            return Err("torrent response is too large".to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn resolve_hashes(state: &AppState, hashes: Vec<String>) -> Vec<String> {
    if hashes.len() == 1 && hashes[0] == "all" {
        let reg = state.registry.read().await;
        reg.iter().map(|entry| entry.info_hash.clone()).collect()
    } else {
        hashes
    }
}

async fn default_save_path(state: &AppState, preferences: &JsonMap) -> Result<String, String> {
    if let Some(save_path) = preferences
        .get("save_path")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        return Ok(format!("{}/", save_path.trim_end_matches('/')));
    }
    if let Some(engine) = &state.engine {
        let roots = engine.list_storage_roots().await?;
        if let Some(root) = roots.into_iter().find(|root| root.ok) {
            return Ok(format!(
                "{}/",
                root.path.to_string_lossy().trim_end_matches('/')
            ));
        }
    }
    let reg = state.registry.read().await;
    let save_path = reg
        .iter()
        .next()
        .map(|entry| format!("{}/", entry.save_path.trim_end_matches('/')))
        .unwrap_or_else(|| "/downloads/".to_owned());
    Ok(save_path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use rt_config::Config;
    use rt_engine::Engine;
    use rt_session::{SessionRegistry, TorrentEntry};
    use std::sync::Arc;
    use tokio::sync::{Notify, RwLock};
    use tower::ServiceExt;

    use crate::router::build_qbit_router;

    async fn make_state_with(hash: &str) -> AppState {
        let state = AppState::new();
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.to_owned(),
                "name".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        state
    }

    fn qbit_info(hash: &str, tracker: &str, trackers_count: u32) -> QbTorrentInfo {
        QbTorrentInfo {
            hash: hash.to_owned(),
            name: hash.to_owned(),
            state: "downloading".into(),
            size: 100,
            total_size: 100,
            downloaded: 25,
            downloaded_session: 25,
            uploaded: 5,
            uploaded_session: 5,
            ratio: 0.2,
            save_path: "/data/".into(),
            content_path: format!("/data/{hash}"),
            root_path: format!("/data/{hash}"),
            category: String::new(),
            tags: String::new(),
            added_on: 1,
            completion_on: -1,
            last_activity: 1,
            seen_complete: -1,
            time_active: 0,
            seeding_time: 0,
            num_leechs: 0,
            num_seeds: 0,
            dlspeed: 0,
            upspeed: 0,
            dl_limit: -1,
            up_limit: -1,
            eta: -1,
            progress: 0.25,
            priority: 0,
            amount_left: 75,
            auto_tmm: false,
            seq_dl: false,
            f_l_piece_prio: false,
            force_start: false,
            super_seeding: false,
            ratio_limit: -1.0,
            seeding_time_limit: -1,
            tracker: tracker.to_owned(),
            trackers_count,
            magnet_uri: qbit_magnet_uri(hash),
            infohash_v1: hash.to_owned(),
            infohash_v2: String::new(),
        }
    }

    #[test]
    fn qbit_filters_do_not_silently_fall_back_to_all_torrents() {
        assert!(validate_qbit_filter(Some("active")).is_ok());
        assert!(validate_qbit_filter(Some("uploading")).is_ok());
        assert_eq!(
            validate_qbit_filter(Some("not-a-filter")).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            validate_qbit_filter(Some("stalled")).unwrap_err().0,
            StatusCode::NOT_IMPLEMENTED
        );
    }

    #[test]
    fn qbit_log_entry_projects_session_events() {
        let row = rt_db::SessionEventRow {
            event_id: Some(42),
            occurred_at: 1_700_000_000,
            info_hash: Some("a".repeat(40)),
            kind: "tracker_warning".to_owned(),
            message: Some("tracker warning".to_owned()),
            payload: "{}".to_owned(),
        };

        let entry = qbit_log_entry(row).unwrap();
        assert_eq!(entry.id, 42);
        assert_eq!(entry.message, "tracker warning");
        assert_eq!(entry.timestamp, 1_700_000_000);
        assert_eq!(entry.kind, 2);
    }

    #[test]
    fn qbit_log_type_uses_level_payload_and_kind_fallbacks() {
        assert_eq!(
            qbit_log_type("torrent_added", r#"{"level":"info"}"#).unwrap(),
            1
        );
        assert_eq!(qbit_log_type("tracker_warning", "{}").unwrap(), 2);
        assert_eq!(qbit_log_type("storage_failed", "{}").unwrap(), 4);
        assert_eq!(
            qbit_log_type("tracker", r#"{"level":"critical"}"#).unwrap(),
            4
        );
    }

    #[test]
    fn qbit_log_entry_rejects_corrupt_or_unidentified_session_events() {
        let mut row = rt_db::SessionEventRow {
            event_id: Some(42),
            occurred_at: 1_700_000_000,
            info_hash: None,
            kind: "torrent_added".to_owned(),
            message: None,
            payload: "not json".to_owned(),
        };
        assert!(qbit_log_entry(row.clone()).is_err());

        row.payload = "{}".to_owned();
        row.event_id = None;
        assert!(qbit_log_entry(row).is_err());
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

        let warnings = LogMainQuery {
            limit: None,
            last_known_id: None,
            normal: None,
            info: None,
            warning: Some(true),
            critical: None,
        };
        assert!(!warnings.includes_type(1));
        assert!(warnings.includes_type(2));
        assert!(!warnings.includes_type(4));

        let critical = LogMainQuery {
            limit: None,
            last_known_id: None,
            normal: Some(false),
            info: Some(false),
            warning: Some(false),
            critical: Some(true),
        };
        assert!(!critical.includes_type(1));
        assert!(!critical.includes_type(2));
        assert!(critical.includes_type(4));
        assert_eq!(
            critical.included_levels(),
            vec!["error".to_owned(), "critical".to_owned()]
        );
    }

    #[tokio::test]
    async fn login_returns_ok() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/auth/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(cookie.starts_with("SID=torrentng;"));
    }

    #[tokio::test]
    async fn login_requires_configured_token_and_cookie_auth_round_trips() {
        let mut state = AppState::new();
        state.api_tokens = Arc::new(vec!["secret token".to_owned()]);
        let app = build_qbit_router(state);
        let bad = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/auth/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=operator&password=wrong"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::FORBIDDEN);

        let ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/auth/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=operator&password=secret+token"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let cookie = ok
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_owned();
        assert!(cookie.starts_with("SID=secret%20token; Max-Age=86400;"));

        let protected = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/app/version")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(protected.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn idempotency_key_replays_qbit_mutation_and_rejects_reuse() {
        let state = AppState::new();
        let app = build_qbit_router(state);
        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/createCategory")
                    .header("idempotency-key", "qbit-category-1")
                    .body(Body::from("category=films&savePath=%2Fdata%2Ffilms"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let replay = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/createCategory")
                    .header("idempotency-key", "qbit-category-1")
                    .body(Body::from("category=films&savePath=%2Fdata%2Ffilms"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(
            replay.headers().get("idempotency-replayed").unwrap(),
            "true"
        );

        let conflict = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/createCategory")
                    .header("idempotency-key", "qbit-category-1")
                    .body(Body::from("category=tv"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn app_version_ok() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/app/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn app_shutdown_notifies_daemon_and_email_is_explicitly_unsupported() {
        let shutdown = Arc::new(Notify::new());
        let mut state = AppState::new();
        state.shutdown = Some(Arc::clone(&shutdown));
        let app = build_qbit_router(state);
        let notified = shutdown.notified();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/shutdown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::timeout(std::time::Duration::from_secs(1), notified)
            .await
            .expect("qBittorrent shutdown request was not propagated");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/sendTestEmail")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn app_preferences_and_default_save_path_ok() {
        let hash = "f".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/app/preferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["save_path"].as_str().unwrap(), "/data/");
        assert_json_keys(
            &v,
            &[
                "locale",
                "create_subfolder_enabled",
                "start_paused_enabled",
                "auto_delete_mode",
                "preallocate_all",
                "incomplete_files_ext",
                "auto_tmm_enabled",
                "torrent_changed_tmm_enabled",
                "save_path_changed_tmm_enabled",
                "category_changed_tmm_enabled",
                "save_path",
                "temp_path_enabled",
                "temp_path",
                "scan_dirs",
                "download_in_scan_dirs",
                "export_dir_enabled",
                "export_dir",
                "export_dir_fin",
                "mail_notification_enabled",
                "mail_notification_sender",
                "mail_notification_email",
                "mail_notification_smtp",
                "mail_notification_ssl_enabled",
                "mail_notification_auth_enabled",
                "mail_notification_username",
                "mail_notification_password",
                "autorun_enabled",
                "autorun_program",
                "queueing_enabled",
                "max_active_downloads",
                "max_active_torrents",
                "max_active_uploads",
                "dont_count_slow_torrents",
                "slow_torrent_dl_rate_threshold",
                "slow_torrent_ul_rate_threshold",
                "slow_torrent_inactive_timer",
                "max_ratio_enabled",
                "max_ratio",
                "max_ratio_act",
                "max_seeding_time_enabled",
                "max_seeding_time",
                "listen_port",
                "upnp",
                "random_port",
                "dl_limit",
                "up_limit",
                "max_connec",
                "max_connec_per_torrent",
                "max_uploads",
                "max_uploads_per_torrent",
                "stop_tracker_timeout",
                "enable_piece_extent_affinity",
                "bittorrent_protocol",
                "limit_utp_rate",
                "limit_tcp_overhead",
                "limit_lan_peers",
                "alt_dl_limit",
                "alt_up_limit",
                "scheduler_enabled",
                "schedule_from_hour",
                "schedule_from_min",
                "schedule_to_hour",
                "schedule_to_min",
                "scheduler_days",
                "dht",
                "dhtSameAsBT",
                "dht_port",
                "pex",
                "lsd",
                "encryption",
                "anonymous_mode",
                "proxy_type",
                "proxy_ip",
                "proxy_port",
                "proxy_peer_connections",
                "proxy_auth_enabled",
                "proxy_username",
                "proxy_password",
                "proxy_torrents_only",
                "ip_filter_enabled",
                "ip_filter_path",
                "ip_filter_trackers",
                "web_ui_domain_list",
                "web_ui_address",
                "web_ui_port",
                "web_ui_upnp",
                "web_ui_username",
                "web_ui_password",
                "web_ui_csrf_protection_enabled",
                "web_ui_clickjacking_protection_enabled",
                "web_ui_secure_cookie_enabled",
                "web_ui_max_auth_fail_count",
                "web_ui_ban_duration",
                "web_ui_session_timeout",
                "web_ui_host_header_validation_enabled",
                "bypass_local_auth",
                "bypass_auth_subnet_whitelist_enabled",
                "bypass_auth_subnet_whitelist",
                "alternative_webui_enabled",
                "alternative_webui_path",
                "use_https",
                "ssl_key",
                "ssl_cert",
                "web_ui_https_key_path",
                "web_ui_https_cert_path",
                "dyndns_enabled",
                "dyndns_service",
                "dyndns_username",
                "dyndns_password",
                "dyndns_domain",
                "rss_refresh_interval",
                "rss_max_articles_per_feed",
                "rss_processing_enabled",
                "rss_auto_downloading_enabled",
                "rss_download_repack_proper_episodes",
                "rss_smart_episode_filters",
                "add_trackers_enabled",
                "add_trackers",
                "web_ui_use_custom_http_headers_enabled",
                "web_ui_custom_http_headers",
                "announce_to_all_tiers",
                "announce_to_all_trackers",
                "async_io_threads",
                "banned_ips",
                "checking_memory_use",
                "current_interface_address",
                "current_network_interface",
                "disk_cache",
                "disk_cache_ttl",
                "embedded_tracker_port",
                "enable_coalesce_read_write",
                "enable_embedded_tracker",
                "enable_multi_connections_from_same_ip",
                "enable_os_cache",
                "enable_upload_suggestions",
                "file_pool_size",
                "outgoing_ports_max",
                "outgoing_ports_min",
                "recheck_completed_torrents",
                "resolve_peer_countries",
                "save_resume_data_interval",
                "send_buffer_low_watermark",
                "send_buffer_watermark",
                "send_buffer_watermark_factor",
                "socket_backlog_size",
                "upload_choking_algorithm",
                "upload_slots_behavior",
                "upnp_lease_duration",
                "utp_tcp_mixed_mode",
            ],
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/app/defaultSavePath")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "/data/");
    }

    #[tokio::test]
    async fn app_set_preferences_persists_form_and_json_updates() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/setPreferences")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "json=%7B%22locale%22%3A%22fr%22%2C%22scheduler_enabled%22%3Atrue%2C%22web_ui_port%22%3A9090%7D",
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
                    .uri("/api/qb/v2/app/setPreferences")
                    .body(Body::from(
                        r#"{"rss_processing_enabled":true,"save_path":"/incoming"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/app/preferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["locale"], "fr");
        assert_eq!(v["scheduler_enabled"], true);
        assert_eq!(v["web_ui_port"], 9090);
        assert_eq!(v["rss_processing_enabled"], true);
        assert_eq!(v["save_path"], "/incoming");
    }

    #[tokio::test]
    async fn engine_backed_qbit_app_state_survives_engine_restart() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.storage.download_dir = temp.path().join("downloads");
        config.network.listen_port = 0;
        config.dht.enabled = false;

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let engine = Engine::start(Arc::new(config.clone()), Arc::clone(&registry))
            .await
            .unwrap();
        let state = AppState::with_engine(Arc::clone(&registry), engine.clone());
        let app = build_qbit_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/setPreferences")
                    .body(Body::from(r#"{"locale":"de","save_path":"/persisted"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/setCookies")
                    .body(Body::from(
                        r#"[{"host":"tracker.example","name":"sid","value":"persisted"}]"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/rotateAPIKey")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let api_key: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        let stored_api_key = engine
            .get_setting(SETTING_QBIT_API_KEY.to_owned())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Option<String>>(&stored_api_key).unwrap(),
            api_key["apiKey"].as_str().map(ToOwned::to_owned)
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/rss/addFolder")
                    .body(Body::from("path=linux"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/search/installPlugin")
                    .body(Body::from("sources=https%3A%2F%2Fexample.test%2Fjackett"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/rss/setRule")
                    .body(Body::from(
                        "ruleName=linux&ruleDef=%7B%22enabled%22%3Atrue%7D",
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        engine.shutdown().await;

        let restarted_registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let restarted_engine = Engine::start(Arc::new(config), Arc::clone(&restarted_registry))
            .await
            .unwrap();
        let restarted_state =
            AppState::with_engine(Arc::clone(&restarted_registry), restarted_engine.clone());
        assert_eq!(
            load_qbit_preferences(&restarted_state)
                .await
                .unwrap()
                .get("locale"),
            Some(&serde_json::Value::String("de".to_owned()))
        );
        assert_eq!(
            load_qbit_preferences(&restarted_state)
                .await
                .unwrap()
                .get("save_path"),
            Some(&serde_json::Value::String("/persisted".to_owned()))
        );
        assert_eq!(
            load_qbit_cookies(&restarted_state).await.unwrap(),
            vec![serde_json::json!({
                "host": "tracker.example",
                "name": "sid",
                "value": "persisted",
            })]
        );
        assert_eq!(
            restarted_engine
                .get_setting(SETTING_QBIT_API_KEY.to_owned())
                .await
                .unwrap(),
            Some(stored_api_key)
        );
        assert_eq!(
            load_qbit_rss_items(&restarted_state)
                .await
                .unwrap()
                .get("linux")
                .and_then(|item| item.get("type")),
            Some(&serde_json::Value::String("folder".to_owned()))
        );
        assert_eq!(
            load_qbit_rss_rules(&restarted_state)
                .await
                .unwrap()
                .get("linux")
                .and_then(|rule| rule.get("enabled")),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            load_qbit_search_plugins(&restarted_state)
                .await
                .unwrap()
                .get("jackett")
                .and_then(|plugin| plugin.get("enabled")),
            Some(&serde_json::Value::Bool(true))
        );
        restarted_engine.shutdown().await;
    }

    #[tokio::test]
    async fn app_cookies_and_api_key_roundtrip() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/setCookies")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "cookies=%5B%7B%22host%22%3A%22tracker.example%22%2C%22name%22%3A%22sid%22%2C%22value%22%3A%22abc%22%7D%5D",
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
                    .uri("/api/qb/v2/app/getCookies")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let cookies: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(cookies[0]["host"], "tracker.example");
        assert_eq!(cookies[0]["name"], "sid");
        assert_eq!(cookies[0]["value"], "abc");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/rotateAPIKey")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let rotated: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let rotated_key = rotated["apiKey"].as_str().unwrap();
        assert!(rotated_key.starts_with("tng_"));
        assert_eq!(rotated_key.len(), 68);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/app/deleteAPIKey")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn torrents_info_rejects_unavailable_speed_sorting() {
        let app = build_qbit_router(AppState::new());
        for sort in ["dlspeed", "upspeed"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/qb/v2/torrents/info?sort={sort}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        }
    }

    #[tokio::test]
    async fn torrents_info_empty() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }

    #[tokio::test]
    async fn torrents_add_without_engine_returns_unavailable() {
        let app = build_qbit_router(AppState::new());
        let body = "--x\r\ncontent-disposition: form-data; name=\"torrents\"; filename=\"a.torrent\"\r\n\r\nnope\r\n--x--\r\n";
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/add")
                    .header("content-type", "multipart/form-data; boundary=x")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn torrents_info_with_entry() {
        let hash = "a".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["hash"].as_str().unwrap(), hash);
    }

    #[tokio::test]
    async fn torrents_info_default_page_is_bounded() {
        let state = AppState::new();
        {
            let mut registry = state.registry.write().await;
            for index in 0..=QBIT_DEFAULT_PAGE_SIZE {
                registry
                    .add(TorrentEntry::new(
                        format!("{index:040x}"),
                        format!("torrent-{index}"),
                        "/data".into(),
                    ))
                    .unwrap();
            }
        }

        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        let torrents: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(torrents.as_array().unwrap().len(), QBIT_DEFAULT_PAGE_SIZE);
    }

    #[tokio::test]
    async fn qbit_response_field_matrix_is_present() {
        let hash = "a".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_json_keys(
            &body[0],
            &[
                "hash",
                "name",
                "state",
                "size",
                "total_size",
                "downloaded",
                "downloaded_session",
                "uploaded",
                "uploaded_session",
                "ratio",
                "save_path",
                "content_path",
                "root_path",
                "category",
                "tags",
                "added_on",
                "completion_on",
                "last_activity",
                "seen_complete",
                "time_active",
                "seeding_time",
                "num_leechs",
                "num_seeds",
                "dlspeed",
                "upspeed",
                "dl_limit",
                "up_limit",
                "eta",
                "progress",
                "priority",
                "amount_left",
                "auto_tmm",
                "seq_dl",
                "f_l_piece_prio",
                "force_start",
                "super_seeding",
                "ratio_limit",
                "seeding_time_limit",
                "tracker",
                "trackers_count",
                "magnet_uri",
                "infohash_v1",
                "infohash_v2",
            ],
        );

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/torrents/properties?hash={hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_json_keys(
            &body,
            &[
                "save_path",
                "creation_date",
                "piece_size",
                "comment",
                "total_wasted",
                "total_uploaded",
                "total_uploaded_session",
                "total_downloaded",
                "total_downloaded_session",
                "up_limit",
                "dl_limit",
                "time_elapsed",
                "seeding_time",
                "nb_connections",
                "nb_connections_limit",
                "share_ratio",
                "addition_date",
                "completion_date",
                "created_by",
                "dl_speed_avg",
                "dl_speed",
                "eta",
                "last_seen",
                "peers",
                "peers_total",
                "pieces_have",
                "pieces_num",
                "reannounce",
                "seeds",
                "seeds_total",
                "total_size",
                "up_speed_avg",
                "up_speed",
            ],
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/sync/maindata")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_json_keys(
            &body,
            &[
                "rid",
                "full_update",
                "torrents",
                "torrents_removed",
                "server_state",
            ],
        );
        assert_json_keys(
            &body["server_state"],
            &[
                "dl_info_speed",
                "dl_info_data",
                "up_info_speed",
                "up_info_data",
                "alltime_dl",
                "alltime_ul",
                "average_time_queue",
                "connection_status",
                "free_space_on_disk",
                "global_ratio",
                "queued_io_jobs",
                "queueing",
                "read_cache_hits",
                "read_cache_overload",
                "refresh_interval",
                "total_buffers_size",
                "total_peer_connections",
                "total_queued_size",
                "total_wasted_session",
                "dl_rate_limit",
                "up_rate_limit",
                "use_alt_speed_limits",
                "write_cache_overload",
            ],
        );
        let sync_torrent = &body["torrents"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"];
        assert_json_keys(
            sync_torrent,
            &[
                "hash",
                "name",
                "state",
                "size",
                "total_size",
                "downloaded",
                "downloaded_session",
                "uploaded",
                "uploaded_session",
                "ratio",
                "save_path",
                "content_path",
                "root_path",
                "category",
                "tags",
                "added_on",
                "completion_on",
                "last_activity",
                "seen_complete",
                "time_active",
                "seeding_time",
                "dl_limit",
                "up_limit",
                "seq_dl",
                "f_l_piece_prio",
                "force_start",
                "super_seeding",
                "ratio_limit",
                "seeding_time_limit",
                "magnet_uri",
                "infohash_v1",
                "infohash_v2",
            ],
        );
    }

    fn assert_json_keys(value: &serde_json::Value, keys: &[&str]) {
        let obj = value.as_object().expect("expected JSON object");
        for key in keys {
            assert!(obj.contains_key(*key), "missing key {key} in {obj:?}");
        }
    }

    #[tokio::test]
    async fn torrents_info_filters_by_tag_and_sorts() {
        let state = AppState::new();
        {
            let mut reg = state.registry.write().await;
            let mut first = TorrentEntry::new("a".repeat(40), "zeta".into(), "/data".into());
            first.total_length = 10;
            first.tags = vec!["keep".into()];
            reg.add(first).unwrap();
            let mut second = TorrentEntry::new("b".repeat(40), "alpha".into(), "/data".into());
            second.total_length = 30;
            second.tags = vec!["keep".into()];
            reg.add(second).unwrap();
            let mut skipped = TorrentEntry::new("c".repeat(40), "middle".into(), "/data".into());
            skipped.total_length = 20;
            skipped.tags = vec!["skip".into()];
            reg.add(skipped).unwrap();
        }
        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info?tag=keep&sort=size&reverse=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["name"].as_str().unwrap(), "alpha");
        assert_eq!(v[1]["name"].as_str().unwrap(), "zeta");
    }

    #[tokio::test]
    async fn torrents_info_intersects_hash_filter_with_indexed_filters() {
        let state = AppState::new();
        let keep_hash = "a".repeat(40);
        let skip_hash = "b".repeat(40);
        {
            let mut reg = state.registry.write().await;
            let mut keep = TorrentEntry::new(keep_hash.clone(), "keep".into(), "/data".into());
            keep.tags = vec!["wanted".into()];
            reg.add(keep).unwrap();
            let mut skip = TorrentEntry::new(skip_hash.clone(), "skip".into(), "/data".into());
            skip.tags = vec!["other".into()];
            reg.add(skip).unwrap();
        }
        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/qb/v2/torrents/info?hashes={keep_hash}|{skip_hash}&tag=wanted"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["hash"], keep_hash);
    }

    #[tokio::test]
    async fn torrents_info_rejects_mixed_all_hash_filter() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info?hashes=all%7Cdeadbeef")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn torrents_info_pages_are_pinned_by_snapshot_header() {
        let state = AppState::new();
        for (hash, name) in [("a", "alpha"), ("b", "bravo"), ("c", "charlie")] {
            state
                .registry
                .write()
                .await
                .add(TorrentEntry::new(
                    format!("{hash}{}", "0".repeat(39)),
                    name.into(),
                    "/data".into(),
                ))
                .unwrap();
        }
        let app = build_qbit_router(state.clone());
        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/info?sort=name&limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let snapshot = first
            .headers()
            .get("x-torrentng-snapshot")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let body = axum::body::to_bytes(first.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body[0]["name"], "alpha");

        let first_hash = "a0".to_owned() + &"0".repeat(38);
        {
            let mut registry = state.registry.write().await;
            let mut entry = registry.get_mut(&first_hash).unwrap();
            entry.name = "zulu".into();
        }

        let second = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/qb/v2/torrents/info?sort=name&offset=1&limit=1&snapshot={snapshot}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let body = axum::body::to_bytes(second.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body[0]["name"], "bravo");
    }

    #[tokio::test]
    async fn sync_maindata_returns_registry_deltas_and_removals() {
        let state = AppState::new();
        let first_hash = "a0".to_owned() + &"0".repeat(38);
        let second_hash = "b0".to_owned() + &"0".repeat(38);
        state
            .registry
            .write()
            .await
            .add(TorrentEntry::new(
                first_hash.clone(),
                "alpha".into(),
                "/data".into(),
            ))
            .unwrap();
        state
            .registry
            .write()
            .await
            .add(TorrentEntry::new(
                second_hash.clone(),
                "bravo".into(),
                "/data".into(),
            ))
            .unwrap();
        let app = build_qbit_router(state.clone());
        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/sync/maindata")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(first.into_body(), 16 * 1024)
            .await
            .unwrap();
        let first: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let rid = first["rid"].as_i64().unwrap();
        assert_eq!(first["torrents"].as_object().unwrap().len(), 2);

        {
            let mut registry = state.registry.write().await;
            let mut entry = registry.get_mut(&first_hash).unwrap();
            entry.name = "renamed".into();
        }
        state.registry.write().await.remove(&second_hash).unwrap();

        let second = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/sync/maindata?rid={rid}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let body = axum::body::to_bytes(second.into_body(), 16 * 1024)
            .await
            .unwrap();
        let second: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(second["full_update"], false);
        assert_eq!(second["torrents"][&first_hash]["name"], "renamed");
        assert_eq!(second["torrents_removed"], serde_json::json!([second_hash]));
    }

    #[tokio::test]
    async fn torrents_properties_returns_registry_projection_without_engine() {
        let hash = "d".repeat(40);
        let state = make_state_with(&hash).await;
        {
            let mut reg = state.registry.write().await;
            let mut entry = reg.get_mut(&hash).unwrap();
            entry.total_length = 1_000;
            entry.amount_left = 250;
            entry.added_at = 100;
            entry.completed_at = Some(200);
            entry.stats.add_download(750);
            entry.stats.add_upload(1_500);
        }
        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/torrents/properties?hash={hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["save_path"].as_str().unwrap(), "/data/");
        assert_eq!(v["total_size"].as_i64().unwrap(), 1_000);
        assert_eq!(v["total_downloaded"].as_i64().unwrap(), 750);
        assert_eq!(v["total_uploaded"].as_i64().unwrap(), 1_500);
        assert_eq!(v["share_ratio"].as_f64().unwrap(), 2.0);
        assert!(v["time_elapsed"].as_i64().unwrap() > 0);
        assert!(v["seeding_time"].as_i64().unwrap() > 0);
        assert_eq!(v["eta"].as_i64().unwrap(), -1);
        assert_eq!(v["piece_size"].as_i64().unwrap(), 0);
        assert_eq!(v["pieces_num"].as_i64().unwrap(), 0);
    }

    #[tokio::test]
    async fn torrents_properties_missing_hash_is_bad_request() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/properties")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn torrents_categories_and_tags_project_registry_labels() {
        let hash = "e".repeat(40);
        let state = make_state_with(&hash).await;
        {
            let mut reg = state.registry.write().await;
            let mut entry = reg.get_mut(&hash).unwrap();
            entry.category = Some("movies".into());
            entry.tags = vec!["hd".into(), "archive".into(), "hd".into()];
        }
        let app = build_qbit_router(state);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/categories")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["movies"]["name"].as_str().unwrap(), "movies");
        assert_eq!(v["movies"]["savePath"].as_str().unwrap(), "/data/");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!(["archive", "hd"]));
    }

    #[tokio::test]
    async fn sync_maindata_returns_full_update() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/sync/maindata")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["full_update"], true);
        assert!(v["rid"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn sync_maindata_uses_stable_rid_for_unchanged_registry() {
        let hash = "9".repeat(40);
        let app = build_qbit_router(make_state_with(&hash).await);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/sync/maindata")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let first: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let rid = first["rid"].as_i64().unwrap();
        assert_eq!(first["full_update"], true);
        assert_eq!(first["torrents"].as_object().unwrap().len(), 1);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/sync/maindata?rid={rid}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let second: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(second["rid"].as_i64().unwrap(), rid);
        assert_eq!(second["full_update"], false);
        assert!(second["torrents"].as_object().unwrap().is_empty());
    }

    #[test]
    fn sync_rid_is_order_independent_and_covers_metadata_projection() {
        let first = qbit_info("a", "udp://tracker-a", 1);
        let second = qbit_info("b", "udp://tracker-b", 1);
        assert_eq!(
            sync_rid_for_infos(&[first.clone(), second.clone()]),
            sync_rid_for_infos(&[second, first.clone()])
        );

        let changed = qbit_info("a", "udp://tracker-c", 2);
        assert_ne!(sync_rid_for_infos(&[first]), sync_rid_for_infos(&[changed]));
    }

    #[test]
    fn sync_rid_covers_tracker_snapshot_digest() {
        let info = qbit_info("a", "udp://tracker-a", 1);
        let first = vec![("a".to_owned(), 1_u64)];
        let changed = vec![("a".to_owned(), 2_u64)];

        assert_ne!(
            sync_rid_for_infos_and_trackers(std::slice::from_ref(&info), &first),
            sync_rid_for_infos_and_trackers(&[info], &changed)
        );
    }

    #[test]
    fn qbit_tracker_projection_prefers_live_snapshot_order() {
        let trackers = vec![
            EngineTrackerSnapshot {
                id: 2,
                tier: 2,
                announce: "https://tracker-b.example/announce".to_owned(),
                status: "working".to_owned(),
                last_announce_at: None,
                next_announce_at: None,
                last_success_at: None,
                failure_reason: None,
                warning_message: None,
                seeders: None,
                leechers: None,
                completed: None,
            },
            EngineTrackerSnapshot {
                id: 1,
                tier: 1,
                announce: "https://tracker-a.example/announce".to_owned(),
                status: "working".to_owned(),
                last_announce_at: None,
                next_announce_at: None,
                last_success_at: None,
                failure_reason: None,
                warning_message: None,
                seeders: None,
                leechers: None,
                completed: None,
            },
        ];

        assert_eq!(
            qbit_tracker_projection_from_snapshots(&trackers),
            ("https://tracker-a.example/announce".to_owned(), 2)
        );
    }

    #[tokio::test]
    async fn transfer_info_fails_closed_without_engine() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/transfer/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn set_category_resolves_all_hashes() {
        let hash = "a".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&category=archive"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reg = state.registry.read().await;
        assert_eq!(reg.get(&hash).unwrap().category.as_deref(), Some("archive"));
    }

    #[tokio::test]
    async fn set_category_decodes_url_encoded_form_values() {
        let hash = "1".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&category=tv%20shows"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reg = state.registry.read().await;
        assert_eq!(
            reg.get(&hash).unwrap().category.as_deref(),
            Some("tv shows")
        );
    }

    #[tokio::test]
    async fn rename_and_set_location_update_registry_without_engine() {
        let hash = "2".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/rename")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("hash={hash}&name=better%20name")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setLocation")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&location=%2Fnew%20data"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let reg = state.registry.read().await;
        let entry = reg.get(&hash).unwrap();
        assert_eq!(entry.name, "better name");
        assert_eq!(entry.save_path, "/new data");
    }

    #[tokio::test]
    async fn category_and_global_tag_endpoints_update_registry() {
        let hash = "3".repeat(40);
        let state = make_state_with(&hash).await;
        {
            let mut reg = state.registry.write().await;
            let mut entry = reg.get_mut(&hash).unwrap();
            entry.category = Some("old".into());
            entry.tags = vec!["remove".into(), "keep".into()];
        }
        let app = build_qbit_router(state.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/editCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("category=old&newCategory=new"))
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
                    .uri("/api/qb/v2/torrents/deleteTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("tags=remove"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/removeCategories")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("categories=new"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let reg = state.registry.read().await;
        let entry = reg.get(&hash).unwrap();
        assert_eq!(entry.category, None);
        assert_eq!(entry.tags, vec!["keep".to_owned()]);
    }

    #[tokio::test]
    async fn set_category_applies_stored_category_save_path() {
        let hash = "4".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/createCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("category=linux&savePath=%2Fsrv%2Flinux"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setCategory")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&category=linux"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reg = state.registry.read().await;
        let entry = reg.get(&hash).unwrap();
        assert_eq!(entry.category.as_deref(), Some("linux"));
        assert_eq!(entry.save_path, "/srv/linux");
    }

    #[tokio::test]
    async fn qbit_alias_and_broad_compat_routes_are_registered() {
        let app = build_qbit_router(make_state_with(&"a".repeat(40)).await);
        for (method, path, body) in qbit_route_matrix() {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_ne!(resp.status(), StatusCode::NOT_FOUND, "{path}");
            assert_ne!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
        }
    }

    #[tokio::test]
    async fn qbit_file_priority_rejects_values_outside_engine_contract() {
        let app = build_qbit_router(make_state_with(&"a".repeat(40)).await);
        for priority in ["-1", "3", "99"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/qb/v2/torrents/filePrio")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(format!("hash=a&id=0&priority={priority}")))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{priority}");
        }
    }

    fn qbit_route_matrix() -> Vec<(&'static str, &'static str, &'static str)> {
        vec![
            ("POST", "/api/qb/v2/auth/login", ""),
            ("POST", "/api/qb/v2/auth/logout", ""),
            ("GET", "/api/qb/v2/app/version", ""),
            ("GET", "/api/qb/v2/app/webapiVersion", ""),
            ("GET", "/api/qb/v2/app/buildInfo", ""),
            ("GET", "/api/qb/v2/app/preferences", ""),
            ("POST", "/api/qb/v2/app/setPreferences", "json={}"),
            ("POST", "/api/qb/v2/app/shutdown", ""),
            ("POST", "/api/qb/v2/app/sendTestEmail", ""),
            ("GET", "/api/qb/v2/app/getCookies", ""),
            ("POST", "/api/qb/v2/app/setCookies", "cookies=[]"),
            ("POST", "/api/qb/v2/app/rotateAPIKey", ""),
            ("POST", "/api/qb/v2/app/deleteAPIKey", ""),
            ("GET", "/api/qb/v2/app/networkInterfaceList", ""),
            ("GET", "/api/qb/v2/app/networkInterfaceAddressList", ""),
            ("GET", "/api/qb/v2/app/defaultSavePath", ""),
            ("GET", "/api/qb/v2/torrents/info", ""),
            ("POST", "/api/qb/v2/torrents/add", ""),
            ("POST", "/api/qb/v2/torrents/pause", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/resume", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/start", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/stop", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/delete", ""),
            ("POST", "/api/qb/v2/torrents/reannounce", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/recheck", "hashes=all"),
            ("GET", "/api/qb/v2/torrents/trackers", ""),
            (
                "POST",
                "/api/qb/v2/torrents/addTrackers",
                "hash=a&urls=http://tracker/announce",
            ),
            ("POST", "/api/qb/v2/torrents/editTracker", ""),
            (
                "POST",
                "/api/qb/v2/torrents/removeTrackers",
                "hash=a&urls=http://a",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/addPeers",
                "hashes=a&peers=127.0.0.1:6881",
            ),
            ("GET", "/api/qb/v2/torrents/files", ""),
            ("GET", "/api/qb/v2/torrents/webseeds", ""),
            ("GET", "/api/qb/v2/torrents/pieceStates", ""),
            ("GET", "/api/qb/v2/torrents/pieceHashes", ""),
            ("GET", "/api/qb/v2/torrents/export", ""),
            (
                "POST",
                "/api/qb/v2/torrents/filePrio",
                "hash=a&id=0&priority=1",
            ),
            ("POST", "/api/qb/v2/torrents/increasePrio", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/decreasePrio", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/topPrio", "hashes=all"),
            ("POST", "/api/qb/v2/torrents/bottomPrio", "hashes=all"),
            ("GET", "/api/qb/v2/torrents/properties", ""),
            ("GET", "/api/qb/v2/torrents/categories", ""),
            ("GET", "/api/qb/v2/torrents/tags", ""),
            (
                "POST",
                "/api/qb/v2/torrents/rename",
                "hash=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&name=b",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/renameFile",
                "hash=a&oldPath=a&newPath=b",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/renameFolder",
                "hash=a&oldPath=a&newPath=b",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setLocation",
                "hashes=all&location=/tmp",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setSavePath",
                "hashes=all&savePath=/tmp",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setCategory",
                "hashes=all&category=test",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/createCategory",
                "category=test",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/editCategory",
                "category=test&savePath=/tmp",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/removeCategories",
                "categories=test",
            ),
            ("POST", "/api/qb/v2/torrents/addTags", "hashes=all&tags=a,b"),
            ("POST", "/api/qb/v2/torrents/setTags", "hashes=all&tags=a,b"),
            (
                "POST",
                "/api/qb/v2/torrents/removeTags",
                "hashes=all&tags=a,b",
            ),
            ("POST", "/api/qb/v2/torrents/createTags", "tags=a,b"),
            ("POST", "/api/qb/v2/torrents/deleteTags", "tags=a,b"),
            ("GET", "/api/qb/v2/torrents/downloadLimit", ""),
            (
                "POST",
                "/api/qb/v2/torrents/setDownloadLimit",
                "hashes=all&limit=0",
            ),
            ("GET", "/api/qb/v2/torrents/uploadLimit", ""),
            (
                "POST",
                "/api/qb/v2/torrents/setUploadLimit",
                "hashes=all&limit=0",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setShareLimits",
                "hashes=all&ratioLimit=-2&seedingTimeLimit=-2",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setForceStart",
                "hashes=all&value=false",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setSuperSeeding",
                "hashes=all&value=false",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setAutoTMM",
                "hashes=all&enable=false",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/setAutoManagement",
                "hashes=all&enable=false",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/toggleSequentialDownload",
                "hashes=all",
            ),
            (
                "POST",
                "/api/qb/v2/torrents/toggleFirstLastPiecePrio",
                "hashes=all",
            ),
            ("GET", "/api/qb/v2/sync/maindata", ""),
            ("GET", "/api/qb/v2/sync/torrentPeers", ""),
            ("GET", "/api/qb/v2/transfer/info", ""),
            ("GET", "/api/qb/v2/transfer/downloadLimit", ""),
            ("GET", "/api/qb/v2/transfer/uploadLimit", ""),
            ("GET", "/api/qb/v2/transfer/speedLimitsMode", ""),
            ("POST", "/api/qb/v2/transfer/toggleSpeedLimitsMode", ""),
            ("POST", "/api/qb/v2/transfer/setDownloadLimit", "limit=0"),
            ("POST", "/api/qb/v2/transfer/setUploadLimit", "limit=0"),
            (
                "POST",
                "/api/qb/v2/transfer/banPeers",
                "peers=127.0.0.1:6881",
            ),
            ("GET", "/api/qb/v2/log/main", ""),
            ("GET", "/api/qb/v2/log/peers", ""),
            ("GET", "/api/qb/v2/search/status", ""),
            ("GET", "/api/qb/v2/search/categories", ""),
            ("GET", "/api/qb/v2/search/plugins", ""),
            (
                "POST",
                "/api/qb/v2/search/installPlugin",
                "sources=https%3A%2F%2Fexample.test%2Fjackett",
            ),
            ("POST", "/api/qb/v2/search/uninstallPlugin", "names=jackett"),
            (
                "POST",
                "/api/qb/v2/search/enablePlugin",
                "names=jackett&enable=true",
            ),
            ("POST", "/api/qb/v2/search/updatePlugins", ""),
            (
                "POST",
                "/api/qb/v2/search/start",
                "pattern=test&plugins=all&category=all",
            ),
            ("POST", "/api/qb/v2/search/stop", "id=1"),
            ("GET", "/api/qb/v2/search/results", ""),
            ("POST", "/api/qb/v2/search/delete", "id=1"),
            ("GET", "/api/qb/v2/rss/items", ""),
            ("GET", "/api/qb/v2/rss/rules", ""),
            ("GET", "/api/qb/v2/rss/matchingArticles", ""),
            ("POST", "/api/qb/v2/rss/addFolder", "path=test"),
            (
                "POST",
                "/api/qb/v2/rss/addFeed",
                "url=http://example.com/feed&path=test",
            ),
            (
                "POST",
                "/api/qb/v2/rss/moveItem",
                "itemPath=test&destPath=dest",
            ),
            (
                "POST",
                "/api/qb/v2/rss/markAsRead",
                "itemPath=dest&articleId=all",
            ),
            ("POST", "/api/qb/v2/rss/refreshItem", "itemPath=dest"),
            ("POST", "/api/qb/v2/rss/removeItem", "path=dest"),
            ("POST", "/api/qb/v2/rss/setRule", "ruleName=test&ruleDef={}"),
            (
                "POST",
                "/api/qb/v2/rss/renameRule",
                "ruleName=test&newRuleName=test2",
            ),
            ("POST", "/api/qb/v2/rss/removeRule", "ruleName=test"),
        ]
    }

    #[tokio::test]
    async fn qbit_detail_endpoints_fail_closed_without_engine_metadata() {
        let hash = "d".repeat(40);
        let app = build_qbit_router(make_state_with(&hash).await);
        for path in [
            format!("/api/qb/v2/torrents/files?hash={hash}"),
            format!("/api/qb/v2/torrents/trackers?hash={hash}"),
        ] {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[tokio::test]
    async fn qbit_search_plugins_and_jobs_are_stateful() {
        let hash = "d".repeat(40);
        let app = build_qbit_router(make_state_with(&hash).await);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/search/installPlugin")
                    .body(Body::from("sources=https%3A%2F%2Fexample.test%2Fjackett"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/search/plugins")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let plugins: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(plugins[0]["name"], "jackett");
        assert_eq!(plugins[0]["enabled"], true);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/search/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let status: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
                .unwrap();
        assert_eq!(status["plugins"][0]["name"], "jackett");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/search/categories")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let categories: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
                .unwrap();
        assert_eq!(categories, serde_json::json!(["all"]));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/search/start")
                    .body(Body::from("pattern=ubuntu&plugins=all&category=all"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let started: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = started["id"].as_i64().unwrap();
        assert!(id > 0);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/search/results?id={id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let results: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(results["pattern"], "ubuntu");
        assert_eq!(results["status"], "Stopped");
        assert_eq!(results["total"], 0);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/search/delete")
                    .body(Body::from(format!("id={id}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn qbit_rss_items_and_rules_round_trip() {
        let hash = "d".repeat(40);
        let app = build_qbit_router(make_state_with(&hash).await);

        for (path, body) in [
            ("/api/qb/v2/rss/addFolder", "path=linux"),
            (
                "/api/qb/v2/rss/addFeed",
                "url=https%3A%2F%2Fexample.test%2Frss&path=linux%2Fexample",
            ),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/rss/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let items: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(items["linux"]["type"], "folder");
        assert_eq!(items["linux/example"]["url"], "https://example.test/rss");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/rss/setRule")
                    .body(Body::from(
                        "ruleName=linux&ruleDef=%7B%22enabled%22%3Atrue%7D",
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
                    .uri("/api/qb/v2/rss/renameRule")
                    .body(Body::from("ruleName=linux&newRuleName=linux-renamed"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/rss/rules")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let rules: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(rules["linux-renamed"]["enabled"], true);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/rss/removeRule")
                    .body(Body::from("ruleName=linux-renamed"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn qbit_torrent_export_requires_hash_and_engine_blob() {
        let hash = "e".repeat(40);
        let app = build_qbit_router(make_state_with(&hash).await);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/export")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/torrents/export?hash={hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn qbit_torrent_export_streams_persisted_torrent_blob() {
        let temp = tempfile::tempdir().unwrap();
        let hash = "f".repeat(40);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry
            .write()
            .await
            .add(TorrentEntry::new(
                hash.clone(),
                "exported".into(),
                temp.path().join("downloads").to_string_lossy().into(),
            ))
            .unwrap();

        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.storage.download_dir = temp.path().join("downloads");
        config.network.listen_port = 0;
        config.dht.enabled = false;
        let blob_dir = config.daemon.session_dir.join("torrents");
        std::fs::create_dir_all(&blob_dir).unwrap();
        let raw = b"d4:infod4:name8:exportedee".to_vec();
        std::fs::write(blob_dir.join(format!("{hash}.torrent")), &raw).unwrap();
        let conn = rusqlite::Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        rt_db::upsert(
            &conn,
            &rt_db::TorrentRow {
                info_hash: hash.clone(),
                name: "exported".to_owned(),
                total_length: 0,
                piece_length: 0,
                piece_count: 0,
                is_private: false,
                save_path: config.storage.download_dir.to_string_lossy().into_owned(),
                category: None,
                tags: Vec::new(),
                state: "stopped".to_owned(),
                added_at: 1,
                completed_at: None,
                uploaded: 0,
                downloaded: 0,
                ratio: 0.0,
                trackers: Vec::new(),
            },
        )
        .unwrap();

        let engine = Engine::start(Arc::new(config), Arc::clone(&registry))
            .await
            .unwrap();
        let app = build_qbit_router(AppState::with_engine(registry, engine));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/torrents/export?hash={hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/x-bittorrent"
        );
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        assert_eq!(&body[..], &raw[..]);
    }

    #[tokio::test]
    async fn create_tags_persists_empty_global_tags() {
        let state = AppState::new();
        let app = build_qbit_router(state.clone());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/createTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("tags=hd,remux"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let tags: Vec<String> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tags, vec!["hd".to_owned(), "remux".to_owned()]);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/deleteTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("tags=hd"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/torrents/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let tags: Vec<String> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tags, vec!["remux".to_owned()]);
    }

    #[tokio::test]
    async fn transfer_limit_endpoints_roundtrip_without_engine() {
        let hash = "e".repeat(40);
        let app = build_qbit_router(make_state_with(&hash).await);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/transfer/setDownloadLimit")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("limit=4096"))
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
                    .uri("/api/qb/v2/transfer/setUploadLimit")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("limit=2048"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/transfer/downloadLimit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "4096");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/transfer/uploadLimit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "2048");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/transfer/toggleSpeedLimitsMode")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/qb/v2/transfer/speedLimitsMode")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "1");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/qb/v2/torrents/downloadLimit?hashes={hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let limits: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(limits[hash], 0);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/setUploadLimit")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&limit=0"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn transfer_ban_peers_fails_closed_without_engine() {
        let app = build_qbit_router(AppState::new());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/transfer/banPeers")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("peers=127.0.0.1:6881|[::1]:6882"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn add_and_remove_tags_resolve_all_hashes() {
        let hash = "b".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/addTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&tags=hd,archive"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/removeTags")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all&tags=hd"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let reg = state.registry.read().await;
        assert_eq!(reg.get(&hash).unwrap().tags, vec!["archive".to_owned()]);
    }

    #[tokio::test]
    async fn reannounce_reports_unavailable_without_engine() {
        let hash = "c".repeat(40);
        let state = make_state_with(&hash).await;
        let app = build_qbit_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/qb/v2/torrents/reannounce")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("hashes=all"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn ssrf_guard_rejects_private_and_local_ips() {
        let policy = rt_engine::OutboundEgressPolicy::default();
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            assert!(
                policy.validate_ip(value.parse().unwrap()).is_err(),
                "{value}"
            );
        }
        assert!(policy.validate_ip("8.8.8.8".parse().unwrap()).is_ok());
    }

    #[test]
    fn redact_log_url_removes_sensitive_parts() {
        assert_eq!(
            redact_log_url("magnet:?xt=urn:btih:abcdef"),
            "magnet:redacted"
        );
        let redacted = redact_log_url(
            "https://user:pass@example.test/announce?passkey=secret&foo=bar&api_key=hidden",
        );
        assert!(redacted.starts_with("https://example.test/announce?"));
        assert!(redacted.contains("passkey=redacted"));
        assert!(redacted.contains("api_key=redacted"));
        assert!(redacted.contains("foo=bar"));
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("hidden"));
        assert_eq!(redact_log_url("not a url"), "url:invalid");
    }

    #[test]
    fn pieces_have_is_bounded() {
        assert_eq!(pieces_have(1_000, 250, 100, 10), 8);
        assert_eq!(pieces_have(1_000, 0, 100, 10), 10);
        assert_eq!(pieces_have(1_000, 250, 0, 10), 0);
        assert_eq!(pieces_have(1_000, 250, 100, 0), 0);
    }

    #[test]
    fn qbit_progress_and_piece_count_ignore_stale_completion_timestamp() {
        assert!((torrent_progress(1_000, 250, true) - 0.75).abs() < f64::EPSILON);
        assert_eq!(pieces_have(1_000, 250, 100, 10), 8);
        assert_eq!(torrent_progress(1_000, 0, false), 1.0);
    }

    #[test]
    fn qbit_state_projects_active_recheck_as_checking() {
        assert_eq!(qbit_state_with_recheck("downloading", true), "checkingDL");
        assert_eq!(qbit_state_with_recheck("seeding", false), "uploading");
    }

    #[test]
    fn parse_form_body_decodes_qbit_forms() {
        let params = parse_form_body("hashes=a%7Cb&tags=high+quality%2Carchive");
        assert_eq!(params.get("hashes").unwrap(), "a|b");
        assert_eq!(params.get("tags").unwrap(), "high quality,archive");
    }

    #[test]
    fn split_tracker_values_accepts_qbit_separators_and_dedupes() {
        assert_eq!(
            split_tracker_values("udp://one/announce|udp://two/announce\nudp://one/announce"),
            vec![
                "udp://one/announce".to_owned(),
                "udp://two/announce".to_owned()
            ]
        );
        assert_eq!(
            split_tracker_values("udp://one/announce\r\nudp://two/announce"),
            vec![
                "udp://one/announce".to_owned(),
                "udp://two/announce".to_owned()
            ]
        );
    }

    #[test]
    fn qbit_tracker_projection_uses_persisted_engine_state() {
        let trackers = vec![EngineTrackerSnapshot {
            id: 1,
            tier: 2,
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
        }];
        let projected = qbit_trackers_from_snapshots(&trackers);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].url, "https://tracker.example/announce");
        assert_eq!(projected[0].status, 3);
        assert_eq!(projected[0].tier, 2);
        assert_eq!(projected[0].num_peers, 7);
        assert_eq!(projected[0].num_seeds, 3);
        assert_eq!(projected[0].num_leeches, 4);
        assert_eq!(projected[0].num_downloaded, 5);
        assert_eq!(projected[0].msg, "slow scrape");
    }

    #[test]
    fn parse_peer_addrs_accepts_pipe_separated_socket_addresses() {
        let peers = parse_peer_addrs("127.0.0.1:6881|[::1]:6882|bad");
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0], "127.0.0.1:6881".parse::<SocketAddr>().unwrap());
        assert_eq!(peers[1], "[::1]:6882".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn strict_mutation_parsers_reject_dropped_values() {
        assert!(strict_hashes_from_str("").is_none());
        assert!(strict_hashes_from_str("a||b").is_none());
        assert!(strict_hashes_from_str("all|a").is_none());
        assert_eq!(
            strict_hashes_from_str("ABCD|ef01").unwrap(),
            vec!["abcd".to_owned(), "ef01".to_owned()]
        );
        assert!(strict_peer_addrs("127.0.0.1:6881|bad").is_err());
        assert!(strict_numeric_list("0|bad").is_err());
        assert!(strict_tracker_values("udp://one/announce||udp://two/announce").is_err());
        assert_eq!(
            strict_tracker_values("udp://one/announce\r\nudp://two/announce").unwrap(),
            vec![
                "udp://one/announce".to_owned(),
                "udp://two/announce".to_owned()
            ]
        );
        assert!(strict_tag_values("one,,two", false).is_err());
        assert!(strict_tag_values(" one, ", false).is_err());
        assert_eq!(
            strict_tag_values("one, two", false).unwrap(),
            vec!["one".to_owned(), "two".to_owned()]
        );
        assert!(strict_tag_values("", false).is_err());
        assert!(strict_tag_values("", true).unwrap().is_empty());
    }

    #[test]
    fn search_plugin_validation_rejects_silent_projection_loss() {
        let mut plugins = serde_json::Map::new();
        plugins.insert(
            "jackett".to_owned(),
            serde_json::json!({
                "enabled": true,
                "supportedCategories": ["all", "movies"]
            }),
        );
        assert!(validate_qbit_search_plugins(&plugins).is_ok());

        plugins.insert(
            "broken".to_owned(),
            serde_json::json!({"supportedCategories": ["movies", null]}),
        );
        assert!(validate_qbit_search_plugins(&plugins).is_err());
    }

    #[test]
    fn qbit_peer_log_entry_projects_engine_peer_snapshot() {
        let peer = EnginePeerSnapshot {
            addr: "127.0.0.1:6881".parse().unwrap(),
            client: "BitTorrent peer".to_owned(),
            choked: false,
            upload_choked: false,
            interested: true,
            pieces: 4,
            pieces_total: 8,
            progress: 0.5,
            download_rate: 128,
            upload_rate: 256,
            downloaded: 1024,
            uploaded: 2048,
        };
        let entry = qbit_peer_log_entry(&"a".repeat(40), &peer);
        assert_eq!(entry["torrent"], "a".repeat(40));
        assert_eq!(entry["ip"], "127.0.0.1");
        assert_eq!(entry["port"], 6881);
        assert_eq!(entry["progress"], 0.5);
    }

    #[test]
    fn qbit_torrent_peers_projection_and_rid_are_stable() {
        let first = EnginePeerSnapshot {
            addr: "10.0.0.2:51413".parse().unwrap(),
            client: "peer-a".to_owned(),
            choked: false,
            upload_choked: false,
            interested: true,
            pieces: 5,
            pieces_total: 10,
            progress: 0.5,
            download_rate: 111,
            upload_rate: 222,
            downloaded: 333,
            uploaded: 444,
        };
        let second = EnginePeerSnapshot {
            addr: "10.0.0.3:51413".parse().unwrap(),
            client: "peer-b".to_owned(),
            choked: true,
            upload_choked: true,
            interested: false,
            pieces: 10,
            pieces_total: 10,
            progress: 1.0,
            download_rate: 0,
            upload_rate: 555,
            downloaded: 666,
            uploaded: 777,
        };
        assert_eq!(
            qbit_peer_rid(&[first.clone(), second.clone()]),
            qbit_peer_rid(&[second.clone(), first.clone()])
        );
        let changed = EnginePeerSnapshot {
            download_rate: 999,
            ..first.clone()
        };
        assert_ne!(
            qbit_peer_rid(std::slice::from_ref(&first)),
            qbit_peer_rid(&[changed])
        );

        let peers = qbit_peer_map(&[first]);
        let peer = &peers["10.0.0.2:51413"];
        assert_json_keys(
            peer,
            &[
                "client",
                "connection",
                "country",
                "country_code",
                "dl_speed",
                "downloaded",
                "files",
                "flags",
                "flags_desc",
                "ip",
                "peer_id_client",
                "port",
                "progress",
                "relevance",
                "up_speed",
                "uploaded",
            ],
        );
    }

    #[test]
    fn qbit_swarm_projection_counts_live_peers_and_rates() {
        let seed = EnginePeerSnapshot {
            addr: "127.0.0.1:6881".parse().unwrap(),
            client: "seed".to_owned(),
            choked: false,
            upload_choked: false,
            interested: false,
            pieces: 10,
            pieces_total: 10,
            progress: 1.0,
            download_rate: 100,
            upload_rate: 1_000,
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
            download_rate: 200,
            upload_rate: 2_000,
            downloaded: 30,
            uploaded: 40,
        };
        assert_eq!(
            qbit_swarm_from_peers(&[seed, leecher]),
            QbitSwarmProjection {
                seeds: 1,
                leechers: 1,
                download_rate: 300,
                upload_rate: 3_000,
            }
        );
    }

    #[test]
    fn qbit_session_rates_sum_torrent_info_rates() {
        let mut first = qbit_info(&"a".repeat(40), "", 0);
        first.dlspeed = 123;
        first.upspeed = 456;
        let mut second = qbit_info(&"b".repeat(40), "", 0);
        second.dlspeed = 10;
        second.upspeed = 20;

        assert_eq!(
            qbit_session_rates_from_infos(&[first, second]),
            QbitSwarmProjection {
                seeds: 0,
                leechers: 0,
                download_rate: 133,
                upload_rate: 476,
            }
        );
    }

    #[test]
    fn parse_qbit_bool_accepts_common_wire_values() {
        assert_eq!(parse_qbit_bool("true"), Some(true));
        assert_eq!(parse_qbit_bool("1"), Some(true));
        assert_eq!(parse_qbit_bool("false"), Some(false));
        assert_eq!(parse_qbit_bool("0"), Some(false));
        assert_eq!(parse_qbit_bool("wat"), None);
    }

    #[test]
    fn qbit_api_snapshot_estimates_scale_with_torrent_count() {
        assert_eq!(estimate_qbit_torrent_info_snapshot_bytes(0), 0);
        assert_eq!(estimate_qbit_torrent_info_snapshot_bytes(10), 20_480);
        assert_eq!(estimate_qbit_maindata_snapshot_bytes(0), 16 * 1024);
        assert_eq!(
            estimate_qbit_maindata_snapshot_bytes(10),
            16 * 1024 + 23_040
        );
        assert_eq!(
            estimate_qbit_metadata_snapshot_bytes(10, 100, 2),
            16 * 1024 + 5_120 + 9_600 + 512
        );
        assert_eq!(estimate_qbit_tracker_snapshot_bytes(10), 8 * 1024 + 5_120);
        assert_eq!(estimate_qbit_label_snapshot_bytes(10), 8 * 1024 + 2_560);
        assert_eq!(estimate_qbit_limit_map_snapshot_bytes(10), 8 * 1024 + 1_920);
        assert_eq!(estimate_qbit_properties_snapshot_bytes(), 32 * 1024);
        assert_eq!(estimate_qbit_peer_snapshot_bytes(10), 8 * 1024 + 10_240);
    }

    #[test]
    fn qbit_files_project_partial_per_file_progress() {
        let mut entry = TorrentEntry::new("a".repeat(40), "files".into(), "/data".into());
        entry.total_length = 300;
        entry.amount_left = 125;
        let meta = EngineTorrentMetadata {
            piece_length: 100,
            piece_count: 3,
            piece_hashes: Vec::new(),
            piece_states: Vec::new(),
            is_private: false,
            trackers: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            files: vec![
                EngineTorrentFile {
                    index: 0,
                    path: "one.bin".to_owned(),
                    length: 100,
                    priority: 1,
                    wanted: true,
                },
                EngineTorrentFile {
                    index: 1,
                    path: "two.bin".to_owned(),
                    length: 200,
                    priority: 0,
                    wanted: true,
                },
            ],
        };

        let files = qbit_file_infos(&entry, &meta);
        assert_eq!(files[0].progress, 1.0);
        assert_eq!(files[1].progress, 0.375);
        assert_eq!(files[1].priority, 0);
    }
}
