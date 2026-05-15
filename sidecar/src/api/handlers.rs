use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

use super::server::AppState;
use crate::cache::{Category, ListParams};

// --- Health ---

#[derive(Serialize)]
pub struct HealthResponse {
    status: &'static str,
    rtorrent: &'static str,
    cached_torrents: i64,
}

pub async fn health(State(s): State<AppState>) -> impl IntoResponse {
    let rt_ok = s.rt.call("system.listMethods", &[]).await.is_ok();
    let cached = s.db.count().unwrap_or(0);
    let code = if rt_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(HealthResponse {
        status:          "ok",
        rtorrent:        if rt_ok { "connected" } else { "unreachable" },
        cached_torrents: cached,
    }))
}

// --- Metrics ---

pub async fn metrics_handler(State(s): State<AppState>) -> impl IntoResponse {
    // Update gauges from cache before rendering
    if let Ok(count) = s.db.count() {
        s.metrics.torrents_total.store(count, Ordering::Relaxed);
    }
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        s.metrics.render(),
    )
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
            tracing::error!("list torrents: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Add torrent ---

pub async fn add_torrent(
    State(s): State<AppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    s.metrics.api_requests_total.fetch_add(1, Ordering::Relaxed);
    let mut save_path = String::new();
    let mut start = true;
    let mut magnet: Option<String> = None;
    let mut torrent_data: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name() {
            Some("save_path") => { save_path = field.text().await.unwrap_or_default(); }
            Some("start")     => { start = field.text().await.unwrap_or_default() != "false"; }
            Some("magnet")    => { magnet = Some(field.text().await.unwrap_or_default()); }
            Some("torrent")   => { torrent_data = field.bytes().await.ok().map(|b| b.to_vec()); }
            _ => {}
        }
    }

    if let Some(m) = magnet {
        match s.rt.load_magnet(&m, &save_path, start).await {
            Ok(_) => return StatusCode::ACCEPTED.into_response(),
            Err(e) => { tracing::error!("add magnet: {e}"); return StatusCode::INTERNAL_SERVER_ERROR.into_response(); }
        }
    }
    if let Some(data) = torrent_data {
        match s.rt.load_torrent(&data, &save_path, start).await {
            Ok(_) => return StatusCode::ACCEPTED.into_response(),
            Err(e) => { tracing::error!("add torrent: {e}"); return StatusCode::INTERNAL_SERVER_ERROR.into_response(); }
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
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => { tracing::error!("delete {hash}: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

// --- Per-torrent actions ---

pub async fn torrent_start(State(s): State<AppState>, Path(hash): Path<String>) -> impl IntoResponse {
    match s.rt.start(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => { tracing::error!("start {hash}: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}
pub async fn torrent_stop(State(s): State<AppState>, Path(hash): Path<String>) -> impl IntoResponse {
    match s.rt.stop(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => { tracing::error!("stop {hash}: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}
pub async fn torrent_recheck(State(s): State<AppState>, Path(hash): Path<String>) -> impl IntoResponse {
    match s.rt.recheck(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => { tracing::error!("recheck {hash}: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}
pub async fn torrent_reannounce(State(s): State<AppState>, Path(hash): Path<String>) -> impl IntoResponse {
    match s.rt.reannounce(&hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => { tracing::error!("reannounce {hash}: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

// --- Trackers ---

pub async fn torrent_trackers(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    match s.rt.list_trackers(&hash).await {
        Ok(trackers) => Json(serde_json::json!({ "trackers": trackers })).into_response(),
        Err(e) => { tracing::error!("list trackers {hash}: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

// --- Files ---

pub async fn torrent_files(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    match s.rt.list_files(&hash).await {
        Ok(files) => Json(serde_json::json!({ "files": files })).into_response(),
        Err(e) => { tracing::error!("list files {hash}: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
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
    for item in &body.files {
        if let Err(e) = s.rt.set_file_priority(&hash, item.index, item.priority).await {
            tracing::warn!("set file priority {hash}[{}]: {e}", item.index);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

// --- Categories ---

pub async fn list_categories(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_categories() {
        Ok(cats) => Json(cats).into_response(),
        Err(e) => { tracing::error!("list categories: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
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
    let save_path = body.save_path.as_deref().unwrap_or("");
    match s.db.upsert_category(&body.name, save_path) {
        Ok(_) => Json(Category { name: body.name, save_path: save_path.to_owned() }).into_response(),
        Err(e) => { tracing::error!("upsert category: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

pub async fn delete_category(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match s.db.delete_category(&name) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => { tracing::error!("delete category {name}: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

// --- Tags ---

pub async fn list_tags(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_tags() {
        Ok(tags) => Json(tags).into_response(),
        Err(e) => { tracing::error!("list tags: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

#[derive(Deserialize)]
pub struct TagBody { pub name: String }

pub async fn create_tag(
    State(s): State<AppState>,
    Json(body): Json<TagBody>,
) -> impl IntoResponse {
    match s.db.ensure_tag(&body.name) {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(e) => { tracing::error!("create tag: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

pub async fn delete_tag(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match s.db.delete_tag(&name) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => { tracing::error!("delete tag {name}: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

// --- Torrent category/tag assignment ---

#[derive(Deserialize)]
pub struct SetCategoryBody { pub category: String }

pub async fn set_torrent_category(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<SetCategoryBody>,
) -> impl IntoResponse {
    // Persist to DB and push to rTorrent (d.custom1 = category name)
    if let Err(e) = s.db.set_torrent_category(&hash, &body.category) {
        tracing::error!("db set category {hash}: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Err(e) = s.rt.set_category(&hash, &body.category).await {
        tracing::warn!("rt set category {hash}: {e}");
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct ModTagsBody { pub tags: Vec<String> }

pub async fn add_torrent_tags(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<ModTagsBody>,
) -> impl IntoResponse {
    for tag in &body.tags {
        if let Err(e) = s.db.add_torrent_tag(&hash, tag) {
            tracing::error!("add tag {tag} to {hash}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn remove_torrent_tags(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    Json(body): Json<ModTagsBody>,
) -> impl IntoResponse {
    for tag in &body.tags {
        if let Err(e) = s.db.remove_torrent_tag(&hash, tag) {
            tracing::error!("remove tag {tag} from {hash}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

// --- Bulk actions ---

#[derive(Deserialize)]
pub struct BulkBody {
    pub hashes: Vec<String>,
    #[serde(default)]
    pub dry_run: bool,
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
    if body.dry_run {
        return Json(BulkResult { applied: body.hashes.clone(), errors: vec![], dry_run: true }).into_response();
    }
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for hash in &body.hashes {
        let res = match action.as_str() {
            "start"      => s.rt.start(hash).await,
            "stop"       => s.rt.stop(hash).await,
            "recheck"    => s.rt.recheck(hash).await,
            "reannounce" => s.rt.reannounce(hash).await,
            _ => { errors.push(format!("unknown action: {action}")); continue; }
        };
        match res {
            Ok(_) => applied.push(hash.clone()),
            Err(e) => errors.push(format!("{hash}: {e}")),
        }
    }
    Json(BulkResult { applied, errors, dry_run: false }).into_response()
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
        Err(e) => { tracing::error!("get user agent: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
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
        Ok(_) => { tracing::info!(user_agent = %ua, "user agent updated"); Json(UserAgentResponse { user_agent: ua }).into_response() }
        Err(e) => { tracing::error!("set user agent: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}
