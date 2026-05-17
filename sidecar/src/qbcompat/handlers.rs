use axum::{
    extract::{Form, Multipart, Query, State},
    http::{header, StatusCode},
    response::{AppendHeaders, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

use crate::{
    api::{server::AppState, ws::Event},
    cache::{AppEventRow, ListParams, RssRule, TorrentRow},
    rtorrent::TransferRates,
};

pub fn build_router(_state: AppState) -> Router<AppState> {
    Router::new()
        // Auth
        .route("/auth/login", post(auth_login))
        .route("/auth/logout", post(auth_logout))
        // App
        .route("/app/version", get(app_version).post(app_version))
        .route(
            "/app/webapiVersion",
            get(app_api_version).post(app_api_version),
        )
        .route("/app/buildInfo", get(app_build_info).post(app_build_info))
        .route(
            "/app/preferences",
            get(app_preferences).post(app_preferences),
        )
        .route(
            "/app/defaultSavePath",
            get(app_default_save_path).post(app_default_save_path),
        )
        // Torrents
        .route("/torrents/info", get(torrents_info))
        .route("/torrents/properties", get(torrents_properties))
        .route("/torrents/add", post(torrents_add))
        .route("/torrents/start", post(torrents_resume))
        .route("/torrents/stop", post(torrents_pause))
        .route("/torrents/pause", post(torrents_pause))
        .route("/torrents/resume", post(torrents_resume))
        .route("/torrents/delete", post(torrents_delete))
        .route("/torrents/recheck", post(torrents_recheck))
        .route("/torrents/reannounce", post(torrents_reannounce))
        .route("/torrents/trackers", get(torrents_trackers))
        .route("/torrents/webseeds", get(empty_array))
        .route("/torrents/files", get(torrents_files))
        .route("/torrents/pieceStates", get(empty_array))
        .route("/torrents/pieceHashes", get(empty_array))
        .route("/torrents/setCategory", post(torrents_set_category))
        .route("/torrents/addTags", post(torrents_add_tags))
        .route("/torrents/removeTags", post(torrents_remove_tags))
        .route("/torrents/setTags", post(torrents_set_tags))
        .route("/torrents/addPeers", post(ok_form))
        .route("/torrents/editTracker", post(torrents_edit_tracker))
        .route("/torrents/addTrackers", post(torrents_add_trackers))
        .route("/torrents/removeTrackers", post(torrents_remove_trackers))
        .route("/torrents/increasePrio", post(ok_form))
        .route("/torrents/decreasePrio", post(ok_form))
        .route("/torrents/topPrio", post(ok_form))
        .route("/torrents/bottomPrio", post(ok_form))
        .route("/torrents/filePrio", post(torrents_file_prio))
        .route("/torrents/rename", post(torrents_rename))
        .route("/torrents/renameFile", post(torrents_rename_file))
        .route("/torrents/renameFolder", post(torrents_rename_file))
        .route("/torrents/downloadLimit", get(empty_object))
        .route("/torrents/setDownloadLimit", post(ok_form))
        .route("/torrents/uploadLimit", get(empty_object))
        .route("/torrents/setUploadLimit", post(ok_form))
        .route("/torrents/setShareLimits", post(torrents_set_share_limits))
        .route("/torrents/setLocation", post(torrents_set_location))
        .route("/torrents/setSavePath", post(torrents_set_location))
        .route("/torrents/setAutoManagement", post(ok_form))
        .route("/torrents/setAutoTMM", post(ok_form))
        .route("/torrents/setForceStart", post(ok_form))
        .route("/torrents/setSuperSeeding", post(ok_form))
        .route(
            "/torrents/toggleSequentialDownload",
            post(torrents_toggle_sequential_download),
        )
        .route("/torrents/toggleFirstLastPiecePrio", post(ok_form))
        .route("/torrents/categories", get(categories))
        .route("/torrents/createCategory", post(create_category))
        .route("/torrents/editCategory", post(edit_category))
        .route("/torrents/removeCategories", post(remove_categories))
        .route("/torrents/tags", get(tags))
        .route("/torrents/createTags", post(create_tags))
        .route("/torrents/deleteTags", post(delete_tags))
        // Sync / transfer
        .route("/sync/maindata", get(sync_maindata))
        .route("/transfer/info", get(transfer_info))
        .route("/transfer/speedLimitsMode", get(zero_text))
        .route("/transfer/toggleSpeedLimitsMode", post(ok_form))
        .route("/transfer/downloadLimit", get(zero_text))
        .route("/transfer/setDownloadLimit", post(ok_form))
        .route("/transfer/uploadLimit", get(zero_text))
        .route("/transfer/setUploadLimit", post(ok_form))
        .route("/transfer/banPeers", post(ok_form))
        .route("/log/main", get(log_main))
        .route("/log/peers", get(empty_array))
        .route("/search/status", get(search_status))
        .route("/search/categories", get(empty_array))
        .route("/search/plugins", get(empty_array))
        .route("/search/installPlugin", post(ok_form))
        .route("/search/uninstallPlugin", post(ok_form))
        .route("/search/enablePlugin", post(ok_form))
        .route("/search/updatePlugins", post(ok_form))
        .route("/search/start", post(search_start))
        .route("/search/stop", post(ok_form))
        .route("/search/results", get(search_results))
        .route("/search/delete", post(ok_form))
        .route("/rss/items", get(empty_object))
        .route("/rss/addFolder", post(ok_form))
        .route("/rss/addFeed", post(ok_form))
        .route("/rss/removeItem", post(ok_form))
        .route("/rss/moveItem", post(ok_form))
        .route("/rss/markAsRead", post(ok_form))
        .route("/rss/refreshItem", post(ok_form))
        .route("/rss/setRule", post(rss_set_rule))
        .route("/rss/renameRule", post(rss_rename_rule))
        .route("/rss/removeRule", post(rss_remove_rule))
        .route("/rss/rules", get(rss_rules))
        .route("/rss/matchingArticles", get(rss_matching_articles))
        .route("/app/setPreferences", post(app_set_preferences))
}

// --- Auth ---

async fn auth_login(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    if s.cfg.auth.api_tokens.is_empty() {
        return "Ok.".into_response();
    }

    let candidate = f
        .get("password")
        .or_else(|| f.get("username"))
        .map(String::as_str)
        .unwrap_or("");

    if s.cfg.auth.api_tokens.iter().any(|token| token == candidate) {
        let tng_cookie = format!("tng_session={candidate}; Path=/; HttpOnly; SameSite=Lax");
        let sid_cookie = format!("SID={candidate}; Path=/; HttpOnly; SameSite=Lax");
        (
            AppendHeaders([
                (header::SET_COOKIE, tng_cookie),
                (header::SET_COOKIE, sid_cookie),
            ]),
            "Ok.",
        )
            .into_response()
    } else {
        "Fails.".into_response()
    }
}
async fn auth_logout() -> impl IntoResponse {
    (
        AppendHeaders([
            (
                header::SET_COOKIE,
                "tng_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
            ),
            (
                header::SET_COOKIE,
                "SID=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
            ),
        ]),
        StatusCode::OK,
    )
}

async fn ok_form(Form(_f): Form<HashMap<String, String>>) -> StatusCode {
    StatusCode::OK
}

async fn empty_array() -> Json<serde_json::Value> {
    Json(json!([]))
}

async fn empty_object() -> Json<serde_json::Value> {
    Json(json!({}))
}

async fn zero_text() -> &'static str {
    "0"
}

#[derive(Debug, Deserialize)]
struct LogMainQuery {
    limit: Option<usize>,
    last_known_id: Option<i64>,
    normal: Option<bool>,
    info: Option<bool>,
    warning: Option<bool>,
    critical: Option<bool>,
}

#[derive(Debug, Serialize)]
struct QbLogEntry {
    id: i64,
    message: String,
    timestamp: i64,
    #[serde(rename = "type")]
    kind: i64,
}

async fn log_main(State(s): State<AppState>, Query(q): Query<LogMainQuery>) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let levels = q.included_levels();
    match s
        .db
        .list_app_events_filtered(limit, None, &levels, q.last_known_id)
    {
        Ok(events) => (
            StatusCode::OK,
            Json(
                events
                    .into_iter()
                    .map(qbit_log_entry)
                    .filter(|entry| q.includes_type(entry.kind))
                    .collect::<Vec<_>>(),
            ),
        ),
        Err(e) => {
            tracing::warn!(
                component = "api",
                operation = "log_main",
                result = "error",
                error = %e,
                "failed to read app events"
            );
            (StatusCode::OK, Json(Vec::<QbLogEntry>::new()))
        }
    }
}

