use std::{collections::BTreeMap, convert::Infallible, path::PathBuf, time::Duration};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use base64::Engine as _;
use futures::Stream;
use rt_api_model::{
    AddTorrentRequest, AddTorrentResponse, ApiError, FileInfo, TorrentDetail, TorrentSummary,
};
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_metrics::MemoryClass;
use rt_session::{TorrentEntry, TorrentState};
use rt_storage::{
    runtime::StorageRuntime, DeletePlanRequest, ImportPlanRequest, MovePlanRequest, PlanIssue,
    PlannedStorageAction, StoragePlan, StoragePlanStep, STORAGE_LATENCY_BUCKETS_NS,
};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// `GET /api/v1/torrents` — list all torrents.
pub async fn list_torrents(State(state): State<AppState>) -> impl IntoResponse {
    if let Some(engine) = &state.engine {
        let torrent_count = state.registry.read().await.iter().count();
        let estimate = estimate_torrent_summary_snapshot_bytes(torrent_count);
        match engine
            .reserve_memory(MemoryClass::ApiSnapshot, estimate)
            .await
        {
            Ok(Some(_lease)) => {
                let reg = state.registry.read().await;
                let summaries: Vec<TorrentSummary> = reg.iter().map(torrent_summary).collect();
                return (StatusCode::OK, Json(summaries)).into_response();
            }
            Ok(None) => return api_snapshot_budget_exhausted(),
            Err(e) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
                )
                    .into_response();
            }
        }
    }

    let reg = state.registry.read().await;
    let summaries: Vec<TorrentSummary> = reg.iter().map(torrent_summary).collect();
    (StatusCode::OK, Json(summaries)).into_response()
}

fn api_snapshot_budget_exhausted() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(
            serde_json::to_value(ApiError::internal(
                "api snapshot memory budget exhausted".to_owned(),
            ))
            .unwrap(),
        ),
    )
        .into_response()
}

/// `POST /api/v1/torrents` — add a v1/hybrid `.torrent` from base64 JSON.
pub async fn add_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AddTorrentRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal(
                    "native engine is not available".to_owned(),
                ))
                .unwrap(),
            ),
        )
            .into_response();
    };

    if let Some(magnet) = req
        .magnet
        .as_deref()
        .filter(|magnet| !magnet.trim().is_empty())
    {
        let magnet = match parse_magnet(magnet) {
            Ok(magnet) => magnet,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::to_value(ApiError::bad_request(e.to_string())).unwrap()),
                )
                    .into_response();
            }
        };
        let save_path = if req.save_path.trim().is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(req.save_path))
        };
        let paused = !req.start.unwrap_or(true);
        return match engine
            .add_magnet_with_labels(
                magnet,
                save_path,
                paused,
                req.category,
                req.tags.unwrap_or_default(),
            )
            .await
        {
            Ok(info_hash) => (
                StatusCode::CREATED,
                Json(serde_json::to_value(AddTorrentResponse { info_hash }).unwrap()),
            )
                .into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response(),
        };
    }

    let Some(torrent_b64) = req.torrent_b64.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ApiError::bad_request("torrent_b64 is required".to_owned()))
                    .unwrap(),
            ),
        )
            .into_response();
    };

    let raw = match base64::engine::general_purpose::STANDARD.decode(torrent_b64) {
        Ok(raw) => raw,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ApiError::bad_request(format!(
                        "invalid torrent_b64: {e}"
                    )))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    };
    let meta = match parse_torrent(&raw) {
        Ok(meta) => meta,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ApiError::bad_request(format!(
                        "invalid torrent metadata: {e}"
                    )))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    };

    let save_path = if req.save_path.trim().is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(req.save_path))
    };
    let paused = !req.start.unwrap_or(true);

    match engine
        .add_torrent_with_labels(
            meta,
            save_path,
            paused,
            req.category,
            req.tags.unwrap_or_default(),
        )
        .await
    {
        Ok(info_hash) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(AddTorrentResponse { info_hash }).unwrap()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `GET /api/v1/torrents/{hash}` — get one torrent.
pub async fn get_torrent(
    State(state): State<AppState>,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    let summary = {
        let reg = state.registry.read().await;
        match reg.get(&info_hash) {
            Some(e) => torrent_summary(e),
            None => return not_found(info_hash),
        }
    };
    let Some(engine) = &state.engine else {
        return (StatusCode::OK, Json(serde_json::to_value(summary).unwrap())).into_response();
    };

    let base_estimate = estimate_torrent_detail_base_snapshot_bytes();
    let _base_lease = match engine
        .reserve_memory(MemoryClass::ApiSnapshot, base_estimate)
        .await
    {
        Ok(Some(lease)) => lease,
        Ok(None) => return api_snapshot_budget_exhausted(),
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
            )
                .into_response();
        }
    };

    match engine.torrent_metadata(info_hash.clone()).await {
        Ok(meta) => {
            let extra_estimate = estimate_torrent_detail_snapshot_bytes(&summary, &meta)
                .saturating_sub(base_estimate);
            let _extra_lease = if extra_estimate > 0 {
                match engine
                    .reserve_memory(MemoryClass::ApiSnapshot, extra_estimate)
                    .await
                {
                    Ok(Some(lease)) => Some(lease),
                    Ok(None) => return api_snapshot_budget_exhausted(),
                    Err(e) => {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
                        )
                            .into_response();
                    }
                }
            } else {
                None
            };
            let detail = TorrentDetail {
                summary,
                piece_length: meta.piece_length as i64,
                piece_count: meta.piece_count as i64,
                is_private: meta.is_private,
                trackers: meta.trackers,
                files: meta
                    .files
                    .into_iter()
                    .map(|file| FileInfo {
                        file_index: file.index,
                        path: file.path,
                        length: file.length as i64,
                        priority: 1,
                    })
                    .collect(),
            };
            (StatusCode::OK, Json(serde_json::to_value(detail).unwrap())).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::to_value(ApiError::internal(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `DELETE /api/v1/torrents/{hash}` — remove a torrent.
pub async fn delete_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    if let Some(engine) = &state.engine {
        match engine.remove_torrent(info_hash.clone(), false).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => not_found(info_hash),
        }
    } else {
        let mut reg = state.registry.write().await;
        let _ = reg.remove(&info_hash);
        StatusCode::NO_CONTENT.into_response()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoragePlanRequest {
    operation: String,
    source: Option<PathBuf>,
    destination: Option<PathBuf>,
    target: Option<PathBuf>,
    bytes: Option<u64>,
    available_bytes: Option<u64>,
    hardlink_or_copy: Option<bool>,
    dry_run: Option<bool>,
    dry_run_approved: Option<bool>,
    affected_torrents: Option<Vec<String>>,
    roots: Option<Vec<PathBuf>>,
    completed_steps: Option<Vec<usize>>,
}

#[derive(Debug, Serialize)]
pub struct StoragePlanResponse {
    operation: String,
    job_id: Option<String>,
    plan: StoragePlanView,
}

#[derive(Debug, Serialize)]
pub struct StoragePlanView {
    dry_run: bool,
    can_apply: bool,
    issues: Vec<String>,
    steps: Vec<StoragePlanStepView>,
    rollback_steps: Vec<StoragePlanStepView>,
}

#[derive(Debug, Serialize)]
pub struct StoragePlanStepView {
    action: String,
    source: Option<String>,
    destination: Option<String>,
    bytes: u64,
}

/// `POST /api/v1/storage/plan` — preview a move/import/delete storage plan.
pub async fn storage_preview_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StoragePlanRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    match build_storage_plan(&req, true) {
        Ok(plan) => {
            if let Some(response) = validate_storage_plan_roots(&plan, req.roots.as_deref()) {
                return response;
            }
            (
                StatusCode::OK,
                Json(storage_plan_response(&req.operation, &plan, None)),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

/// `POST /api/v1/storage/execute` — execute a storage plan through durable engine jobs.
pub async fn storage_execute_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StoragePlanRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal(
                    "native engine is not available".to_owned(),
                ))
                .unwrap(),
            ),
        )
            .into_response();
    };
    let roots = match req.roots.clone() {
        Some(roots) if !roots.is_empty() => roots,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ApiError::bad_request(
                        "roots must include at least one configured storage root for execution"
                            .to_owned(),
                    ))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    };
    let plan = match build_storage_plan(&req, false) {
        Ok(plan) => plan,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
            )
                .into_response();
        }
    };
    if let Some(response) = validate_storage_plan_roots(&plan, Some(&roots)) {
        return response;
    }
    match engine
        .execute_storage_plan(
            normalize_storage_operation(&req.operation)
                .unwrap_or_else(|| req.operation.to_ascii_lowercase()),
            req.affected_torrents.unwrap_or_default(),
            plan.clone(),
            roots,
            req.completed_steps.unwrap_or_default(),
        )
        .await
    {
        Ok(job_id) => (
            StatusCode::ACCEPTED,
            Json(storage_plan_response(&req.operation, &plan, Some(job_id))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::to_value(ApiError::bad_request(e)).unwrap()),
        )
            .into_response(),
    }
}

fn build_storage_plan(req: &StoragePlanRequest, preview: bool) -> Result<StoragePlan, String> {
    let dry_run = if preview {
        true
    } else {
        req.dry_run.unwrap_or(false)
    };
    match normalize_storage_operation(&req.operation).as_deref() {
        Some("move") => Ok(rt_storage::plan_move(&MovePlanRequest {
            source: req
                .source
                .clone()
                .ok_or_else(|| "source is required for move".to_owned())?,
            destination: req
                .destination
                .clone()
                .ok_or_else(|| "destination is required for move".to_owned())?,
            bytes: req.bytes.unwrap_or(0),
            available_bytes: req.available_bytes,
            dry_run,
        })),
        Some("import") => Ok(rt_storage::plan_import(&ImportPlanRequest {
            source: req
                .source
                .clone()
                .ok_or_else(|| "source is required for import".to_owned())?,
            destination: req
                .destination
                .clone()
                .ok_or_else(|| "destination is required for import".to_owned())?,
            bytes: req.bytes.unwrap_or(0),
            available_bytes: req.available_bytes,
            hardlink_or_copy: req.hardlink_or_copy.unwrap_or(false),
            dry_run,
        })),
        Some("delete") => Ok(rt_storage::plan_delete(&DeletePlanRequest {
            target: req
                .target
                .clone()
                .ok_or_else(|| "target is required for delete".to_owned())?,
            bytes: req.bytes.unwrap_or(0),
            dry_run,
            dry_run_approved: req.dry_run_approved.unwrap_or(!dry_run),
        })),
        _ => Err("operation must be one of move, import, or delete".to_owned()),
    }
}

