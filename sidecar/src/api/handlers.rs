use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::{ffi::CString, path::Path as FsPath, sync::atomic::Ordering, time::Duration};
use tokio::process::Command;

use super::server::AppState;
use super::ws::Event;
use crate::cache::{
    Category, ListParams, RatioGroup, RssRule, SavedView, WorkflowRule, WorkflowRun,
};

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
    let code = if rt_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        Json(HealthResponse {
            status: "ok",
            rtorrent: if rt_ok { "connected" } else { "unreachable" },
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

pub async fn tracker_health(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.tracker_health() {
        Ok(trackers) => Json(serde_json::json!({ "trackers": trackers })).into_response(),
        Err(e) => {
            tracing::error!("tracker health: {e}");
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

// --- Saved views ---

pub async fn list_saved_views(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_saved_views() {
        Ok(views) => Json(views).into_response(),
        Err(e) => {
            tracing::error!("list saved views: {e}");
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
            tracing::error!("upsert saved view: {e}");
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
            tracing::error!("delete saved view {id}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Ratio groups ---

pub async fn list_ratio_groups(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_ratio_groups() {
        Ok(groups) => Json(groups).into_response(),
        Err(e) => {
            tracing::error!("list ratio groups: {e}");
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
            tracing::error!("upsert ratio group: {e}");
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
            tracing::error!("delete ratio group {name}: {e}");
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
            tracing::error!("get ratio group {name}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if !group.enabled {
        return (StatusCode::BAD_REQUEST, "ratio group is disabled").into_response();
    }

    let hashes = match s.db.ratio_group_hashes(&group) {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!("ratio group hashes {name}: {e}");
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
            tracing::error!("list workflow rules: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn list_workflow_runs(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_workflow_runs() {
        Ok(runs) => Json(runs).into_response(),
        Err(e) => {
            tracing::error!("list workflow runs: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn list_rss_rules(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_rss_rules() {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => {
            tracing::error!("list rss rules: {e}");
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
            tracing::error!("upsert rss rule: {e}");
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
            tracing::error!("delete rss rule {id}: {e}");
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
            tracing::error!("test rss rules: {e}");
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
            tracing::error!("apply rss rules: {e}");
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
            tracing::error!("upsert workflow rule: {e}");
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
            tracing::error!("delete workflow rule {id}: {e}");
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
            tracing::error!("get workflow rule {id}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if !rule.enabled {
        return (StatusCode::BAD_REQUEST, "workflow rule is disabled").into_response();
    }
    let hashes = match s.db.workflow_hashes(&rule) {
        Ok(hashes) => hashes,
        Err(e) => {
            tracing::error!("workflow hashes {id}: {e}");
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
        .env("RTNG_WORKFLOW_ID", &rule.id)
        .env("RTNG_WORKFLOW_NAME", &rule.name)
        .env("RTNG_TORRENT_HASH", hash);
    if let Some(category) = &rule.category {
        child.env("RTNG_CATEGORY", category);
    }
    if let Some(tracker) = &rule.tracker {
        child.env("RTNG_TRACKER", tracker);
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
        tracing::error!("record workflow run {}: {e}", rule.id);
    } else {
        emit(s, Event::WorkflowRunsUpdated);
    }
}

fn emit(s: &AppState, event: Event) {
    let _ = s.events.send(event);
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
            tracing::error!("list torrents: {e}");
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
            tracing::error!("get torrent {hash}: {e}");
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
            tracing::error!("db exists {hash}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if let Err(e) = s.rt.set_location(&hash, save_path).await {
        tracing::error!("set location {hash}: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Err(e) = s.db.set_torrent_location(&hash, save_path) {
        tracing::error!("db set location {hash}: {e}");
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
                tracing::error!("add magnet: {e}");
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
                tracing::error!("add torrent: {e}");
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
                tracing::warn!("cache delete {hash}: {e}");
            }
            emit(&s, Event::TorrentRemoved { hash });
            emit(&s, Event::TrackerHealthUpdated);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!("delete {hash}: {e}");
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
            tracing::error!("start {hash}: {e}");
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
            tracing::error!("stop {hash}: {e}");
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
            tracing::error!("recheck {hash}: {e}");
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
            tracing::error!("reannounce {hash}: {e}");
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
            tracing::error!("list trackers {hash}: {e}");
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
            tracing::warn!("add tracker {hash} {url}: {e}");
            failures.push(format!("add {url}: {e}"));
        }
    }
    for url in remove {
        if let Err(e) = s.rt.remove_tracker(&hash, url).await {
            tracing::warn!("remove tracker {hash} {url}: {e}");
            failures.push(format!("remove {url}: {e}"));
        }
    }
    for (orig_url, new_url) in edit {
        if let Err(e) = s.rt.edit_tracker(&hash, orig_url, new_url).await {
            tracing::warn!("edit tracker {hash} {orig_url}: {e}");
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
            tracing::error!("list files {hash}: {e}");
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
            tracing::warn!("set file priority {hash}[{}]: {e}", item.index);
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
            tracing::error!("list categories: {e}");
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
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!("upsert category: {e}");
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
            tracing::error!("delete category {name}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- Tags ---

pub async fn list_tags(State(s): State<AppState>) -> impl IntoResponse {
    match s.db.list_tags() {
        Ok(tags) => Json(tags).into_response(),
        Err(e) => {
            tracing::error!("list tags: {e}");
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
            tracing::error!("create tag: {e}");
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
            tracing::error!("delete tag {name}: {e}");
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
            tracing::error!("db exists {hash}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // Persist to DB and push to rTorrent (d.custom1 = category name)
    if let Err(e) = s.db.set_torrent_category(&hash, &body.category) {
        tracing::error!("db set category {hash}: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Err(e) = s.rt.set_category(&hash, &body.category).await {
        tracing::warn!("rt set category {hash}: {e}");
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
            tracing::error!("db exists {hash}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    for tag in &tags {
        if let Err(e) = s.db.add_torrent_tag(&hash, tag) {
            tracing::error!("add tag {tag} to {hash}: {e}");
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
            tracing::error!("db exists {hash}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    for tag in &tags {
        if let Err(e) = s.db.remove_torrent_tag(&hash, tag) {
            tracing::error!("remove tag {tag} from {hash}: {e}");
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
            tracing::error!("get user agent: {e}");
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
            tracing::error!("set user agent: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
