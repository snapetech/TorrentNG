use axum::{
    extract::{Form, Multipart, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    api::server::AppState,
    cache::{ListParams, TorrentRow},
};

const QB_VERSION: &str = "5.0.0";
const QB_API_VERSION: &str = "2.11.0";

pub fn build_router(_state: AppState) -> Router<AppState> {
    Router::new()
        // Auth
        .route("/auth/login",        post(auth_login))
        .route("/auth/logout",       post(auth_logout))
        // App
        .route("/app/version",       get(app_version))
        .route("/app/webapiVersion", get(app_api_version))
        .route("/app/preferences",   get(app_preferences))
        // Torrents
        .route("/torrents/info",      get(torrents_info))
        .route("/torrents/add",       post(torrents_add))
        .route("/torrents/pause",     post(torrents_pause))
        .route("/torrents/resume",    post(torrents_resume))
        .route("/torrents/delete",    post(torrents_delete))
        .route("/torrents/recheck",   post(torrents_recheck))
        .route("/torrents/reannounce",post(torrents_reannounce))
        .route("/torrents/trackers",  get(torrents_trackers))
        .route("/torrents/files",     get(torrents_files))
        .route("/torrents/setCategory", post(torrents_set_category))
        .route("/torrents/addTags",   post(torrents_add_tags))
        .route("/torrents/removeTags",post(torrents_remove_tags))
        .route("/torrents/editTracker", post(stub_ok))
        .route("/torrents/addTrackers", post(stub_ok))
        .route("/torrents/filePrio",  post(torrents_file_prio))
        .route("/torrents/categories",get(categories))
        .route("/torrents/tags",      get(tags))
        // Sync / transfer
        .route("/sync/maindata",      get(sync_maindata))
        .route("/transfer/info",      get(transfer_info))
}

// --- Auth ---

async fn auth_login() -> &'static str { "Ok." }
async fn auth_logout() -> StatusCode { StatusCode::OK }

// --- App ---

async fn app_version() -> &'static str { QB_VERSION }
async fn app_api_version() -> &'static str { QB_API_VERSION }
async fn app_preferences() -> Json<serde_json::Value> {
    Json(json!({
        "save_path": "/data/downloads",
        "queueing_enabled": false,
        "max_active_torrents": -1,
    }))
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

async fn torrents_info(
    State(s): State<AppState>,
    Query(q): Query<InfoQuery>,
) -> impl IntoResponse {
    let is_status = q.filter.as_deref().map(is_status_filter).unwrap_or(false);
    let params = ListParams {
        filter:   if is_status { None } else { q.filter.clone() },
        status:   if is_status { q.filter } else { None },
        category: q.category,
        tag:      q.tag,
        sort:     q.sort.as_deref().map(map_sort).map(String::from),
        dir:      if q.reverse.as_deref() == Some("true") { Some("desc".into()) } else { Some("asc".into()) },
        limit:    q.limit,
        offset:   q.offset,
    };

    match s.db.list(&params) {
        Ok((rows, _)) => Json(rows.iter().map(to_qb_torrent).collect::<Vec<_>>()).into_response(),
        Err(e) => { tracing::error!("qb torrents/info: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

#[derive(Deserialize)]
struct AddForm {
    urls: Option<String>,
    savepath: Option<String>,
    paused: Option<String>,
    category: Option<String>,
}

async fn torrents_add(
    State(s): State<AppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Try multipart first (with .torrent file), fall back to form
    let mut urls: Option<String> = None;
    let mut save_path = String::new();
    let mut paused = false;
    let mut torrent_data: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name() {
            Some("urls")     => { urls = Some(field.text().await.unwrap_or_default()); }
            Some("savepath") => { save_path = field.text().await.unwrap_or_default(); }
            Some("paused")   => { paused = field.text().await.unwrap_or_default() == "true"; }
            Some("torrents") => { torrent_data = field.bytes().await.ok().map(|b| b.to_vec()); }
            _ => {}
        }
    }

    let start = !paused;

    if let Some(url_list) = urls {
        for url in url_list.split('\n') {
            let url = url.trim();
            if url.is_empty() { continue; }
            if let Err(e) = s.rt.load_magnet(url, &save_path, start).await {
                tracing::error!("qb add url {url}: {e}");
                return "Fails.".into_response();
            }
        }
        return "Ok.".into_response();
    }

    if let Some(data) = torrent_data {
        if let Err(e) = s.rt.load_torrent(&data, &save_path, start).await {
            tracing::error!("qb add torrent: {e}");
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
    for hash in split_hashes(hashes_str.as_deref()) {
        let res = match action {
            "start"     => s.rt.start(&hash).await,
            "stop"      => s.rt.stop(&hash).await,
            "recheck"   => s.rt.recheck(&hash).await,
            "reannounce"=> s.rt.reannounce(&hash).await,
            _ => Ok(()),
        };
        if let Err(e) = res {
            tracing::warn!("qb {action} {hash}: {e}");
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
    for hash in split_hashes(f.hashes.as_deref()) {
        if let Err(e) = s.rt.remove(&hash, delete_files).await {
            tracing::warn!("qb delete {hash}: {e}");
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
            let out: Vec<_> = trackers.iter().map(|t| {
                let status = if t.is_enabled && t.success_counter > 0 { 2 }
                    else if t.failed_counter > 0 { 4 }
                    else { 0 };
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
            }).collect();
            Json(json!(out)).into_response()
        }
        Err(e) => {
            tracing::error!("qb trackers {hash}: {e}");
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
            let out: Vec<_> = files.iter().map(|f| {
                let progress = if f.size_chunks > 0 {
                    f.completed_chunks as f64 / f.size_chunks as f64
                } else { 1.0 };
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
            }).collect();
            Json(json!(out)).into_response()
        }
        Err(e) => {
            tracing::error!("qb files {hash}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
async fn categories(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_categories() {
        Ok(cats) => {
            let map: serde_json::Map<String, serde_json::Value> = cats.iter()
                .map(|c| (c.name.clone(), json!({ "name": c.name, "savePath": c.save_path })))
                .collect();
            Json(serde_json::Value::Object(map)).into_response()
        }
        Err(e) => { tracing::error!("qb categories: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

async fn tags(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_tags() {
        Ok(tags) => Json(tags).into_response(),
        Err(e) => { tracing::error!("qb tags: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

#[derive(Deserialize)]
struct SetCategoryForm { hashes: Option<String>, category: Option<String> }

async fn torrents_set_category(State(s): State<AppState>, Form(f): Form<SetCategoryForm>) -> StatusCode {
    let category = f.category.as_deref().unwrap_or("");
    for hash in split_hashes(f.hashes.as_deref()) {
        if let Err(e) = s.db.set_torrent_category(&hash, category) {
            tracing::warn!("qb setCategory db {hash}: {e}");
        }
        if let Err(e) = s.rt.set_category(&hash, category).await {
            tracing::warn!("qb setCategory rt {hash}: {e}");
        }
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct TagsForm { hashes: Option<String>, tags: Option<String> }

async fn torrents_add_tags(State(s): State<AppState>, Form(f): Form<TagsForm>) -> StatusCode {
    let tag_list: Vec<&str> = f.tags.as_deref().unwrap_or("").split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
    for hash in split_hashes(f.hashes.as_deref()) {
        for tag in &tag_list {
            if let Err(e) = s.db.add_torrent_tag(&hash, tag) {
                tracing::warn!("qb addTags {hash} {tag}: {e}");
            }
        }
    }
    StatusCode::OK
}

async fn torrents_remove_tags(State(s): State<AppState>, Form(f): Form<TagsForm>) -> StatusCode {
    let tag_list: Vec<&str> = f.tags.as_deref().unwrap_or("").split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
    for hash in split_hashes(f.hashes.as_deref()) {
        for tag in &tag_list {
            if let Err(e) = s.db.remove_torrent_tag(&hash, tag) {
                tracing::warn!("qb removeTags {hash} {tag}: {e}");
            }
        }
    }
    StatusCode::OK
}

async fn stub_ok() -> StatusCode { StatusCode::OK }

#[derive(Deserialize)]
struct FilePrioForm {
    hash: Option<String>,
    id: Option<String>,    // pipe-separated file indices
    priority: Option<String>,
}

async fn torrents_file_prio(
    State(s): State<AppState>,
    Form(f): Form<FilePrioForm>,
) -> StatusCode {
    let hash = match f.hash { Some(h) => h, None => return StatusCode::BAD_REQUEST };
    let priority: i64 = match f.priority.as_deref() {
        Some("0") => 0,
        Some("6") | Some("7") => 2,
        _ => 1,
    };
    if let Some(ids) = f.id {
        for id_str in ids.split('|') {
            if let Ok(idx) = id_str.parse::<usize>() {
                if let Err(e) = s.rt.set_file_priority(&hash, idx, priority).await {
                    tracing::warn!("qb filePrio {hash}[{idx}]: {e}");
                }
            }
        }
    }
    StatusCode::OK
}

// --- Sync ---

async fn sync_maindata(State(s): State<AppState>) -> impl IntoResponse {
    let params = ListParams { limit: Some(10000), ..Default::default() };
    match s.db.list(&params) {
        Ok((rows, total)) => {
            let torrents: serde_json::Map<String, serde_json::Value> = rows.iter()
                .map(|t| (t.hash.clone(), serde_json::to_value(to_qb_torrent(t)).unwrap()))
                .collect();
            Json(json!({
                "rid": 1,
                "full_update": true,
                "torrents": torrents,
                "torrents_removed": [],
                "categories": {},
                "tags": [],
                "server_state": {
                    "connection_status": "connected",
                    "dl_info_speed": 0,
                    "up_info_speed": 0,
                },
                "total": total,
            })).into_response()
        }
        Err(e) => { tracing::error!("qb maindata: {e}"); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

// --- Transfer ---

async fn transfer_info() -> Json<serde_json::Value> {
    Json(json!({
        "connection_status": "connected",
        "dl_info_speed": 0,
        "dl_info_data": 0,
        "up_info_speed": 0,
        "up_info_data": 0,
        "dl_rate_limit": 0,
        "up_rate_limit": 0,
    }))
}

// --- mapping helpers ---

pub fn to_qb_torrent(t: &TorrentRow) -> serde_json::Value {
    let progress = if t.size_bytes > 0 { t.bytes_done as f64 / t.size_bytes as f64 } else { 0.0 };
    let state = if !t.is_open { "pausedUP" }
        else if t.state == 2 { "checkingUP" }
        else if t.complete && t.is_active { "uploading" }
        else if !t.complete && t.is_active { "downloading" }
        else { "pausedUP" };
    let eta = if t.down_rate > 0 && t.size_bytes > t.bytes_done {
        (t.size_bytes - t.bytes_done) / t.down_rate
    } else { 8_640_000 };

    json!({
        "hash":          t.hash,
        "name":          t.name,
        "size":          t.size_bytes,
        "progress":      progress,
        "dlspeed":       t.down_rate,
        "upspeed":       t.up_rate,
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

pub fn to_summary(t: &TorrentRow) -> serde_json::Value { to_qb_torrent(t) }

fn split_hashes(s: Option<&str>) -> Vec<String> {
    match s {
        None | Some("") | Some("all") => vec![],
        Some(s) => s.split('|').map(str::to_owned).collect(),
    }
}

fn map_sort(s: &str) -> &str {
    match s {
        "name"     => "name",
        "size"     => "size",
        "added_on" => "added",
        "ratio"    => "ratio",
        "dlspeed"  => "speed_down",
        "upspeed"  => "speed_up",
        "progress" => "progress",
        _          => "name",
    }
}

fn is_status_filter(f: &str) -> bool {
    matches!(f, "downloading" | "seeding" | "completed" | "paused" | "active"
                | "inactive" | "resumed" | "stalled" | "checking" | "moving" | "errored")
}