fn validate_storage_plan_roots(
    plan: &StoragePlan,
    roots: Option<&[PathBuf]>,
) -> Option<axum::response::Response> {
    let roots = roots?;
    if roots.is_empty() {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ApiError::bad_request(
                        "roots must not be empty".to_owned(),
                    ))
                    .unwrap(),
                ),
            )
                .into_response(),
        );
    }
    let mut dry_run_plan = plan.clone();
    dry_run_plan.dry_run = true;
    match rt_storage::execute_storage_plan_under_roots(&dry_run_plan, roots) {
        Ok(_) => None,
        Err(e) => Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(ApiError::bad_request(e.to_string())).unwrap()),
            )
                .into_response(),
        ),
    }
}

fn storage_plan_response(
    operation: &str,
    plan: &StoragePlan,
    job_id: Option<String>,
) -> StoragePlanResponse {
    StoragePlanResponse {
        operation: normalize_storage_operation(operation)
            .unwrap_or_else(|| operation.to_ascii_lowercase()),
        job_id,
        plan: StoragePlanView::from_plan(plan),
    }
}

impl StoragePlanView {
    fn from_plan(plan: &StoragePlan) -> Self {
        Self {
            dry_run: plan.dry_run,
            can_apply: plan.can_apply,
            issues: plan.issues.iter().map(storage_plan_issue_label).collect(),
            steps: plan
                .steps
                .iter()
                .map(StoragePlanStepView::from_step)
                .collect(),
            rollback_steps: plan
                .rollback_steps
                .iter()
                .map(StoragePlanStepView::from_step)
                .collect(),
        }
    }
}

impl StoragePlanStepView {
    fn from_step(step: &StoragePlanStep) -> Self {
        Self {
            action: storage_plan_step_action_label(&step.action).to_owned(),
            source: step.source.as_ref().map(|path| path.display().to_string()),
            destination: step
                .destination
                .as_ref()
                .map(|path| path.display().to_string()),
            bytes: step.bytes,
        }
    }
}

fn normalize_storage_operation(operation: &str) -> Option<String> {
    match operation.trim().to_ascii_lowercase().as_str() {
        "move" => Some("move".to_owned()),
        "import" => Some("import".to_owned()),
        "delete" => Some("delete".to_owned()),
        _ => None,
    }
}

fn storage_plan_step_action_label(action: &PlannedStorageAction) -> &'static str {
    match action {
        PlannedStorageAction::ImportExisting => "import_existing",
        PlannedStorageAction::Rename => "rename",
        PlannedStorageAction::CopyVerifyRename => "copy_verify_rename",
        PlannedStorageAction::SafeDelete => "safe_delete",
    }
}

fn storage_plan_issue_label(issue: &PlanIssue) -> String {
    match issue {
        PlanIssue::SourceMissing(path) => format!("source missing: {}", path.display()),
        PlanIssue::DestinationExists(path) => format!("destination exists: {}", path.display()),
        PlanIssue::InsufficientCapacity { needed, available } => {
            format!("insufficient capacity: needed {needed}, available {available}")
        }
        PlanIssue::DeleteRequiresDryRunApproval => {
            "delete requires execute confirmation".to_owned()
        }
    }
}

/// `POST /api/v1/torrents/{hash}/pause` — pause a torrent.
pub async fn pause_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    control_torrent(state, headers, info_hash, TorrentControl::Pause).await
}

/// `POST /api/v1/torrents/{hash}/resume` — resume a torrent.
pub async fn resume_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    control_torrent(state, headers, info_hash, TorrentControl::Resume).await
}

/// `POST /api/v1/torrents/{hash}/recheck` — force a piece hash recheck.
pub async fn recheck_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    control_torrent(state, headers, info_hash, TorrentControl::Recheck).await
}

/// `POST /api/v1/torrents/{hash}/reannounce` — force tracker announce.
pub async fn reannounce_torrent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    control_torrent(state, headers, info_hash, TorrentControl::Reannounce).await
}

/// `GET /api/v1/torrents/{hash}/diagnostics` — explain why a torrent is not seeding.
pub async fn diagnose_torrent(
    State(state): State<AppState>,
    Path(info_hash): Path<String>,
) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ApiError::internal(
                    "native engine is not available".to_owned(),
                ))
                .unwrap(),
            ),
        )
            .into_response();
    };
    match engine.diagnose_torrent(info_hash.clone()).await {
        Ok(diagnostic) => (
            StatusCode::OK,
            Json(serde_json::to_value(diagnostic).unwrap()),
        )
            .into_response(),
        Err(_) => not_found(info_hash),
    }
}

/// `GET /health` — native engine readiness probe.
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let torrent_count = state.registry.read().await.iter().count();
    let ready = state.engine.is_some();
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(serde_json::json!({
            "status": if ready { "ok" } else { "unavailable" },
            "ready": ready,
            "native_engine": ready,
            "torrent_count": torrent_count,
            "engine": {
                "mode": if ready { "native" } else { "unavailable" },
                "source_of_truth": if ready { "sqlite_session_db" } else { "registry_only" },
                "track1_sidecar_required": false,
                "capabilities": native_engine_capabilities(),
            },
        })),
    )
}

fn native_engine_capabilities() -> serde_json::Value {
    serde_json::json!({
        "torrent_identity": {
            "v1": true,
            "v2": true,
            "hybrid": true,
            "hash_lengths": [40, 64],
            "magnet_xt": ["btih", "btmh"],
        },
        "metadata": {
            "torrent_files": true,
            "magnets": true,
            "pure_v2_metadata_placeholders": true,
            "pure_v2_metadata_completion": true,
        },
        "session": {
            "durable_torrents": true,
            "durable_files": true,
            "durable_trackers": true,
            "durable_limits": true,
            "durable_labels": true,
            "event_log": true,
            "crash_restore": true,
        },
        "jobs": {
            "durable_recheck": true,
            "pause_resume_cancel": true,
            "crash_recovery": true,
            "storage_throttled": true,
        },
        "storage": {
            "root_registry": true,
            "mount_identity": true,
            "dry_run_import": true,
            "safe_move": true,
            "safe_delete_after_dry_run": true,
            "v2_file_root_verify": true,
        },
        "networking": {
            "tcp_peer_wire": true,
            "http_trackers": true,
            "udp_trackers": true,
            "dht": true,
            "utp": true,
            "private_torrent_dht_pex_lsd_default_off": true,
        },
        "compatibility": {
            "native_rest": true,
            "native_sse": true,
            "qbittorrent_v2": true,
            "transmission_rpc": true,
            "deluge_rpc": true,
        },
        "migration": {
            "rtorrent": true,
            "qbittorrent": true,
            "transmission": true,
            "dry_run_reports": true,
            "atomic_db_import": true,
        },
        "operations": {
            "prometheus_metrics": true,
            "diagnostics": true,
            "bounded_shutdown": true,
            "api_token_auth": true,
            "scale_certification": true,
        },
    })
}

/// `GET /metrics` — Prometheus text exposition for native engine state.
pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            "# native engine unavailable\n".to_owned(),
        );
    };
    match engine.stats().await {
        Ok(stats) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            render_metrics(&stats),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            format!("# failed to collect native engine metrics: {e}\n"),
        ),
    }
}

/// `GET /api/v1/events` — server-sent torrent delta stream.
pub async fn stream_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = futures::stream::unfold(
        EventStreamState::new(state),
        |mut stream_state| async move {
            loop {
                stream_state.tick.tick().await;
                let delta = torrent_delta(&stream_state.state, &stream_state.previous).await;
                if delta.torrents.is_empty() && delta.removed.is_empty() {
                    continue;
                }

                stream_state.seq = stream_state.seq.saturating_add(1);
                stream_state.previous = delta.current;
                let payload = serde_json::json!({
                    "seq": stream_state.seq,
                    "torrents": delta.torrents,
                    "removed": delta.removed,
                });
                let event = Event::default()
                    .event("torrent_delta")
                    .id(stream_state.seq.to_string())
                    .json_data(payload)
                    .expect("torrent delta serializes");
                return Some((Ok(event), stream_state));
            }
        },
    );

    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Debug, Deserialize)]
pub struct SessionEventsQuery {
    limit: Option<usize>,
    torrent: Option<String>,
    kind: Option<String>,
    level: Option<String>,
    last_known_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SessionEventResponse {
    id: i64,
    timestamp: i64,
    torrent: Option<String>,
    kind: String,
    level: String,
    message: Option<String>,
    payload: serde_json::Value,
}

/// `GET /api/v1/session-events` — recent durable engine events.
pub async fn list_session_events(
    State(state): State<AppState>,
    Query(query): Query<SessionEventsQuery>,
) -> impl IntoResponse {
    let Some(engine) = &state.engine else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Vec::<SessionEventResponse>::new()),
        );
    };
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let levels = query.level.iter().cloned().collect::<Vec<_>>();
    match engine
        .session_events_filtered(
            query.torrent.clone(),
            query.kind.clone(),
            levels,
            query.last_known_id,
            limit,
        )
        .await
    {
        Ok(events) => {
            let events = events
                .into_iter()
                .filter_map(session_event_response)
                .collect::<Vec<_>>();
            (StatusCode::OK, Json(events))
        }
        Err(e) => {
            tracing::warn!(component = "api", operation = "session_events", error = %e, "failed to list session events");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Vec::<SessionEventResponse>::new()),
            )
        }
    }
}