impl LogMainQuery {
    fn included_levels(&self) -> Vec<&'static str> {
        let any_filter = self.normal.is_some()
            || self.info.is_some()
            || self.warning.is_some()
            || self.critical.is_some();
        if !any_filter {
            return Vec::new();
        }

        let mut levels = Vec::new();
        if self.normal.unwrap_or(false) || self.info.unwrap_or(false) {
            levels.push("info");
        }
        if self.warning.unwrap_or(false) {
            levels.push("warn");
            levels.push("warning");
        }
        if self.critical.unwrap_or(false) {
            levels.push("error");
            levels.push("critical");
        }
        levels
    }

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
}

fn qbit_log_entry(row: crate::cache::AppEventRow) -> QbLogEntry {
    QbLogEntry {
        id: row.event_id.unwrap_or_default(),
        message: row.message,
        timestamp: row.occurred_at,
        kind: match row.level.as_str() {
            "error" | "critical" => 4,
            "warn" | "warning" => 2,
            _ => 1,
        },
    }
}

async fn search_status() -> Json<serde_json::Value> {
    Json(json!({
        "status": "Stopped",
        "total": 0,
    }))
}

async fn search_start(Form(_f): Form<HashMap<String, String>>) -> Json<serde_json::Value> {
    Json(json!({
        "id": 0,
    }))
}

async fn search_results(Query(_q): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "Stopped",
        "total": 0,
        "results": [],
    }))
}