fn session_event_response(row: rt_db::SessionEventRow) -> Option<SessionEventResponse> {
    let payload = serde_json::from_str::<serde_json::Value>(&row.payload)
        .unwrap_or_else(|_| serde_json::json!({}));
    let level = payload
        .get("level")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| level_from_kind(&row.kind).to_owned());
    Some(SessionEventResponse {
        id: row.event_id.unwrap_or_default(),
        timestamp: row.occurred_at,
        torrent: row.info_hash,
        kind: row.kind,
        level,
        message: row.message,
        payload,
    })
}

fn level_from_kind(kind: &str) -> &'static str {
    let lower = kind.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("failed") {
        "error"
    } else if lower.contains("warn") {
        "warn"
    } else {
        "info"
    }
}

struct EventStreamState {
    state: AppState,
    previous: BTreeMap<String, String>,
    seq: u64,
    tick: tokio::time::Interval,
}

impl EventStreamState {
    fn new(state: AppState) -> Self {
        EventStreamState {
            state,
            previous: BTreeMap::new(),
            seq: 0,
            tick: tokio::time::interval(Duration::from_secs(1)),
        }
    }
}

struct TorrentDelta {
    current: BTreeMap<String, String>,
    torrents: Vec<TorrentSummary>,
    removed: Vec<String>,
}

async fn torrent_delta(state: &AppState, previous: &BTreeMap<String, String>) -> TorrentDelta {
    let _lease = if let Some(engine) = &state.engine {
        let torrent_count = state.registry.read().await.iter().count();
        match engine
            .reserve_memory(
                MemoryClass::ApiSnapshot,
                estimate_torrent_delta_snapshot_bytes(torrent_count),
            )
            .await
        {
            Ok(Some(lease)) => Some(lease),
            Ok(None) | Err(_) => {
                return TorrentDelta {
                    current: BTreeMap::new(),
                    torrents: Vec::new(),
                    removed: Vec::new(),
                };
            }
        }
    } else {
        None
    };

    let reg = state.registry.read().await;
    let mut current = BTreeMap::new();
    let mut torrents = Vec::new();
    for entry in reg.iter() {
        let summary = torrent_summary(entry);
        let encoded = serde_json::to_string(&summary).expect("torrent summary serializes");
        if previous.get(&summary.info_hash) != Some(&encoded) {
            torrents.push(summary);
        }
        current.insert(entry.info_hash.clone(), encoded);
    }
    let removed = previous
        .keys()
        .filter(|hash| !current.contains_key(*hash))
        .cloned()
        .collect();

    TorrentDelta {
        current,
        torrents,
        removed,
    }
}

fn estimate_torrent_summary_snapshot_bytes(torrent_count: usize) -> u64 {
    // Conservative enough to cover Vec growth and cloned strings for typical
    // summaries without letting a huge API snapshot bypass governor pressure.
    (torrent_count as u64).saturating_mul(1024)
}

fn estimate_torrent_delta_snapshot_bytes(torrent_count: usize) -> u64 {
    (torrent_count as u64).saturating_mul(1536)
}

fn estimate_torrent_detail_base_snapshot_bytes() -> u64 {
    64 * 1024
}

fn estimate_torrent_detail_snapshot_bytes(
    summary: &TorrentSummary,
    meta: &rt_engine::EngineTorrentMetadata,
) -> u64 {
    let summary_strings = summary
        .info_hash
        .len()
        .saturating_add(summary.name.len())
        .saturating_add(summary.state.len())
        .saturating_add(summary.save_path.len())
        .saturating_add(summary.category.as_ref().map_or(0, String::len))
        .saturating_add(summary.tags.iter().map(String::len).sum::<usize>());
    let tracker_strings = meta.trackers.iter().map(String::len).sum::<usize>();
    let webseed_strings = meta.webseeds.iter().map(String::len).sum::<usize>();
    let file_strings = meta.files.iter().map(|file| file.path.len()).sum::<usize>();
    let structured = 2048usize
        .saturating_add(meta.trackers.len().saturating_mul(256))
        .saturating_add(meta.webseeds.len().saturating_mul(256))
        .saturating_add(meta.files.len().saturating_mul(512))
        .saturating_add(meta.piece_hashes.len().saturating_mul(64))
        .saturating_add(meta.piece_states.len().saturating_mul(8));
    structured
        .saturating_add(summary_strings)
        .saturating_add(tracker_strings)
        .saturating_add(webseed_strings)
        .saturating_add(file_strings)
        .max(estimate_torrent_detail_base_snapshot_bytes() as usize) as u64
}

fn torrent_summary(e: &TorrentEntry) -> TorrentSummary {
    TorrentSummary {
        info_hash: e.info_hash.clone(),
        name: e.name.clone(),
        state: e.state.as_str().to_owned(),
        total_length: e.total_length as i64,
        downloaded: e.stats.downloaded as i64,
        uploaded: e.stats.uploaded as i64,
        ratio: e.stats.ratio(),
        save_path: e.save_path.clone(),
        category: e.category.clone(),
        tags: e.tags.clone(),
        added_at: e.added_at as i64,
        completed_at: e.completed_at.map(|t| t as i64),
        num_peers: 0,
        num_seeds: 0,
    }
}