async fn rss_rules(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_rss_rules() {
        Ok(rules) => {
            let map: serde_json::Map<String, serde_json::Value> = rules
                .into_iter()
                .map(|rule| {
                    (
                        rule.name.clone(),
                        json!({
                            "name": rule.name,
                            "enabled": rule.enabled,
                            "affectedFeeds": [rule.feed_url],
                            "mustContain": rule.include,
                            "mustNotContain": rule.exclude.unwrap_or_default(),
                            "assignedCategory": rule.category.unwrap_or_default(),
                            "savePath": rule.save_path.unwrap_or_default(),
                            "addPaused": !rule.start,
                            "tags": rule.tags.join(","),
                        }),
                    )
                })
                .collect();
            Json(serde_json::Value::Object(map)).into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_rss_rules",
                result = "error",
                error = %e,
                "qBit RSS rule listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
struct RssSetRuleForm {
    #[serde(rename = "ruleName")]
    rule_name: Option<String>,
    rule: Option<String>,
}

async fn rss_set_rule(State(s): State<AppState>, Form(f): Form<RssSetRuleForm>) -> StatusCode {
    let Some(name) = f
        .rule_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let raw = f.rule.as_deref().unwrap_or("{}");
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "set_rss_rule",
                result = "error",
                error = %e,
                "qBit RSS rule JSON parse failed"
            );
            return StatusCode::BAD_REQUEST;
        }
    };
    let feed_url = value
        .get("affectedFeeds")
        .and_then(|v| v.as_array())
        .and_then(|feeds| feeds.first())
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let include = value
        .get("mustContain")
        .or_else(|| value.get("contains"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let rule = RssRule {
        id: String::new(),
        name: name.to_owned(),
        enabled: value
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        feed_url: feed_url.to_owned(),
        include: include.to_owned(),
        exclude: value
            .get("mustNotContain")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .filter(|v| !v.trim().is_empty()),
        category: value
            .get("assignedCategory")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .filter(|v| !v.trim().is_empty()),
        save_path: value
            .get("savePath")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .filter(|v| !v.trim().is_empty()),
        tags: value
            .get("tags")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect(),
        start: !value
            .get("addPaused")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    if rule.feed_url.trim().is_empty() || rule.include.trim().is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    match s.db.upsert_rss_rule(rule) {
        Ok(_) => {
            emit(&s, Event::RssRulesUpdated);
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "set_rss_rule",
                rule = %name,
                result = "error",
                error = %e,
                "qBit RSS rule update failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Deserialize)]
struct RssRenameRuleForm {
    #[serde(rename = "ruleName")]
    rule_name: Option<String>,
    #[serde(rename = "newRuleName")]
    new_rule_name: Option<String>,
}

async fn rss_rename_rule(
    State(s): State<AppState>,
    Form(f): Form<RssRenameRuleForm>,
) -> StatusCode {
    let Some(old_name) = f.rule_name.as_deref() else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_name) = f
        .new_rule_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(mut rule) =
        s.db.list_rss_rules()
            .ok()
            .and_then(|rules| rules.into_iter().find(|rule| rule.name == old_name))
    else {
        return StatusCode::NOT_FOUND;
    };
    rule.name = new_name.to_owned();
    match s.db.upsert_rss_rule(rule) {
        Ok(_) => {
            emit(&s, Event::RssRulesUpdated);
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "rename_rss_rule",
                rule = %old_name,
                new_rule = %new_name,
                result = "error",
                error = %e,
                "qBit RSS rule rename failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Deserialize)]
struct RssRemoveRuleForm {
    #[serde(rename = "ruleName")]
    rule_name: Option<String>,
}

async fn rss_remove_rule(
    State(s): State<AppState>,
    Form(f): Form<RssRemoveRuleForm>,
) -> StatusCode {
    let Some(name) = f.rule_name.as_deref() else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(id) = s.db.list_rss_rules().ok().and_then(|rules| {
        rules
            .into_iter()
            .find(|rule| rule.name == name)
            .map(|rule| rule.id)
    }) else {
        return StatusCode::OK;
    };
    match s.db.delete_rss_rule(&id) {
        Ok(_) => {
            emit(&s, Event::RssRulesUpdated);
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "remove_rss_rule",
                rule = %name,
                result = "error",
                error = %e,
                "qBit RSS rule removal failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Deserialize)]
struct RssMatchingQuery {
    article: Option<String>,
}

async fn rss_matching_articles(
    State(s): State<AppState>,
    Query(q): Query<RssMatchingQuery>,
) -> impl IntoResponse {
    let Some(title) = q
        .article
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Json(json!([])).into_response();
    };
    match s.db.match_rss_item(title, None) {
        Ok(matches) => Json(json!(matches
            .into_iter()
            .filter(|m| m.matched)
            .map(|m| m.rule_name)
            .collect::<Vec<_>>()))
        .into_response(),
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "match_rss_articles",
                result = "error",
                error = %e,
                "qBit RSS article matching failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- App ---

async fn app_version(State(s): State<AppState>) -> String {
    s.cfg.identity.qbittorrent_version.clone()
}
async fn app_api_version(State(s): State<AppState>) -> String {
    s.cfg.identity.qbittorrent_webapi_version.clone()
}
async fn app_build_info(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "qt": s.cfg.identity.qbittorrent_build_qt,
        "libtorrent": s.cfg.identity.qbittorrent_build_libtorrent,
        "boost": "",
        "openssl": "",
        "bitness": 64,
    }))
}
async fn app_preferences() -> Json<serde_json::Value> {
    Json(json!({
        "save_path": "/data/downloads",
        "queueing_enabled": false,
        "max_active_torrents": -1,
    }))
}
async fn app_default_save_path() -> &'static str {
    "/data/downloads"
}

async fn app_set_preferences(
    State(s): State<AppState>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let Some(raw_prefs) = f.get("json") else {
        return StatusCode::BAD_REQUEST;
    };
    let prefs: serde_json::Value = match serde_json::from_str(raw_prefs) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                component = "qbcompat",
                operation = "set_preferences",
                result = "error",
                error = %e,
                "qBit preferences JSON parse failed"
            );
            return StatusCode::BAD_REQUEST;
        }
    };

    if let Some(ua) = prefs
        .get("network_http_user_agent")
        .and_then(|v| v.as_str())
    {
        match s.rt.set_user_agent(ua).await {
            Ok(_) => record_operator_event(
                &s,
                "info",
                "settings_changed",
                "qBittorrent preferences updated rTorrent user agent",
                serde_json::json!({
                    "component": "qbcompat",
                    "operation": "set_preferences",
                    "setting": "network_http_user_agent",
                    "result": "updated",
                    "user_agent_len": ua.len(),
                }),
            ),
            Err(e) => {
                tracing::warn!(
                    component = "qbcompat",
                    operation = "set_user_agent",
                result = "error",
                    error = %e,
                    "qBit user-agent preference update failed"
                );
                record_operator_event(
                    &s,
                    "warn",
                    "rtorrent_user_agent_error",
                    "qBittorrent preference update could not apply rTorrent user agent",
                    serde_json::json!({
                        "component": "qbcompat",
                        "operation": "set_preferences",
                        "setting": "network_http_user_agent",
                        "result": "error",
                        "error": e.to_string(),
                    }),
                );
            }
        }
    }

    StatusCode::OK
}

// --- Torrents ---

#[derive(Deserialize)]
struct InfoQuery {
    filter: Option<String>,
    category: Option<String>,
    tag: Option<String>,
    sort: Option<String>,
    reverse: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn torrents_info(State(s): State<AppState>, Query(q): Query<InfoQuery>) -> impl IntoResponse {
    let is_status = q.filter.as_deref().map(is_status_filter).unwrap_or(false);
    let params = ListParams {
        filter: if is_status { None } else { q.filter.clone() },
        status: if is_status { q.filter } else { None },
        category: q.category,
        tag: q.tag,
        tracker: None,
        media_type: None,
        sort: q.sort.as_deref().map(map_sort).map(String::from),
        dir: if q.reverse.as_deref() == Some("true") {
            Some("desc".into())
        } else {
            Some("asc".into())
        },
        limit: q.limit,
        offset: q.offset,
    };

    match s.db.list(&params) {
        Ok((rows, _)) => Json(rows.iter().map(to_qb_torrent).collect::<Vec<_>>()).into_response(),
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "torrents_info",
                result = "error",
                error = %e,
                "qBit torrent list query failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
struct HashQuery {
    hash: Option<String>,
}

async fn torrents_properties(
    State(s): State<AppState>,
    Query(q): Query<HashQuery>,
) -> impl IntoResponse {
    let Some(hash) = q.hash else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match s.db.get(&hash) {
        Ok(Some(t)) => Json(json!({
            "save_path": t.directory,
            "creation_date": t.creation_date,
            "piece_size": 0,
            "comment": "",
            "total_wasted": 0,
            "total_uploaded": t.up_total,
            "total_uploaded_session": 0,
            "total_downloaded": t.down_total,
            "total_downloaded_session": 0,
            "up_limit": -1,
            "dl_limit": -1,
            "time_elapsed": 0,
            "seeding_time": 0,
            "nb_connections": t.peers_connected,
            "nb_connections_limit": -1,
            "share_ratio": t.ratio as f64 / 1000.0,
            "addition_date": t.creation_date,
            "completion_date": t.timestamp_finished,
            "created_by": "",
            "dl_speed_avg": 0,
            "dl_speed": t.down_rate,
            "eta": if t.down_rate > 0 && t.size_bytes > t.bytes_done {
                (t.size_bytes - t.bytes_done) / t.down_rate
            } else {
                8_640_000
            },
            "last_seen": t.updated_at,
            "peers": t.peers_connected,
            "peers_total": t.peers_connected,
            "pieces_have": 0,
            "pieces_num": 0,
            "reannounce": 0,
            "seeds": t.peers_complete,
            "seeds_total": t.peers_complete,
            "total_size": t.size_bytes,
            "up_speed_avg": 0,
            "up_speed": t.up_rate,
        }))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "torrent_properties",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit torrent properties query failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn torrents_add(State(s): State<AppState>, mut multipart: Multipart) -> impl IntoResponse {
    // Try multipart first (with .torrent file), fall back to form
    let mut urls: Option<String> = None;
    let mut save_path = String::new();
    let mut category = String::new();
    let mut paused = false;
    let mut stopped = false;
    let mut torrent_data: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("urls") => {
                urls = Some(field.text().await.unwrap_or_default());
            }
            Some("savepath") => {
                save_path = field.text().await.unwrap_or_default();
            }
            Some("category") => {
                category = field.text().await.unwrap_or_default();
            }
            Some("paused") => {
                paused = field.text().await.unwrap_or_default() == "true";
            }
            Some("stopped") => {
                stopped = field.text().await.unwrap_or_default() == "true";
            }
            Some("torrents") => {
                torrent_data = field.bytes().await.ok().map(|b| b.to_vec());
            }
            _ => {
                if let Some(name) = name {
                    let _ = field.text().await;
                    tracing::debug!(
                        component = "qbcompat",
                        operation = "add_torrent",
                        field = %name,
                        "ignored qBit add field"
                    );
                }
            }
        }
    }

    let start = !(paused || stopped);

    if let Some(url_list) = urls {
        let mut added = false;
        for url in url_list.split('\n') {
            let url = url.trim();
            if url.is_empty() {
                continue;
            }
            added = true;
            if let Err(e) = s.rt.load_url(url, &save_path, &category, start).await {
                tracing::error!(
                    component = "qbcompat",
                    operation = "add_url",
                    source = %redact_log_url(url),
                result = "error",
                    error = %e,
                    "qb add url failed"
                );
                return "Fails.".into_response();
            }
        }
        if added {
            return "Ok.".into_response();
        }
    }

    if let Some(data) = torrent_data {
        if data.is_empty() {
            return (StatusCode::BAD_REQUEST, "Fails.").into_response();
        }
        if let Err(e) = s.rt.load_torrent(&data, &save_path, &category, start).await {
            tracing::error!(
                component = "qbcompat",
                operation = "add_torrent",
                result = "error",
                error = %e,
                "qb add torrent failed"
            );
            return "Fails.".into_response();
        }
        return "Ok.".into_response();
    }

    (StatusCode::BAD_REQUEST, "Fails.").into_response()
}

#[derive(Deserialize)]
struct HashesForm {
    hashes: Option<String>,
}

async fn torrents_pause(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    bulk_action(&s, &f.hashes, "stop").await
}
async fn torrents_resume(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    bulk_action(&s, &f.hashes, "start").await
}
async fn torrents_recheck(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    bulk_action(&s, &f.hashes, "recheck").await
}
async fn torrents_reannounce(State(s): State<AppState>, Form(f): Form<HashesForm>) -> StatusCode {
    bulk_action(&s, &f.hashes, "reannounce").await
}

async fn bulk_action(s: &AppState, hashes_str: &Option<String>, action: &str) -> StatusCode {
    for hash in split_hashes(&s.db, hashes_str.as_deref()) {
        let res = match action {
            "start" => s.rt.start(&hash).await,
            "stop" => s.rt.stop(&hash).await,
            "recheck" => s.rt.recheck(&hash).await,
            "reannounce" => s.rt.reannounce(&hash).await,
            _ => Ok(()),
        };
        if let Err(e) = res {
            tracing::warn!(
                component = "qbcompat",
                operation = %action,
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit torrent action failed"
            );
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct DeleteForm {
    hashes: Option<String>,
    #[serde(rename = "deleteFiles")]
    delete_files: Option<String>,
}

async fn torrents_delete(State(s): State<AppState>, Form(f): Form<DeleteForm>) -> StatusCode {
    let delete_files = f.delete_files.as_deref() == Some("true");
    for hash in split_hashes(&s.db, f.hashes.as_deref()) {
        if let Err(e) = s.rt.remove(&hash, delete_files).await {
            tracing::warn!(
                component = "qbcompat",
                operation = "delete_torrent",
                torrent = %hash,
                delete_files,
                result = "error",
                error = %e,
                "qBit delete failed"
            );
            continue;
        }
        if let Err(e) = s.db.delete(&hash) {
            tracing::warn!(
                component = "cache",
                operation = "delete_torrent",
                torrent = %hash,
                result = "error",
                error = %e,
                "cache delete failed after qBit delete"
            );
        } else {
            emit(&s, Event::TorrentRemoved { hash: hash.clone() });
            emit(&s, Event::TrackerHealthUpdated);
        }
    }
    StatusCode::OK
}

async fn torrents_trackers(
    State(s): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let hash = match q.get("hash") {
        Some(h) => h.clone(),
        None => return Json(json!([])).into_response(),
    };
    match s.rt.list_trackers(&hash).await {
        Ok(trackers) => {
            let out: Vec<_> = trackers
                .iter()
                .map(|t| {
                    let status = if t.is_enabled && t.success_counter > 0 {
                        2
                    } else if t.failed_counter > 0 {
                        4
                    } else {
                        0
                    };
                    json!({
                        "url":      t.url,
                        "status":   status,
                        "tier":     t.group,
                        "num_peers":   t.scrape_incomplete + t.scrape_complete,
                        "num_seeds":   t.scrape_complete,
                        "num_leechs":  t.scrape_incomplete,
                        "num_downloaded": t.scrape_downloaded,
                        "msg":      t.message,
                    })
                })
                .collect();
            Json(json!(out)).into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_trackers",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit tracker listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn torrents_files(
    State(s): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let hash = match q.get("hash") {
        Some(h) => h.clone(),
        None => return Json(json!([])).into_response(),
    };
    match s.rt.list_files(&hash).await {
        Ok(files) => {
            let out: Vec<_> = files
                .iter()
                .map(|f| {
                    let progress = if f.size_chunks > 0 {
                        f.completed_chunks as f64 / f.size_chunks as f64
                    } else {
                        1.0
                    };
                    // qBit priority: 0=do not download, 1=normal, 6=high, 7=maximal
                    let priority = match f.priority {
                        0 => 0,
                        2 => 6,
                        _ => 1,
                    };
                    json!({
                        "index":        f.index,
                        "name":         f.path,
                        "size":         f.size_bytes,
                        "progress":     progress,
                        "priority":     priority,
                        "is_seed":      f.completed_chunks >= f.size_chunks,
                        "piece_range":  [0, 0],
                        "availability": if f.is_created { 1.0f64 } else { 0.0f64 },
                    })
                })
                .collect();
            Json(json!(out)).into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_files",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit file listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
async fn categories(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_categories() {
        Ok(cats) => {
            let map: serde_json::Map<String, serde_json::Value> = cats
                .iter()
                .map(|c| {
                    (
                        c.name.clone(),
                        json!({ "name": c.name, "savePath": c.save_path }),
                    )
                })
                .collect();
            Json(serde_json::Value::Object(map)).into_response()
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_categories",
                result = "error",
                error = %e,
                "qBit category listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn tags(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_tags() {
        Ok(tags) => Json(tags).into_response(),
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "list_tags",
                result = "error",
                error = %e,
                "qBit tag listing failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
struct SetCategoryForm {
    hashes: Option<String>,
    category: Option<String>,
}

async fn torrents_set_category(
    State(s): State<AppState>,
    Form(f): Form<SetCategoryForm>,
) -> StatusCode {
    let category = f.category.as_deref().unwrap_or("");
    for hash in split_hashes(&s.db, f.hashes.as_deref()) {
        if let Err(e) = s.db.set_torrent_category(&hash, category) {
            tracing::warn!(
                component = "cache",
                operation = "set_category",
                torrent = %hash,
                category = %category,
                result = "error",
                error = %e,
                "cache category update failed"
            );
        } else {
            emit_torrent_updated(&s, &hash);
            emit(&s, Event::CategoriesUpdated);
        }
        if let Err(e) = s.rt.set_category(&hash, category).await {
            tracing::warn!(
                component = "rtorrent",
                operation = "set_category",
                torrent = %hash,
                category = %category,
                result = "error",
                error = %e,
                "rTorrent category update failed"
            );
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct TagsForm {
    hashes: Option<String>,
    tags: Option<String>,
}

async fn torrents_add_tags(State(s): State<AppState>, Form(f): Form<TagsForm>) -> StatusCode {
    let tag_list: Vec<&str> = f
        .tags
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    for hash in split_hashes(&s.db, f.hashes.as_deref()) {
        for tag in &tag_list {
            if let Err(e) = s.db.add_torrent_tag(&hash, tag) {
                tracing::warn!(
                    component = "cache",
                    operation = "add_tag",
                    torrent = %hash,
                    tag = %tag,
                result = "error",
                    error = %e,
                    "cache tag add failed"
                );
            } else {
                emit_torrent_updated(&s, &hash);
                emit(&s, Event::TagsUpdated);
            }
        }
    }
    StatusCode::OK
}

async fn torrents_remove_tags(State(s): State<AppState>, Form(f): Form<TagsForm>) -> StatusCode {
    let tag_list: Vec<&str> = f
        .tags
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    for hash in split_hashes(&s.db, f.hashes.as_deref()) {
        for tag in &tag_list {
            if let Err(e) = s.db.remove_torrent_tag(&hash, tag) {
                tracing::warn!(
                    component = "cache",
                    operation = "remove_tag",
                    torrent = %hash,
                    tag = %tag,
                result = "error",
                    error = %e,
                    "cache tag removal failed"
                );
            } else {
                emit_torrent_updated(&s, &hash);
                emit(&s, Event::TagsUpdated);
            }
        }
    }
    StatusCode::OK
}

async fn torrents_set_tags(State(s): State<AppState>, Form(f): Form<TagsForm>) -> StatusCode {
    let tag_list: Vec<&str> = f
        .tags
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    for hash in split_hashes(&s.db, f.hashes.as_deref()) {
        if let Err(e) = s.db.set_torrent_tags(&hash, &tag_list) {
            tracing::warn!(
                component = "cache",
                operation = "set_tags",
                torrent = %hash,
                result = "error",
                error = %e,
                "cache tag replace failed"
            );
        } else {
            emit_torrent_updated(&s, &hash);
            emit(&s, Event::TagsUpdated);
        }
    }
    StatusCode::OK
}

// --- qBit category/tag management ---

#[derive(Deserialize)]
struct CreateCategoryForm {
    category: Option<String>,
    #[serde(rename = "savePath")]
    save_path: Option<String>,
}

async fn create_category(
    State(s): State<AppState>,
    Form(f): Form<CreateCategoryForm>,
) -> StatusCode {
    let name = match f.category.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => n,
        _ => return StatusCode::BAD_REQUEST,
    };
    let save_path = f.save_path.as_deref().unwrap_or("");
    match s.db.upsert_category(name, save_path) {
        Ok(_) => {
            emit(&s, Event::CategoriesUpdated);
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "create_category",
                category = %name,
                result = "error",
                error = %e,
                "qBit category create failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn edit_category(State(s): State<AppState>, Form(f): Form<CreateCategoryForm>) -> StatusCode {
    let name = match f.category.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => n,
        _ => return StatusCode::BAD_REQUEST,
    };
    let save_path = f.save_path.as_deref().unwrap_or("");
    match s.db.upsert_category(name, save_path) {
        Ok(_) => {
            emit(&s, Event::CategoriesUpdated);
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "edit_category",
                category = %name,
                result = "error",
                error = %e,
                "qBit category edit failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Deserialize)]
struct RemoveCategoriesForm {
    categories: Option<String>,
}

async fn remove_categories(
    State(s): State<AppState>,
    Form(f): Form<RemoveCategoriesForm>,
) -> StatusCode {
    for name in f
        .categories
        .as_deref()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Err(e) = s.db.delete_category(name) {
            tracing::warn!(
                component = "qbcompat",
                operation = "remove_category",
                category = %name,
                result = "error",
                error = %e,
                "qBit category removal failed"
            );
        } else {
            emit(&s, Event::CategoriesUpdated);
            emit(&s, Event::TrackerHealthUpdated);
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct CreateTagsForm {
    tags: Option<String>,
}

async fn create_tags(State(s): State<AppState>, Form(f): Form<CreateTagsForm>) -> StatusCode {
    for tag in f
        .tags
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if let Err(e) = s.db.ensure_tag(tag) {
            tracing::warn!(
                component = "qbcompat",
                operation = "create_tag",
                tag = %tag,
                result = "error",
                error = %e,
                "qBit tag create failed"
            );
        } else {
            emit(&s, Event::TagsUpdated);
        }
    }
    StatusCode::OK
}

async fn delete_tags(State(s): State<AppState>, Form(f): Form<CreateTagsForm>) -> StatusCode {
    for tag in f
        .tags
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if let Err(e) = s.db.delete_tag(tag) {
            tracing::warn!(
                component = "qbcompat",
                operation = "delete_tag",
                tag = %tag,
                result = "error",
                error = %e,
                "qBit tag delete failed"
            );
        } else {
            emit(&s, Event::TagsUpdated);
            emit(&s, Event::TrackerHealthUpdated);
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct FilePrioForm {
    hash: Option<String>,
    id: Option<String>, // pipe-separated file indices
    priority: Option<String>,
}

async fn torrents_file_prio(State(s): State<AppState>, Form(f): Form<FilePrioForm>) -> StatusCode {
    let hash = match f.hash {
        Some(h) => h,
        None => return StatusCode::BAD_REQUEST,
    };
    let priority: i64 = match f.priority.as_deref() {
        Some("0") => 0,
        Some("6") | Some("7") => 2,
        _ => 1,
    };
    if let Some(ids) = f.id {
        for id_str in ids.split('|') {
            if let Ok(idx) = id_str.parse::<usize>() {
                if let Err(e) = s.rt.set_file_priority(&hash, idx, priority).await {
                    tracing::warn!(
                            component = "qbcompat",
                            operation = "set_file_priority",
                            torrent = %hash,
                            file_index = idx,
                            priority,
                    result = "error",
                            error = %e,
                            "qBit file priority update failed"
                        );
                }
            }
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct AddTrackersForm {
    hashes: Option<String>,
    urls: Option<String>,
}

async fn torrents_add_trackers(
    State(s): State<AppState>,
    Form(f): Form<AddTrackersForm>,
) -> StatusCode {
    let urls: Vec<&str> = f
        .urls
        .as_deref()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .collect();

    for hash in split_hashes(&s.db, f.hashes.as_deref()) {
        for url in &urls {
            if let Err(e) = s.rt.add_tracker(&hash, url).await {
                tracing::warn!(
                    component = "qbcompat",
                    operation = "add_tracker",
                    torrent = %hash,
                    tracker = %redact_log_url(url),
                result = "error",
                    error = %e,
                    "qb add tracker failed"
                );
            }
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct RemoveTrackersForm {
    hash: Option<String>,
    urls: Option<String>,
}

async fn torrents_remove_trackers(
    State(s): State<AppState>,
    Form(f): Form<RemoveTrackersForm>,
) -> StatusCode {
    let Some(hash) = f.hash else {
        return StatusCode::BAD_REQUEST;
    };
    let urls: Vec<&str> = f
        .urls
        .as_deref()
        .unwrap_or("")
        .split('|')
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .collect();

    for url in urls {
        if let Err(e) = s.rt.remove_tracker(&hash, url).await {
            tracing::warn!(
                component = "qbcompat",
                operation = "remove_tracker",
                torrent = %hash,
                tracker = %redact_log_url(url),
                result = "error",
                error = %e,
                "qb remove tracker failed"
            );
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct EditTrackerForm {
    hash: Option<String>,
    #[serde(rename = "origUrl")]
    orig_url: Option<String>,
    #[serde(rename = "newUrl")]
    new_url: Option<String>,
}

async fn torrents_edit_tracker(
    State(s): State<AppState>,
    Form(f): Form<EditTrackerForm>,
) -> StatusCode {
    let Some(hash) = f.hash else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(orig_url) = f.orig_url else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(new_url) = f.new_url else {
        return StatusCode::BAD_REQUEST;
    };
    if let Err(e) = s.rt.edit_tracker(&hash, &orig_url, &new_url).await {
        tracing::warn!(
            component = "qbcompat",
            operation = "edit_tracker",
            torrent = %hash,
            tracker = %redact_log_url(&orig_url),
            new_tracker = %redact_log_url(&new_url),
                result = "error",
            error = %e,
            "qBit tracker edit failed"
        );
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct RenameForm {
    hash: Option<String>,
    name: Option<String>,
}

async fn torrents_rename(State(s): State<AppState>, Form(f): Form<RenameForm>) -> StatusCode {
    let Some(hash) = f.hash else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(name) = f.name else {
        return StatusCode::BAD_REQUEST;
    };
    if let Err(e) = s.rt.rename_torrent(&hash, &name).await {
        tracing::warn!(
            component = "qbcompat",
            operation = "rename_torrent",
            torrent = %hash,
                result = "error",
            error = %e,
            "qBit torrent rename failed"
        );
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct RenameFileForm {
    hash: Option<String>,
    id: Option<usize>,
    name: Option<String>,
}

async fn torrents_rename_file(
    State(s): State<AppState>,
    Form(f): Form<RenameFileForm>,
) -> StatusCode {
    let Some(hash) = f.hash else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(id) = f.id else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(name) = f.name else {
        return StatusCode::BAD_REQUEST;
    };
    if let Err(e) = s.rt.rename_file(&hash, id, &name).await {
        tracing::warn!(
            component = "qbcompat",
            operation = "rename_file",
            torrent = %hash,
            file_index = id,
                result = "error",
            error = %e,
            "qBit file rename failed"
        );
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct ShareLimitsForm {
    hashes: Option<String>,
    #[serde(rename = "ratioLimit")]
    ratio_limit: Option<f64>,
    #[serde(rename = "seedingTimeLimit")]
    seeding_time_limit: Option<i64>,
}

async fn torrents_set_share_limits(
    State(s): State<AppState>,
    Form(f): Form<ShareLimitsForm>,
) -> StatusCode {
    let ratio_limit_milli = f
        .ratio_limit
        .map(|ratio| (ratio * 1000.0) as i64)
        .unwrap_or(-2);
    let seeding_time_limit = f.seeding_time_limit.unwrap_or(-2);
    for hash in split_hashes(&s.db, f.hashes.as_deref()) {
        if let Err(e) =
            s.rt.set_share_limits(&hash, ratio_limit_milli, seeding_time_limit)
                .await
        {
            tracing::warn!(
                component = "qbcompat",
                operation = "set_share_limits",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit share limit update failed"
            );
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct LocationForm {
    hashes: Option<String>,
    location: Option<String>,
}

async fn torrents_set_location(
    State(s): State<AppState>,
    Form(f): Form<LocationForm>,
) -> StatusCode {
    let Some(location) = f.location.as_deref().map(str::trim) else {
        return StatusCode::BAD_REQUEST;
    };
    if location.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    for hash in split_hashes(&s.db, f.hashes.as_deref()) {
        match s.db.exists(&hash) {
            Ok(true) => {
                if let Err(e) = s.db.set_torrent_location(&hash, location) {
                    tracing::warn!(
                            component = "cache",
                            operation = "set_location",
                            torrent = %hash,
                    result = "error",
                            error = %e,
                            "cache location update failed"
                        );
                } else {
                    emit_torrent_updated(&s, &hash);
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(
                component = "cache",
                operation = "exists",
                torrent = %hash,
                result = "error",
                error = %e,
                "cache torrent existence check failed"
            ),
        }
        if let Err(e) = s.rt.set_location(&hash, location).await {
            tracing::warn!(
                component = "rtorrent",
                operation = "set_location",
                torrent = %hash,
                result = "error",
                error = %e,
                "rTorrent location update failed"
            );
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct ToggleSequentialForm {
    hashes: Option<String>,
}

async fn torrents_toggle_sequential_download(
    State(s): State<AppState>,
    Form(f): Form<ToggleSequentialForm>,
) -> StatusCode {
    for hash in split_hashes(&s.db, f.hashes.as_deref()) {
        if let Err(e) = s.rt.toggle_sequential_download(&hash).await {
            tracing::warn!(
                component = "qbcompat",
                operation = "toggle_sequential_download",
                torrent = %hash,
                result = "error",
                error = %e,
                "qBit sequential download toggle failed"
            );
        }
    }
    StatusCode::OK
}

// --- Sync ---

#[derive(Deserialize)]
struct MaindataQuery {
    rid: Option<i64>,
}

async fn sync_maindata(
    State(s): State<AppState>,
    Query(q): Query<MaindataQuery>,
) -> impl IntoResponse {
    // rid absent or 0 → full update; rid>0 → incremental since that rid
    let rid = q.rid.unwrap_or(0);
    let full = rid == 0;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let (categories, tags) = match sync_metadata(&s) {
        Ok(metadata) => metadata,
        Err(e) => {
            tracing::error!(
                component = "qbcompat",
                operation = "sync_maindata_metadata",
                result = "error",
                error = %e,
                "qBit maindata metadata load failed"
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let server_state = qb_server_state(crate::stats::current_rates(s.rt.clone()).await);

    if full {
        let params = ListParams {
            limit: Some(50000),
            ..Default::default()
        };
        match s.db.list(&params) {
            Ok((rows, _total)) => {
                // Use wall clock as floor so empty DB doesn't return rid=0 (which re-triggers full update)
                let max_updated: i64 = rows
                    .iter()
                    .map(|t| t.updated_at)
                    .max()
                    .unwrap_or(now_secs)
                    .max(now_secs - 1);
                let torrents = match torrents_map(&rows) {
                    Ok(torrents) => torrents,
                    Err(e) => {
                        tracing::error!(
                                    component = "qbcompat",
                                    operation = "sync_maindata_serialize",
                        result = "error",
                                    error = %e,
                                    "qBit maindata serialization failed"
                                );
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                };
                Json(json!({
                    "rid": max_updated,
                    "full_update": true,
                    "torrents": torrents,
                    "torrents_removed": [],
                    "categories": categories,
                    "tags": tags,
                    "server_state": server_state,
                }))
                .into_response()
            }
            Err(e) => {
                tracing::error!(
                    component = "qbcompat",
                    operation = "sync_maindata",
                result = "error",
                    error = %e,
                    "qBit maindata query failed"
                );
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    } else {
        match s.db.list_since(rid) {
            Ok((rows, max_updated)) => {
                let torrents = match torrents_map(&rows) {
                    Ok(torrents) => torrents,
                    Err(e) => {
                        tracing::error!(
                                    component = "qbcompat",
                                    operation = "sync_maindata_delta_serialize",
                        result = "error",
                                    error = %e,
                                    "qBit maindata delta serialization failed"
                                );
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                };
                Json(json!({
                    "rid": max_updated,
                    "full_update": false,
                    "torrents": torrents,
                    "torrents_removed": [],
                    "categories": categories,
                    "tags": tags,
                    "server_state": server_state,
                }))
                .into_response()
            }
            Err(e) => {
                tracing::error!(
                    component = "qbcompat",
                    operation = "sync_maindata_delta",
                result = "error",
                    error = %e,
                    "qBit maindata delta query failed"
                );
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

// --- Transfer ---

fn torrents_map(
    rows: &[TorrentRow],
) -> serde_json::Result<serde_json::Map<String, serde_json::Value>> {
    rows.iter()
        .map(|t| serde_json::to_value(to_qb_torrent(t)).map(|value| (t.hash.clone(), value)))
        .collect()
}

fn sync_metadata(
    s: &AppState,
) -> anyhow::Result<(serde_json::Map<String, serde_json::Value>, Vec<String>)> {
    let categories =
        s.db.list_categories()?
            .into_iter()
            .map(|c| {
                (
                    c.name.clone(),
                    json!({ "name": c.name, "savePath": c.save_path }),
                )
            })
            .collect();
    let tags = s.db.list_tags()?;
    Ok((categories, tags))
}

async fn transfer_info(State(s): State<AppState>) -> Json<serde_json::Value> {
    let rates = crate::stats::current_rates(s.rt.clone()).await;
    let totals = crate::stats::session_totals();

    Json(json!({
        "connection_status": "connected",
        "dl_info_speed": rates.download,
        "dl_info_data": totals.download,
        "up_info_speed": rates.upload,
        "up_info_data": totals.upload,
        "dl_rate_limit": 0,
        "up_rate_limit": 0,
    }))
}

fn qb_server_state(rates: TransferRates) -> serde_json::Value {
    json!({
        "connection_status": "connected",
        "dl_info_speed": rates.download,
        "up_info_speed": rates.upload,
    })
}

#[cfg(test)]
mod tests {
    use crate::{cache::AppEventRow, rtorrent::TransferRates};

    use super::{qb_server_state, qbit_log_entry, LogMainQuery};

    #[test]
    fn qb_server_state_includes_current_transfer_rates() {
        let state = qb_server_state(TransferRates {
            download: 1_234,
            upload: 567,
        });

        assert_eq!(state["connection_status"], "connected");
        assert_eq!(state["dl_info_speed"], 1_234);
        assert_eq!(state["up_info_speed"], 567);
    }

    #[test]
    fn qbit_log_entry_maps_app_event_levels() {
        let entry = qbit_log_entry(AppEventRow {
            event_id: Some(7),
            occurred_at: 1_700_000_000,
            level: "warning".to_owned(),
            kind: "rtorrent_log".to_owned(),
            message: "tracker warning".to_owned(),
            payload: "{}".to_owned(),
        });
        assert_eq!(entry.id, 7);
        assert_eq!(entry.message, "tracker warning");
        assert_eq!(entry.timestamp, 1_700_000_000);
        assert_eq!(entry.kind, 2);
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

        let warning = LogMainQuery {
            limit: None,
            last_known_id: None,
            normal: None,
            info: None,
            warning: Some(true),
            critical: None,
        };
        assert!(!warning.includes_type(1));
        assert!(warning.includes_type(2));
        assert!(!warning.includes_type(4));
    }

    #[test]
    fn redact_log_url_removes_sensitive_url_parts() {
        assert_eq!(
            super::redact_log_url("magnet:?xt=urn:btih:abc"),
            "[redacted-magnet]"
        );
        assert_eq!(
            super::redact_log_url("https://tracker.example/announce?passkey=secret#frag"),
            "https://tracker.example/announce"
        );
        assert_eq!(
            super::redact_log_url("/data/private/file.torrent"),
            "[redacted-path]"
        );
    }
}

// --- mapping helpers ---

pub fn to_qb_torrent(t: &TorrentRow) -> serde_json::Value {
    let (down_rate, up_rate) = current_row_rates(t);
    let progress = if t.size_bytes > 0 {
        t.bytes_done as f64 / t.size_bytes as f64
    } else {
        0.0
    };
    let state = if !t.is_open {
        "pausedUP"
    } else if t.state == 2 {
        "checkingUP"
    } else if t.complete && t.is_active {
        "uploading"
    } else if !t.complete && t.is_active {
        "downloading"
    } else {
        "pausedUP"
    };
    let eta = if down_rate > 0 && t.size_bytes > t.bytes_done {
        (t.size_bytes - t.bytes_done) / down_rate
    } else {
        8_640_000
    };

    json!({
        "hash":          t.hash,
        "name":          t.name,
        "size":          t.size_bytes,
        "progress":      progress,
        "dlspeed":       down_rate,
        "upspeed":       up_rate,
        "priority":      t.priority,
        "num_seeds":     t.peers_complete,
        "num_leechs":    t.peers_connected,
        "ratio":         t.ratio as f64 / 1000.0,
        "eta":           eta,
        "state":         state,
        "category":      t.category,
        "tags":          t.tags,
        "added_on":      t.creation_date,
        "completion_on": t.timestamp_finished,
        "save_path":     t.directory,
        "downloaded":    t.down_total,
        "uploaded":      t.up_total,
        "amount_left":   (t.size_bytes - t.bytes_done).max(0),
        "completed":     t.bytes_done,
        "tracker":       t.tracker_url,
        "magnet_uri":    "",
    })
}

fn current_row_rates(t: &TorrentRow) -> (i64, i64) {
    const STALE_AFTER_SECS: i64 = 15;
    let now = chrono::Utc::now().timestamp();
    if t.updated_at <= 0 || now.saturating_sub(t.updated_at) > STALE_AFTER_SECS {
        return (0, 0);
    }
    (t.down_rate.max(0), t.up_rate.max(0))
}

fn split_hashes(db: &crate::cache::Db, s: Option<&str>) -> Vec<String> {
    match s {
        None | Some("") => vec![],
        Some(s) if s.trim() == "all" => match db.all_hashes() {
            Ok(hashes) => hashes.into_iter().collect(),
            Err(e) => {
                tracing::warn!(
                    component = "qbcompat",
                    operation = "resolve_hashes",
                result = "error",
                    error = %e,
                    "failed to resolve hashes=all"
                );
                vec![]
            }
        },
        Some(s) => s
            .split('|')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_owned)
            .collect(),
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
    append_operator_event(s, "info", kind, message, payload);
}

fn record_operator_event(
    s: &AppState,
    level: &str,
    kind: &str,
    message: &str,
    payload: serde_json::Value,
) {
    append_operator_event(
        s,
        level,
        kind.to_owned(),
        message.to_owned(),
        payload.to_string(),
    );
}

fn append_operator_event(
    s: &AppState,
    level: &str,
    kind: String,
    message: String,
    payload: String,
) {
    if let Err(e) = s.db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: chrono::Utc::now().timestamp(),
            level: level.to_owned(),
            kind,
            message,
            payload,
        },
        s.cfg.logging.event_retention,
    ) {
        tracing::warn!(component = "app_events", operation = "append", result = "error", error = %e, "failed to append app event");
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

fn map_sort(s: &str) -> &str {
    match s {
        "name" => "name",
        "size" => "size",
        "added_on" => "added",
        "ratio" => "ratio",
        "dlspeed" => "speed_down",
        "upspeed" => "speed_up",
        "progress" => "progress",
        _ => "name",
    }
}

fn is_status_filter(f: &str) -> bool {
    matches!(
        f,
        "downloading"
            | "seeding"
            | "completed"
            | "paused"
            | "active"
            | "inactive"
            | "resumed"
            | "stalled"
            | "checking"
            | "moving"
            | "errored"
    )
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