fn render_metrics(stats: &rt_engine::EngineStats) -> String {
    let mut out = String::new();
    metric(
        &mut out,
        "torrentng_torrents_total",
        "gauge",
        "Total torrents in session",
        stats.torrents_total,
    );
    metric(
        &mut out,
        "torrentng_torrents_seeding",
        "gauge",
        "Currently seeding torrents",
        stats.torrents_seeding,
    );
    metric(
        &mut out,
        "torrentng_torrents_downloading",
        "gauge",
        "Currently downloading torrents",
        stats.torrents_downloading,
    );
    metric(
        &mut out,
        "torrentng_torrents_paused",
        "gauge",
        "Paused or stopped torrents",
        stats.torrents_paused,
    );
    metric(
        &mut out,
        "torrentng_torrents_checking",
        "gauge",
        "Torrents checking pieces",
        stats.torrents_checking,
    );
    metric(
        &mut out,
        "torrentng_torrents_metadata_pending",
        "gauge",
        "Metadata-pending torrents",
        stats.torrents_metadata_pending,
    );
    metric(
        &mut out,
        "torrentng_torrents_queued",
        "gauge",
        "Queued torrents",
        stats.torrents_queued,
    );
    metric(
        &mut out,
        "torrentng_torrents_errored",
        "gauge",
        "Errored torrents",
        stats.torrents_error,
    );
    metric(
        &mut out,
        "torrentng_torrents_activity_hot",
        "gauge",
        "Torrents currently classified as hot by the activity tier policy",
        stats.torrents_activity_hot,
    );
    metric(
        &mut out,
        "torrentng_torrents_activity_warm",
        "gauge",
        "Torrents currently classified as warm by the activity tier policy",
        stats.torrents_activity_warm,
    );
    metric(
        &mut out,
        "torrentng_torrents_activity_dormant",
        "gauge",
        "Torrents currently classified as dormant by the activity tier policy",
        stats.torrents_activity_dormant,
    );
    metric(
        &mut out,
        "torrentng_torrent_tasks_active",
        "gauge",
        "Active per-torrent runtime tasks",
        stats.torrent_tasks_active,
    );
    metric(
        &mut out,
        "torrentng_fastresume_dirty_pieces",
        "gauge",
        "Pieces validated since the last completed durability barrier",
        stats.fastresume_dirty_pieces,
    );
    metric(
        &mut out,
        "torrentng_completed_piece_verify_from_memory_total",
        "counter",
        "Completed piece verifications hashed from assembled in-memory piece data",
        stats.completed_piece_verify_from_memory,
    );
    metric(
        &mut out,
        "torrentng_completed_piece_verify_from_disk_total",
        "counter",
        "Completed piece verifications that fell back to disk re-read",
        stats.completed_piece_verify_from_disk,
    );
    metric(
        &mut out,
        "torrentng_bytes_uploaded_total",
        "counter",
        "Uploaded bytes from session accounting",
        stats.bytes_uploaded,
    );
    metric(
        &mut out,
        "torrentng_bytes_downloaded_total",
        "counter",
        "Downloaded bytes from session accounting",
        stats.bytes_downloaded,
    );
    metric(
        &mut out,
        "torrentng_bytes_left",
        "gauge",
        "Bytes left across enabled torrent pieces",
        stats.bytes_left,
    );
    metric(
        &mut out,
        "torrentng_jobs_active",
        "gauge",
        "Active durable jobs",
        stats.jobs_active,
    );
    metric(
        &mut out,
        "torrentng_trackers_total",
        "gauge",
        "Persisted tracker rows",
        stats.trackers_total,
    );
    metric(
        &mut out,
        "torrentng_trackers_working",
        "gauge",
        "Trackers in working state",
        stats.trackers_working,
    );
    metric(
        &mut out,
        "torrentng_trackers_warning",
        "gauge",
        "Trackers with warning state",
        stats.trackers_warning,
    );
    metric(
        &mut out,
        "torrentng_trackers_error",
        "gauge",
        "Trackers with error state",
        stats.trackers_error,
    );
    metric(
        &mut out,
        "torrentng_dht_routing_nodes",
        "gauge",
        "DHT routing table nodes retained by the native DHT task",
        stats.dht_routing_nodes,
    );
    metric(
        &mut out,
        "torrentng_dht_announced_peer_sets",
        "gauge",
        "Info-hash peer sets retained from DHT announce_peer queries",
        stats.dht_announced_peer_sets,
    );
    metric(
        &mut out,
        "torrentng_dht_announced_peers",
        "gauge",
        "DHT announced peers retained across all info-hash peer sets",
        stats.dht_announced_peers,
    );
    metric(
        &mut out,
        "torrentng_dht_tracked_torrents",
        "gauge",
        "Torrents registered with the native DHT task",
        stats.dht_tracked_torrents,
    );
    metric(
        &mut out,
        "torrentng_dht_outstanding_requests",
        "gauge",
        "DHT KRPC requests currently awaiting responses",
        stats.dht_outstanding_requests,
    );
    metric(
        &mut out,
        "torrentng_dht_queried_nodes",
        "gauge",
        "DHT nodes remembered as queried across active lookups",
        stats.dht_queried_nodes,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_capacity",
        "gauge",
        "Configured open-file cache capacity across running torrent schedulers",
        stats.storage_file_pool_capacity,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_open_files",
        "gauge",
        "Open files across running torrent scheduler caches",
        stats.storage_file_pool_open_files,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_memory_bytes",
        "gauge",
        "Approximate memory used by open-file cache metadata across running torrent schedulers",
        stats.storage_file_pool_memory_bytes,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_hits_total",
        "counter",
        "Open-file cache hits across running torrent schedulers",
        stats.storage_file_pool_hits,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_misses_total",
        "counter",
        "Open-file cache misses across running torrent schedulers",
        stats.storage_file_pool_misses,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_evictions_total",
        "counter",
        "Open-file cache evictions across running torrent schedulers",
        stats.storage_file_pool_evictions,
    );
    metric(
        &mut out,
        "torrentng_storage_file_pool_idle_closes_total",
        "counter",
        "Idle open-file cache closes across running torrent schedulers",
        stats.storage_file_pool_idle_closes,
    );
    metric(
        &mut out,
        "torrentng_storage_io_queue_depth",
        "gauge",
        "Queued disk I/O jobs across running torrent schedulers",
        stats.storage_io_queue_depth,
    );
    metric(
        &mut out,
        "torrentng_storage_hash_queue_depth",
        "gauge",
        "Queued hashing jobs across running torrent schedulers",
        stats.storage_hash_queue_depth,
    );
    metric(
        &mut out,
        "torrentng_storage_queued_disk_bytes",
        "gauge",
        "Estimated process-owned bytes represented by queued disk and hash jobs",
        stats.storage_queued_disk_bytes,
    );
    metric(
        &mut out,
        "torrentng_storage_queue_full_total",
        "counter",
        "Disk or hash jobs denied because the per-mount storage queue was full",
        stats.storage_queue_full,
    );
    metric(
        &mut out,
        "torrentng_storage_dirty_files",
        "gauge",
        "Dirty files tracked by running torrent schedulers",
        stats.storage_dirty_files,
    );
    metric(
        &mut out,
        "torrentng_storage_read_ops_total",
        "counter",
        "Positioned read operations across running torrent schedulers",
        stats.storage_read_ops,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_read_ops_by_class_total",
        "counter",
        "Positioned read operations by I/O class across running torrent schedulers",
        &stats.storage_read_ops_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_write_ops_total",
        "counter",
        "Positioned write operations across running torrent schedulers",
        stats.storage_write_ops,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_write_ops_by_class_total",
        "counter",
        "Positioned write operations by I/O class across running torrent schedulers",
        &stats.storage_write_ops_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_bytes_read_total",
        "counter",
        "Bytes read through running torrent schedulers",
        stats.storage_bytes_read,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_bytes_read_by_class_total",
        "counter",
        "Bytes read by I/O class through running torrent schedulers",
        &stats.storage_bytes_read_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_bytes_written_total",
        "counter",
        "Bytes written through running torrent schedulers",
        stats.storage_bytes_written,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_bytes_written_by_class_total",
        "counter",
        "Bytes written by I/O class through running torrent schedulers",
        &stats.storage_bytes_written_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_read_ops_total",
        "counter",
        "Backend disk read operations across running torrent schedulers",
        stats.storage_backend_read_ops,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_backend_read_ops_by_class_total",
        "counter",
        "Backend disk read operations by I/O class across running torrent schedulers",
        &stats.storage_backend_read_ops_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_bytes_read_total",
        "counter",
        "Bytes read from backend disk operations across running torrent schedulers",
        stats.storage_backend_bytes_read,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_backend_bytes_read_by_class_total",
        "counter",
        "Bytes read from backend disk operations by I/O class across running torrent schedulers",
        &stats.storage_backend_bytes_read_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds_total",
        "counter",
        "Total read queue plus execution latency across running torrent schedulers",
        stats.storage_read_latency_ns,
    );
    latency_histogram(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds",
        "Read queue plus execution latency across running torrent schedulers",
        &stats.storage_read_latency_buckets,
        stats.storage_read_ops,
        stats.storage_read_latency_ns,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds_by_class_total",
        "counter",
        "Total read queue plus execution latency by I/O class",
        &stats.storage_read_latency_ns_by_class,
    );
    metric(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds_total",
        "counter",
        "Total write queue plus execution latency across running torrent schedulers",
        stats.storage_write_latency_ns,
    );
    latency_histogram(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds",
        "Write queue plus execution latency across running torrent schedulers",
        &stats.storage_write_latency_buckets,
        stats.storage_write_ops,
        stats.storage_write_latency_ns,
    );
    metric_by_class(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds_by_class_total",
        "counter",
        "Total write queue plus execution latency by I/O class",
        &stats.storage_write_latency_ns_by_class,
    );
    metric_by_device(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds_by_device_total",
        "counter",
        "Total read queue plus execution latency by storage device",
        &stats.storage_device_latencies,
        |device| device.read_latency_ns,
    );
    latency_histogram_by_device(
        &mut out,
        "torrentng_storage_read_latency_nanoseconds_by_device",
        "Read queue plus execution latency histogram by storage device",
        &stats.storage_device_latencies,
        |device| &device.read_latency_buckets,
        |device| device.read_latency_ns,
    );
    metric_by_device(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds_by_device_total",
        "counter",
        "Total write queue plus execution latency by storage device",
        &stats.storage_device_latencies,
        |device| device.write_latency_ns,
    );
    latency_histogram_by_device(
        &mut out,
        "torrentng_storage_write_latency_nanoseconds_by_device",
        "Write queue plus execution latency histogram by storage device",
        &stats.storage_device_latencies,
        |device| &device.write_latency_buckets,
        |device| device.write_latency_ns,
    );
    metric_by_device(
        &mut out,
        "torrentng_storage_sync_latency_nanoseconds_by_device_total",
        "counter",
        "Total sync queue plus execution latency by storage device",
        &stats.storage_device_latencies,
        |device| device.sync_latency_ns,
    );
    latency_histogram_by_device(
        &mut out,
        "torrentng_storage_sync_latency_nanoseconds_by_device",
        "Sync queue plus execution latency histogram by storage device",
        &stats.storage_device_latencies,
        |device| &device.sync_latency_buckets,
        |device| device.sync_latency_ns,
    );
    metric_by_device(
        &mut out,
        "torrentng_storage_hash_latency_nanoseconds_by_device_total",
        "counter",
        "Total hashing queue plus execution latency by storage device",
        &stats.storage_device_latencies,
        |device| device.hash_latency_ns,
    );
    latency_histogram_by_device(
        &mut out,
        "torrentng_storage_hash_latency_nanoseconds_by_device",
        "Hashing queue plus execution latency histogram by storage device",
        &stats.storage_device_latencies,
        |device| &device.hash_latency_buckets,
        |device| device.hash_latency_ns,
    );
    metric(
        &mut out,
        "torrentng_storage_sync_latency_nanoseconds_total",
        "counter",
        "Total sync queue plus execution latency across running torrent schedulers",
        stats.storage_sync_latency_ns,
    );
    latency_histogram(
        &mut out,
        "torrentng_storage_sync_latency_nanoseconds",
        "Sync queue plus execution latency across running torrent schedulers",
        &stats.storage_sync_latency_buckets,
        stats.storage_sync_ops,
        stats.storage_sync_latency_ns,
    );
    metric(
        &mut out,
        "torrentng_storage_hash_latency_nanoseconds_total",
        "counter",
        "Total hashing queue plus execution latency across running torrent schedulers",
        stats.storage_hash_latency_ns,
    );
    latency_histogram(
        &mut out,
        "torrentng_storage_hash_latency_nanoseconds",
        "Hashing queue plus execution latency across running torrent schedulers",
        &stats.storage_hash_latency_buckets,
        stats.storage_hash_ops,
        stats.storage_hash_latency_ns,
    );
    metric(
        &mut out,
        "torrentng_storage_sync_ops_total",
        "counter",
        "Data sync operations across running torrent schedulers",
        stats.storage_sync_ops,
    );
    metric(
        &mut out,
        "torrentng_storage_hash_ops_total",
        "counter",
        "Hashing operations across running torrent schedulers",
        stats.storage_hash_ops,
    );
    metric(
        &mut out,
        "torrentng_storage_preallocation_failures_total",
        "counter",
        "Preallocation failures across running torrent schedulers",
        stats.storage_preallocation_failures,
    );
    metric(
        &mut out,
        "torrentng_storage_preallocation_fallbacks_total",
        "counter",
        "Preallocation fallback events across running torrent schedulers",
        stats.storage_preallocation_fallbacks,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_cache_entries",
        "gauge",
        "Peer-read readahead cache entries across running torrent schedulers",
        stats.storage_peer_read_cache_entries,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_cache_hits_total",
        "counter",
        "Peer-read readahead cache hits across running torrent schedulers",
        stats.storage_peer_read_cache_hits,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_cache_misses_total",
        "counter",
        "Peer-read readahead cache misses across running torrent schedulers",
        stats.storage_peer_read_cache_misses,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_cache_evictions_total",
        "counter",
        "Peer-read readahead cache evictions across running torrent schedulers",
        stats.storage_peer_read_cache_evictions,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_enabled",
        "gauge",
        "Running torrent schedulers with peer-read elevator enabled",
        stats.storage_peer_read_elevator_enabled,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_queue_depth",
        "gauge",
        "Configured peer-read elevator queue depth across running torrent schedulers",
        stats.storage_peer_read_elevator_queue_depth,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_queued",
        "gauge",
        "Currently queued peer-read elevator requests across running torrent schedulers",
        stats.storage_peer_read_elevator_queued,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_queue_full_total",
        "counter",
        "Peer-read elevator requests denied because the elevator queue was full",
        stats.storage_peer_read_elevator_queue_full,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_batches_total",
        "counter",
        "Peer-read elevator backend batches across running torrent schedulers",
        stats.storage_peer_read_elevator_batches,
    );
    metric(
        &mut out,
        "torrentng_storage_peer_read_elevator_coalesced_requests_total",
        "counter",
        "Peer-read elevator logical requests coalesced into existing backend batches",
        stats.storage_peer_read_elevator_coalesced_requests,
    );
    metric(
        &mut out,
        "torrentng_storage_page_cache_advise_sequential_total",
        "counter",
        "Successful POSIX_FADV_SEQUENTIAL hints issued by storage schedulers",
        stats.storage_page_cache_advise_sequential,
    );
    metric(
        &mut out,
        "torrentng_storage_page_cache_advise_willneed_total",
        "counter",
        "Successful POSIX_FADV_WILLNEED hints issued by storage schedulers",
        stats.storage_page_cache_advise_willneed,
    );
    metric(
        &mut out,
        "torrentng_storage_page_cache_advise_dontneed_total",
        "counter",
        "Successful POSIX_FADV_DONTNEED hints issued by storage schedulers",
        stats.storage_page_cache_advise_dontneed,
    );
    metric(
        &mut out,
        "torrentng_storage_page_cache_advise_failures_total",
        "counter",
        "Failed page-cache advice calls observed by storage schedulers",
        stats.storage_page_cache_advise_failures,
    );
    metric(
        &mut out,
        "torrentng_storage_sparse_data_extents_total",
        "counter",
        "Sparse-file data extents discovered during storage recheck",
        stats.storage_sparse_data_extents,
    );
    metric(
        &mut out,
        "torrentng_storage_sparse_hole_bytes_total",
        "counter",
        "Sparse-file hole bytes skipped during storage recheck",
        stats.storage_sparse_hole_bytes,
    );
    metric(
        &mut out,
        "torrentng_storage_sparse_seek_fallbacks_total",
        "counter",
        "Sparse-file extent probes that fell back to contiguous reads",
        stats.storage_sparse_seek_fallbacks,
    );
    metric(
        &mut out,
        "torrentng_piece_assembly_buffers",
        "gauge",
        "In-memory piece assembly buffers across running torrents",
        stats.piece_assembly_buffers,
    );
    metric(
        &mut out,
        "torrentng_piece_assembly_bytes",
        "gauge",
        "In-memory piece assembly bytes across running torrents",
        stats.piece_assembly_bytes,
    );
    metric(
        &mut out,
        "torrentng_piece_assembly_evictions_total",
        "counter",
        "In-memory piece assembly buffers evicted by torrent-task budgets",
        stats.piece_assembly_evictions,
    );
    metric(
        &mut out,
        "torrentng_peer_request_window_reductions_total",
        "counter",
        "Peer request refill windows reduced because memory pressure limited in-flight piece data",
        stats.peer_request_window_reductions,
    );
    metric(
        &mut out,
        "torrentng_peer_rx_buffer_bytes",
        "gauge",
        "Expected peer receive buffer bytes reserved by outstanding block requests",
        stats.peer_rx_buffer_bytes,
    );
    metric(
        &mut out,
        "torrentng_peer_tx_buffer_bytes",
        "gauge",
        "Peer upload buffer bytes currently owned by torrent tasks",
        stats.peer_tx_buffer_bytes,
    );
    metric(
        &mut out,
        "torrentng_peer_command_queue_depth",
        "gauge",
        "Queued peer commands across active peer tasks",
        stats.peer_command_queue_depth,
    );
    metric(
        &mut out,
        "torrentng_peer_command_queue_capacity",
        "gauge",
        "Total peer command queue capacity across active peer tasks",
        stats.peer_command_queue_capacity,
    );
    metric(
        &mut out,
        "torrentng_peer_command_queue_full_total",
        "counter",
        "Nonblocking peer command sends denied because peer command queues were full",
        stats.peer_command_queue_full,
    );
    metric(
        &mut out,
        "torrentng_tracker_peer_cache_entries",
        "gauge",
        "Tracker-discovered peer addresses retained across running torrents",
        stats.tracker_peer_cache_entries,
    );
    metric(
        &mut out,
        "torrentng_tracker_peer_cache_drops_total",
        "counter",
        "Tracker-discovered peer addresses dropped because bounded peer caches were full",
        stats.tracker_peer_cache_drops,
    );
    if let Some(resources) = &stats.resources {
        metric(
            &mut out,
            "torrentng_memory_cap_bytes",
            "gauge",
            "Configured process-owned memory cap enforced by the resource governor",
            resources.total_cap_bytes,
        );
        metric(
            &mut out,
            "torrentng_memory_used_bytes",
            "gauge",
            "Process-owned bytes currently leased through the resource governor",
            resources.total_used_bytes,
        );
        metric(
            &mut out,
            "torrentng_memory_pressure_state",
            "gauge",
            "Resource governor memory pressure state: normal=0 constrained=1 critical=2",
            match resources.pressure {
                rt_metrics::MemoryPressure::Normal => 0,
                rt_metrics::MemoryPressure::Constrained => 1,
                rt_metrics::MemoryPressure::Critical => 2,
            },
        );
        for class in resources.classes {
            metric_with_label(
                &mut out,
                "torrentng_memory_class_cap_bytes",
                "gauge",
                "Configured process-owned memory cap by resource class",
                "class",
                class.class.as_str(),
                class.cap_bytes,
            );
            metric_with_label(
                &mut out,
                "torrentng_memory_class_used_bytes",
                "gauge",
                "Process-owned bytes currently leased by resource class",
                "class",
                class.class.as_str(),
                class.used_bytes,
            );
            metric_with_label(
                &mut out,
                "torrentng_memory_class_denied_allocations_total",
                "counter",
                "Denied resource governor allocation attempts by resource class",
                "class",
                class.class.as_str(),
                class.denied_allocations,
            );
        }
    }
    for (rank, torrent) in stats.hot_torrent_memory_top.iter().enumerate() {
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_memory_estimated_bytes",
            "gauge",
            "Estimated process-owned memory attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.estimated_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_piece_assembly_bytes",
            "gauge",
            "Piece assembly bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.piece_assembly_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_peer_buffer_bytes",
            "gauge",
            "Peer rx/tx buffer bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.peer_buffer_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_tracker_peer_bytes",
            "gauge",
            "Tracker peer-cache bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.tracker_peer_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_peer_command_queue_bytes",
            "gauge",
            "Peer command queue bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.peer_command_queue_bytes,
        );
        metric_with_two_labels(
            &mut out,
            "torrentng_hot_torrent_storage_cache_bytes",
            "gauge",
            "Per-torrent storage cache bytes attributed to top active torrents",
            ("rank", &(rank + 1).to_string()),
            ("info_hash", &torrent.info_hash),
            torrent.storage_cache_bytes,
        );
    }
    let storage = StorageRuntime::global();
    metric_with_label(
        &mut out,
        "torrentng_storage_backend_selected",
        "gauge",
        "Selected global storage backend; value is 1 for the active backend",
        "backend",
        storage.backend_kind().as_str(),
        1,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_fixed_buffers_supported",
        "gauge",
        "Whether the selected storage backend supports fixed registered buffers",
        u64::from(storage.backend_supports_fixed_buffers()),
    );
    metric(
        &mut out,
        "torrentng_storage_backend_registered_files_supported",
        "gauge",
        "Whether the selected storage backend supports registered file slots",
        u64::from(storage.backend_supports_registered_files()),
    );
    metric(
        &mut out,
        "torrentng_storage_backend_max_batch_len",
        "gauge",
        "Maximum number of storage backend jobs submitted as one batch",
        storage.backend_max_batch_len() as u64,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_fixed_buffer_bytes",
        "gauge",
        "Bytes in each registered fixed buffer for the selected storage backend",
        storage.backend_fixed_buffer_len() as u64,
    );
    let fixed_buffer_strategy = storage.backend_fixed_buffer_strategy();
    metric_with_label(
        &mut out,
        "torrentng_storage_backend_fixed_buffer_strategy",
        "gauge",
        "Selected fixed-buffer strategy; value is 1 for the active strategy",
        "strategy",
        fixed_buffer_strategy.as_str(),
        1,
    );
    metric(
        &mut out,
        "torrentng_storage_backend_fixed_buffer_worker_copy",
        "gauge",
        "Whether fixed-buffer submissions copy through backend-private worker buffers",
        u64::from(fixed_buffer_strategy.uses_worker_copy()),
    );
    metric(
        &mut out,
        "torrentng_storage_backend_frame_pool_slots_supported",
        "gauge",
        "Whether fixed-buffer submissions use registered storage frame-pool slots directly",
        u64::from(fixed_buffer_strategy.uses_frame_pool_slots()),
    );
    metric(
        &mut out,
        "torrentng_storage_handles_open",
        "gauge",
        "Open file handles in the global storage runtime cache",
        storage.handles_open() as u64,
    );
    metric(
        &mut out,
        "torrentng_storage_frame_bytes_in_use",
        "gauge",
        "Frame-pool bytes currently checked out by storage I/O",
        storage.frame_in_use_bytes(),
    );
    metric(
        &mut out,
        "torrentng_storage_frame_bytes_cap",
        "gauge",
        "Frame-pool byte cap for storage I/O",
        storage.frame_cap_bytes(),
    );
    out
}

fn metric(out: &mut String, name: &str, kind: &str, help: &str, value: u64) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
    out.push_str(name);
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

fn metric_by_class(out: &mut String, name: &str, kind: &str, help: &str, values: &[u64; 6]) {
    const IO_CLASSES: [&str; 6] = [
        "metadata",
        "recheck",
        "move_copy",
        "peer_write",
        "peer_read",
        "foreground",
    ];

    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
    for (class, value) in IO_CLASSES.iter().zip(values) {
        out.push_str(name);
        out.push_str("{class=\"");
        out.push_str(class);
        out.push_str("\"} ");
        out.push_str(&value.to_string());
        out.push('\n');
    }
}

fn metric_with_label(
    out: &mut String,
    name: &str,
    kind: &str,
    help: &str,
    label: &str,
    label_value: &str,
    value: u64,
) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
    out.push_str(name);
    out.push('{');
    out.push_str(label);
    out.push_str("=\"");
    push_label_value(out, label_value);
    out.push_str("\"} ");
    out.push_str(&value.to_string());
    out.push('\n');
}

fn metric_with_two_labels(
    out: &mut String,
    name: &str,
    kind: &str,
    help: &str,
    first: (&str, &str),
    second: (&str, &str),
    value: u64,
) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
    out.push_str(name);
    out.push('{');
    out.push_str(first.0);
    out.push_str("=\"");
    push_label_value(out, first.1);
    out.push_str("\",");
    out.push_str(second.0);
    out.push_str("=\"");
    push_label_value(out, second.1);
    out.push_str("\"} ");
    out.push_str(&value.to_string());
    out.push('\n');
}

fn push_label_value(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
}

fn metric_by_device(
    out: &mut String,
    name: &str,
    kind: &str,
    help: &str,
    values: &[rt_engine::StorageDeviceLatencyStats],
    value: impl Fn(&rt_engine::StorageDeviceLatencyStats) -> u64,
) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
    for device in values {
        out.push_str(name);
        out.push_str("{device=\"");
        push_label_value(out, &device.device_id);
        out.push_str("\",profile=\"");
        push_label_value(out, &device.profile);
        out.push_str("\"} ");
        out.push_str(&value(device).to_string());
        out.push('\n');
    }
}

fn latency_histogram(
    out: &mut String,
    name: &str,
    help: &str,
    buckets: &[u64; STORAGE_LATENCY_BUCKETS_NS.len()],
    count: u64,
    sum_ns: u64,
) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" histogram\n");
    for (upper_bound, count) in STORAGE_LATENCY_BUCKETS_NS.iter().zip(buckets) {
        out.push_str(name);
        out.push_str("_bucket{le=\"");
        if *upper_bound == u64::MAX {
            out.push_str("+Inf");
        } else {
            out.push_str(&upper_bound.to_string());
        }
        out.push_str("\"} ");
        out.push_str(&count.to_string());
        out.push('\n');
    }
    out.push_str(name);
    out.push_str("_sum ");
    out.push_str(&sum_ns.to_string());
    out.push('\n');
    out.push_str(name);
    out.push_str("_count ");
    out.push_str(&count.to_string());
    out.push('\n');
}

fn latency_histogram_by_device(
    out: &mut String,
    name: &str,
    help: &str,
    values: &[rt_engine::StorageDeviceLatencyStats],
    buckets: impl Fn(&rt_engine::StorageDeviceLatencyStats) -> &[u64; STORAGE_LATENCY_BUCKETS_NS.len()],
    sum_ns: impl Fn(&rt_engine::StorageDeviceLatencyStats) -> u64,
) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" histogram\n");
    for device in values {
        let buckets = buckets(device);
        for (upper_bound, count) in STORAGE_LATENCY_BUCKETS_NS.iter().zip(buckets) {
            out.push_str(name);
            out.push_str("_bucket{device=\"");
            push_label_value(out, &device.device_id);
            out.push_str("\",profile=\"");
            push_label_value(out, &device.profile);
            out.push_str("\",le=\"");
            if *upper_bound == u64::MAX {
                out.push_str("+Inf");
            } else {
                out.push_str(&upper_bound.to_string());
            }
            out.push_str("\"} ");
            out.push_str(&count.to_string());
            out.push('\n');
        }
        out.push_str(name);
        out.push_str("_sum{device=\"");
        push_label_value(out, &device.device_id);
        out.push_str("\",profile=\"");
        push_label_value(out, &device.profile);
        out.push_str("\"} ");
        out.push_str(&sum_ns(device).to_string());
        out.push('\n');
        out.push_str(name);
        out.push_str("_count{device=\"");
        push_label_value(out, &device.device_id);
        out.push_str("\",profile=\"");
        push_label_value(out, &device.profile);
        out.push_str("\"} ");
        out.push_str(&buckets[STORAGE_LATENCY_BUCKETS_NS.len() - 1].to_string());
        out.push('\n');
    }
}

enum TorrentControl {
    Pause,
    Resume,
    Recheck,
    Reannounce,
}

async fn control_torrent(
    state: AppState,
    headers: HeaderMap,
    info_hash: String,
    control: TorrentControl,
) -> axum::response::Response {
    if let Some(response) = require_mutation_auth(&state, &headers) {
        return response;
    }
    if !torrent_exists(&state, &info_hash).await {
        return not_found(info_hash);
    }
    if let Some(engine) = &state.engine {
        let result = match control {
            TorrentControl::Pause => engine.pause_torrent(info_hash.clone()).await,
            TorrentControl::Resume => engine.resume_torrent(info_hash.clone()).await,
            TorrentControl::Recheck => engine.recheck_torrent(info_hash.clone()).await,
            TorrentControl::Reannounce => engine.reannounce_torrent(info_hash.clone()).await,
        };
        return match result {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => not_found(info_hash),
        };
    }

    let mut reg = state.registry.write().await;
    if let Some(entry) = reg.get_mut(&info_hash) {
        match control {
            TorrentControl::Pause => {
                let _ = entry.transition(TorrentState::Paused);
            }
            TorrentControl::Resume => {
                let _ = entry.transition(TorrentState::Downloading);
            }
            TorrentControl::Recheck => {
                let _ = entry.transition(TorrentState::Checking);
            }
            TorrentControl::Reannounce => {}
        }
        StatusCode::NO_CONTENT.into_response()
    } else {
        not_found(info_hash)
    }
}

fn require_mutation_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<axum::response::Response> {
    if state.api_tokens.is_empty()
        || presented_token(headers).is_some_and(|token| token_allowed(state, token))
    {
        return None;
    }
    Some(
        (
            StatusCode::UNAUTHORIZED,
            Json(
                serde_json::to_value(ApiError::new(
                    "UNAUTHORIZED",
                    "missing or invalid API token",
                ))
                .unwrap(),
            ),
        )
            .into_response(),
    )
}

fn token_allowed(state: &AppState, token: &str) -> bool {
    state.api_tokens.iter().any(|allowed| allowed == token)
}

fn presented_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(extract_session_cookie)
        })
}

fn extract_session_cookie(cookie: &str) -> Option<&str> {
    cookie.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("tng_session=")
    })
}

async fn torrent_exists(state: &AppState, info_hash: &str) -> bool {
    state.registry.read().await.get(info_hash).is_some()
}

fn not_found(info_hash: String) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(
            serde_json::to_value(ApiError::not_found(format!(
                "torrent {info_hash} not found"
            )))
            .unwrap(),
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use rt_session::{SessionRegistry, TorrentEntry};
    use tower::ServiceExt;

    use crate::router::build_router;

    async fn setup_app_with_torrent() -> (axum::Router, String) {
        let state = AppState::new();
        let hash = "a".repeat(40);
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.clone(),
                "my.torrent".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        (build_router(state), hash)
    }

    #[test]
    fn session_event_response_projects_level_and_payload() {
        let event = rt_db::SessionEventRow {
            event_id: Some(12),
            occurred_at: 1_700_000_000,
            info_hash: Some("a".repeat(40)),
            kind: "tracker_warning".to_owned(),
            message: Some("tracker warning".to_owned()),
            payload: r#"{"tracker":"udp://tracker","level":"warn"}"#.to_owned(),
        };

        let projected = session_event_response(event).unwrap();
        assert_eq!(projected.id, 12);
        assert_eq!(projected.level, "warn");
        assert_eq!(projected.kind, "tracker_warning");
        assert_eq!(projected.payload["tracker"], "udp://tracker");
    }

    #[test]
    fn level_from_kind_is_conservative() {
        assert_eq!(level_from_kind("torrent_added"), "info");
        assert_eq!(level_from_kind("tracker_warning"), "warn");
        assert_eq!(level_from_kind("storage_failed"), "error");
    }

    #[test]
    fn storage_plan_preview_projects_move_steps() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let req = StoragePlanRequest {
            operation: "move".to_owned(),
            source: Some(source),
            destination: Some(destination),
            target: None,
            bytes: Some(7),
            available_bytes: Some(7),
            hardlink_or_copy: None,
            dry_run: Some(true),
            dry_run_approved: None,
            affected_torrents: None,
            roots: None,
            completed_steps: None,
        };

        let plan = build_storage_plan(&req, true).unwrap();
        let response = storage_plan_response(&req.operation, &plan, None);

        assert_eq!(response.operation, "move");
        assert!(response.plan.can_apply);
        assert!(response.plan.dry_run);
        assert!(!response.plan.steps.is_empty());
        assert_eq!(response.plan.steps[0].action, "rename");
    }

    #[test]
    fn storage_plan_root_validation_rejects_escape() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("source.bin");
        let destination = allowed.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let req = StoragePlanRequest {
            operation: "move".to_owned(),
            source: Some(source),
            destination: Some(destination),
            target: None,
            bytes: Some(7),
            available_bytes: None,
            hardlink_or_copy: None,
            dry_run: Some(true),
            dry_run_approved: None,
            affected_torrents: None,
            roots: Some(vec![allowed.path().to_path_buf()]),
            completed_steps: None,
        };

        let plan = build_storage_plan(&req, true).unwrap();
        assert!(validate_storage_plan_roots(&plan, req.roots.as_deref()).is_some());
    }

    async fn setup_authed_app_with_torrent() -> (axum::Router, String) {
        let state = AppState {
            registry: std::sync::Arc::new(tokio::sync::RwLock::new(SessionRegistry::new())),
            engine: None,
            api_tokens: std::sync::Arc::new(vec!["secret-token".to_owned()]),
        };
        let hash = "b".repeat(40);
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.clone(),
                "secure.torrent".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        (build_router(state), hash)
    }

    #[tokio::test]
    async fn health_reports_unavailable_without_engine() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["ready"], false);
        assert_eq!(body["engine"]["mode"], "unavailable");
        assert_eq!(body["engine"]["track1_sidecar_required"], false);
        assert_eq!(
            body["engine"]["capabilities"]["torrent_identity"]["hash_lengths"],
            serde_json::json!([40, 64])
        );
        assert_eq!(
            body["engine"]["capabilities"]["compatibility"]["transmission_rpc"],
            true
        );
    }

    #[test]
    fn native_engine_capabilities_cover_rewrite_surface() {
        let capabilities = native_engine_capabilities();
        assert_eq!(capabilities["torrent_identity"]["v2"], true);
        assert_eq!(
            capabilities["metadata"]["pure_v2_metadata_completion"],
            true
        );
        assert_eq!(capabilities["session"]["crash_restore"], true);
        assert_eq!(capabilities["jobs"]["durable_recheck"], true);
        assert_eq!(capabilities["storage"]["v2_file_root_verify"], true);
        assert_eq!(capabilities["networking"]["dht"], true);
        assert_eq!(capabilities["compatibility"]["qbittorrent_v2"], true);
        assert_eq!(capabilities["migration"]["transmission"], true);
        assert_eq!(capabilities["operations"]["prometheus_metrics"], true);
    }

    #[tokio::test]
    async fn metrics_reports_unavailable_without_engine() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn diagnostics_without_engine_returns_unavailable() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/torrents/{hash}/diagnostics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn render_metrics_includes_engine_stats() {
        let mut stats = rt_engine::EngineStats {
            torrents_total: 2,
            torrents_seeding: 1,
            torrents_activity_hot: 2,
            torrents_activity_warm: 3,
            torrents_activity_dormant: 4,
            torrent_tasks_active: 5,
            fastresume_dirty_pieces: 6,
            completed_piece_verify_from_memory: 7,
            completed_piece_verify_from_disk: 8,
            jobs_active: 3,
            trackers_error: 4,
            dht_routing_nodes: 45,
            dht_announced_peer_sets: 46,
            dht_announced_peers: 47,
            dht_tracked_torrents: 48,
            dht_outstanding_requests: 49,
            dht_queried_nodes: 50,
            storage_file_pool_memory_bytes: 57,
            storage_file_pool_hits: 5,
            storage_read_ops: 6,
            storage_hash_ops: 7,
            storage_queued_disk_bytes: 54,
            storage_queue_full: 56,
            storage_peer_read_cache_hits: 8,
            storage_peer_read_cache_evictions: 58,
            piece_assembly_evictions: 9,
            storage_backend_read_ops: 14,
            storage_backend_bytes_read: 15,
            storage_read_latency_ns: 18,
            storage_write_latency_ns: 19,
            storage_sync_latency_ns: 20,
            storage_hash_latency_ns: 21,
            storage_peer_read_elevator_enabled: 24,
            storage_peer_read_elevator_queue_depth: 25,
            storage_peer_read_elevator_queued: 26,
            storage_peer_read_elevator_queue_full: 55,
            storage_peer_read_elevator_batches: 27,
            storage_peer_read_elevator_coalesced_requests: 28,
            storage_page_cache_advise_sequential: 29,
            storage_page_cache_advise_willneed: 30,
            storage_page_cache_advise_dontneed: 31,
            storage_page_cache_advise_failures: 32,
            storage_sparse_data_extents: 33,
            storage_sparse_hole_bytes: 34,
            storage_sparse_seek_fallbacks: 35,
            peer_request_window_reductions: 40,
            peer_rx_buffer_bytes: 41,
            peer_tx_buffer_bytes: 42,
            peer_command_queue_depth: 51,
            peer_command_queue_capacity: 52,
            peer_command_queue_full: 53,
            tracker_peer_cache_entries: 43,
            tracker_peer_cache_drops: 44,
            ..Default::default()
        };
        stats.storage_read_ops_by_class[4] = 10;
        stats.storage_write_ops_by_class[3] = 11;
        stats.storage_bytes_read_by_class[4] = 12;
        stats.storage_bytes_written_by_class[3] = 13;
        stats.storage_backend_read_ops_by_class[4] = 16;
        stats.storage_backend_bytes_read_by_class[4] = 17;
        stats.storage_read_latency_ns_by_class[4] = 22;
        stats.storage_write_latency_ns_by_class[3] = 23;
        stats.storage_read_latency_buckets[7] = 6;
        stats.storage_write_latency_buckets[7] = 11;
        stats.storage_sync_latency_buckets[7] = 2;
        stats.storage_hash_latency_buckets[7] = 7;
        stats.storage_device_latencies = vec![rt_engine::StorageDeviceLatencyStats {
            device_id: "pool\"a\\disk\n1".to_owned(),
            profile: "hdd".to_owned(),
            read_latency_ns: 36,
            write_latency_ns: 37,
            sync_latency_ns: 38,
            hash_latency_ns: 39,
            read_latency_buckets: {
                let mut buckets = [0; STORAGE_LATENCY_BUCKETS_NS.len()];
                buckets[7] = 6;
                buckets
            },
            write_latency_buckets: {
                let mut buckets = [0; STORAGE_LATENCY_BUCKETS_NS.len()];
                buckets[7] = 11;
                buckets
            },
            sync_latency_buckets: {
                let mut buckets = [0; STORAGE_LATENCY_BUCKETS_NS.len()];
                buckets[7] = 2;
                buckets
            },
            hash_latency_buckets: {
                let mut buckets = [0; STORAGE_LATENCY_BUCKETS_NS.len()];
                buckets[7] = 7;
                buckets
            },
        }];
        stats.hot_torrent_memory_top = vec![rt_engine::HotTorrentMemoryStats {
            info_hash: "abc\"def\\ghi\nj".to_owned(),
            estimated_bytes: 54,
            piece_assembly_bytes: 55,
            peer_buffer_bytes: 56,
            tracker_peer_bytes: 57,
            peer_command_queue_bytes: 58,
            storage_cache_bytes: 59,
        }];
        let governor = rt_metrics::ResourceGovernor::new(Default::default());
        assert!(governor
            .try_acquire(MemoryClass::ApiSnapshot, u64::MAX)
            .is_none());
        stats.resources = Some(governor.snapshot());
        let rendered = render_metrics(&stats);
        assert!(rendered.contains("torrentng_torrents_total 2"));
        assert!(rendered.contains("torrentng_torrents_seeding 1"));
        assert!(rendered.contains("torrentng_torrents_activity_hot 2"));
        assert!(rendered.contains("torrentng_torrents_activity_warm 3"));
        assert!(rendered.contains("torrentng_torrents_activity_dormant 4"));
        assert!(rendered.contains("torrentng_torrent_tasks_active 5"));
        assert!(rendered.contains("torrentng_fastresume_dirty_pieces 6"));
        assert!(rendered.contains("torrentng_completed_piece_verify_from_memory_total 7"));
        assert!(rendered.contains("torrentng_completed_piece_verify_from_disk_total 8"));
        assert!(rendered.contains("torrentng_jobs_active 3"));
        assert!(rendered.contains("torrentng_trackers_error 4"));
        assert!(rendered.contains("torrentng_dht_routing_nodes 45"));
        assert!(rendered.contains("torrentng_dht_announced_peer_sets 46"));
        assert!(rendered.contains("torrentng_dht_announced_peers 47"));
        assert!(rendered.contains("torrentng_dht_tracked_torrents 48"));
        assert!(rendered.contains("torrentng_dht_outstanding_requests 49"));
        assert!(rendered.contains("torrentng_dht_queried_nodes 50"));
        assert!(rendered.contains("torrentng_storage_file_pool_memory_bytes 57"));
        assert!(rendered.contains("torrentng_storage_file_pool_hits_total 5"));
        assert!(rendered.contains("torrentng_storage_read_ops_total 6"));
        assert!(rendered.contains("torrentng_storage_hash_ops_total 7"));
        assert!(rendered.contains("torrentng_storage_queued_disk_bytes 54"));
        assert!(rendered.contains("torrentng_storage_queue_full_total 56"));
        assert!(rendered.contains("torrentng_storage_peer_read_cache_hits_total 8"));
        assert!(rendered.contains("torrentng_storage_peer_read_cache_evictions_total 58"));
        assert!(rendered.contains("torrentng_piece_assembly_evictions_total 9"));
        assert!(rendered.contains("torrentng_peer_request_window_reductions_total 40"));
        assert!(rendered.contains("torrentng_peer_rx_buffer_bytes 41"));
        assert!(rendered.contains("torrentng_peer_tx_buffer_bytes 42"));
        assert!(rendered.contains("torrentng_peer_command_queue_depth 51"));
        assert!(rendered.contains("torrentng_peer_command_queue_capacity 52"));
        assert!(rendered.contains("torrentng_peer_command_queue_full_total 53"));
        assert!(rendered.contains("torrentng_tracker_peer_cache_entries 43"));
        assert!(rendered.contains("torrentng_tracker_peer_cache_drops_total 44"));
        assert!(rendered.contains(
            "torrentng_hot_torrent_memory_estimated_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 54"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_piece_assembly_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 55"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_peer_buffer_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 56"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_tracker_peer_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 57"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_peer_command_queue_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 58"
        ));
        assert!(rendered.contains(
            "torrentng_hot_torrent_storage_cache_bytes{rank=\"1\",info_hash=\"abc\\\"def\\\\ghi\\nj\"} 59"
        ));
        assert!(
            rendered.contains("torrentng_storage_read_ops_by_class_total{class=\"peer_read\"} 10")
        );
        assert!(rendered
            .contains("torrentng_storage_write_ops_by_class_total{class=\"peer_write\"} 11"));
        assert!(rendered
            .contains("torrentng_storage_bytes_read_by_class_total{class=\"peer_read\"} 12"));
        assert!(rendered
            .contains("torrentng_storage_bytes_written_by_class_total{class=\"peer_write\"} 13"));
        assert!(rendered.contains("torrentng_storage_backend_read_ops_total 14"));
        assert!(rendered.contains("torrentng_storage_backend_bytes_read_total 15"));
        assert!(rendered
            .contains("torrentng_storage_backend_read_ops_by_class_total{class=\"peer_read\"} 16"));
        assert!(rendered.contains(
            "torrentng_storage_backend_bytes_read_by_class_total{class=\"peer_read\"} 17"
        ));
        assert!(rendered.contains("torrentng_storage_read_latency_nanoseconds_total 18"));
        assert!(rendered.contains("torrentng_storage_write_latency_nanoseconds_total 19"));
        assert!(rendered.contains("torrentng_storage_sync_latency_nanoseconds_total 20"));
        assert!(rendered.contains("torrentng_storage_hash_latency_nanoseconds_total 21"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_enabled 24"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_queue_depth 25"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_queued 26"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_queue_full_total 55"));
        assert!(rendered.contains("torrentng_storage_peer_read_elevator_batches_total 27"));
        assert!(
            rendered.contains("torrentng_storage_peer_read_elevator_coalesced_requests_total 28")
        );
        assert!(rendered.contains("torrentng_storage_page_cache_advise_sequential_total 29"));
        assert!(rendered.contains("torrentng_storage_page_cache_advise_willneed_total 30"));
        assert!(rendered.contains("torrentng_storage_page_cache_advise_dontneed_total 31"));
        assert!(rendered.contains("torrentng_storage_page_cache_advise_failures_total 32"));
        assert!(rendered.contains("torrentng_storage_sparse_data_extents_total 33"));
        assert!(rendered.contains("torrentng_storage_sparse_hole_bytes_total 34"));
        assert!(rendered.contains("torrentng_storage_sparse_seek_fallbacks_total 35"));
        assert!(rendered.contains(
            "torrentng_storage_read_latency_nanoseconds_by_class_total{class=\"peer_read\"} 22"
        ));
        assert!(rendered.contains(
            "torrentng_storage_write_latency_nanoseconds_by_class_total{class=\"peer_write\"} 23"
        ));
        assert!(rendered.contains(
            "torrentng_storage_read_latency_nanoseconds_by_device_total{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 36"
        ));
        assert!(rendered.contains(
            "torrentng_storage_write_latency_nanoseconds_by_device_total{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 37"
        ));
        assert!(rendered.contains(
            "torrentng_storage_sync_latency_nanoseconds_by_device_total{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 38"
        ));
        assert!(rendered.contains(
            "torrentng_storage_hash_latency_nanoseconds_by_device_total{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 39"
        ));
        assert!(rendered.contains(
            "torrentng_storage_read_latency_nanoseconds_by_device_bucket{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\",le=\"+Inf\"} 6"
        ));
        assert!(rendered.contains(
            "torrentng_storage_write_latency_nanoseconds_by_device_bucket{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\",le=\"+Inf\"} 11"
        ));
        assert!(rendered.contains(
            "torrentng_storage_sync_latency_nanoseconds_by_device_count{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 2"
        ));
        assert!(rendered.contains(
            "torrentng_storage_hash_latency_nanoseconds_by_device_sum{device=\"pool\\\"a\\\\disk\\n1\",profile=\"hdd\"} 39"
        ));
        assert!(
            rendered.contains("torrentng_storage_read_latency_nanoseconds_bucket{le=\"+Inf\"} 6")
        );
        assert!(rendered.contains("torrentng_storage_read_latency_nanoseconds_sum 18"));
        assert!(rendered.contains("torrentng_storage_read_latency_nanoseconds_count 6"));
        assert!(
            rendered.contains("torrentng_storage_write_latency_nanoseconds_bucket{le=\"+Inf\"} 11")
        );
        assert!(
            rendered.contains("torrentng_storage_sync_latency_nanoseconds_bucket{le=\"+Inf\"} 2")
        );
        assert!(
            rendered.contains("torrentng_storage_hash_latency_nanoseconds_bucket{le=\"+Inf\"} 7")
        );
        assert!(rendered.contains("torrentng_memory_cap_bytes "));
        assert!(rendered.contains("torrentng_memory_class_cap_bytes{class=\"api_snapshot\"} "));
        assert!(rendered
            .contains("torrentng_memory_class_denied_allocations_total{class=\"api_snapshot\"} 1"));
        assert!(rendered.contains("torrentng_storage_backend_selected{backend=\""));
        assert!(rendered.contains("torrentng_storage_backend_fixed_buffers_supported "));
        assert!(rendered.contains("torrentng_storage_backend_registered_files_supported "));
        assert!(rendered.contains("torrentng_storage_backend_max_batch_len "));
        assert!(rendered.contains("torrentng_storage_backend_fixed_buffer_bytes "));
        assert!(rendered.contains("torrentng_storage_backend_fixed_buffer_strategy{strategy=\""));
        assert!(rendered.contains("torrentng_storage_backend_fixed_buffer_worker_copy "));
        assert!(rendered.contains("torrentng_storage_backend_frame_pool_slots_supported "));
        assert!(rendered.contains("torrentng_storage_handles_open "));
        assert!(rendered.contains("torrentng_storage_frame_bytes_cap "));
    }

    #[tokio::test]
    async fn list_torrents_empty() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents")
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
    async fn add_torrent_without_engine_returns_unavailable() {
        let state = AppState::new();
        let app = build_router(state);
        let body = serde_json::json!({
            "save_path": "/data",
            "torrent_b64": base64::engine::general_purpose::STANDARD.encode(b"not a torrent"),
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/torrents")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn list_torrents_with_entry() {
        let (app, _hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/torrents/{hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_torrent_not_found() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/torrents/{hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn mutating_endpoint_requires_configured_token() {
        let (app, hash) = setup_authed_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/torrents/{hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mutating_endpoint_accepts_bearer_token() {
        let (app, hash) = setup_authed_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/torrents/{hash}"))
                    .header(header::AUTHORIZATION, "Bearer secret-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_torrent_not_found() {
        let state = AppState::new();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/torrents/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn pause_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/torrents/{hash}/pause"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn resume_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/torrents/{hash}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn recheck_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/torrents/{hash}/recheck"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn reannounce_torrent_found() {
        let (app, hash) = setup_app_with_torrent().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/torrents/{hash}/reannounce"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn torrent_delta_reports_initial_changes_and_removals() {
        let state = AppState::new();
        let hash = "b".repeat(40);
        {
            let mut reg = state.registry.write().await;
            reg.add(TorrentEntry::new(
                hash.clone(),
                "delta.torrent".into(),
                "/data".into(),
            ))
            .unwrap();
        }

        let first = torrent_delta(&state, &BTreeMap::new()).await;
        assert_eq!(first.torrents.len(), 1);
        assert_eq!(first.torrents[0].info_hash, hash);
        assert!(first.removed.is_empty());

        {
            let mut reg = state.registry.write().await;
            reg.remove(&hash).unwrap();
        }
        let second = torrent_delta(&state, &first.current).await;
        assert!(second.torrents.is_empty());
        assert_eq!(second.removed, vec![hash]);
    }

    #[test]
    fn api_snapshot_estimates_scale_with_torrent_count() {
        assert_eq!(estimate_torrent_summary_snapshot_bytes(0), 0);
        assert_eq!(estimate_torrent_summary_snapshot_bytes(10), 10 * 1024);
        assert_eq!(estimate_torrent_delta_snapshot_bytes(10), 10 * 1536);
        let summary = TorrentSummary {
            info_hash: "a".repeat(40),
            name: "detail.bin".to_owned(),
            state: "downloading".to_owned(),
            total_length: 1024,
            downloaded: 0,
            uploaded: 0,
            ratio: 0.0,
            save_path: "/data".to_owned(),
            category: Some("linux".to_owned()),
            tags: vec!["iso".to_owned()],
            added_at: 1,
            completed_at: None,
            num_peers: 0,
            num_seeds: 0,
        };
        let small_meta = rt_engine::EngineTorrentMetadata {
            piece_length: 16 * 1024,
            piece_count: 1,
            piece_hashes: vec!["b".repeat(40)],
            piece_states: Vec::new(),
            is_private: false,
            trackers: vec!["http://tracker/announce".to_owned()],
            webseeds: Vec::new(),
            files: vec![rt_engine::EngineTorrentFile {
                index: 0,
                path: "detail.bin".to_owned(),
                length: 1024,
                priority: 1,
                wanted: true,
            }],
        };
        let mut large_meta = small_meta.clone();
        large_meta
            .files
            .extend((0..200).map(|idx| rt_engine::EngineTorrentFile {
                index: idx + 1,
                path: format!("dir/{idx}/large-detail-file-{idx}.bin"),
                length: 1024,
                priority: 1,
                wanted: true,
            }));

        assert_eq!(
            estimate_torrent_detail_snapshot_bytes(&summary, &small_meta),
            estimate_torrent_detail_base_snapshot_bytes()
        );
        assert!(
            estimate_torrent_detail_snapshot_bytes(&summary, &large_meta)
                > estimate_torrent_detail_snapshot_bytes(&summary, &small_meta)
        );
    }

    #[test]
    fn metric_with_label_escapes_label_values_once() {
        let mut rendered = String::new();
        metric_with_label(
            &mut rendered,
            "torrentng_test_metric",
            "gauge",
            "test metric",
            "backend",
            "pool\"a\\disk\n1",
            1,
        );

        assert!(rendered.contains("torrentng_test_metric{backend=\"pool\\\"a\\\\disk\\n1\"} 1"));
        assert!(!rendered.contains("pool\\\"a\\\\disk\\n1pool"));
    }
}
