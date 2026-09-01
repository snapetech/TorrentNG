use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Router,
};

use crate::{
    handlers::{
        add_torrent, add_torrent_peers, add_torrent_tags, apply_rss_rules, auth_login, auth_logout,
        bulk_action, cancel_job, categories, create_tag, cross_seed, delete_category,
        delete_saved_json, delete_tag, delete_torrent, diagnose_torrent, engine_commands,
        engine_diagnostics, get_torrent, get_user_agent, health, list_json_map,
        list_session_events, list_torrent_files, list_torrent_trackers, list_torrents,
        list_workflow_runs, logs, metrics, patch_torrent_files, patch_torrent_trackers, pause_job,
        pause_torrent, reannounce_torrent, recheck_torrent, remove_torrent_tags, restart_engine,
        resume_job, resume_torrent, rtorrent_settings, run_json_workflow, save_rtorrent_settings,
        session_features, set_torrent_category, set_user_agent, sidebar_facets, storage,
        storage_execute_plan, storage_preview_plan, stream_events, tags, test_rss_rules,
        torrent_limits, tracker_health, transfer_info, transfer_limits, update_session_features,
        update_torrent, update_torrent_limits, update_torrent_queue, update_transfer_limits,
        upsert_category, upsert_json_map,
    },
    state::AppState,
};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/torrents", get(list_torrents).post(add_torrent))
        .route(
            "/api/v1/torrents/:hash",
            get(get_torrent).put(update_torrent).delete(delete_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/stop",
            axum::routing::post(pause_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/pause",
            axum::routing::post(pause_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/start",
            axum::routing::post(resume_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/resume",
            axum::routing::post(resume_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/recheck",
            axum::routing::post(recheck_torrent),
        )
        .route(
            "/api/v1/torrents/:hash/reannounce",
            axum::routing::post(reannounce_torrent),
        )
        .route("/api/v1/torrents/:hash/category", put(set_torrent_category))
        .route(
            "/api/v1/torrents/:hash/limits",
            get(torrent_limits).put(update_torrent_limits),
        )
        .route("/api/v1/torrents/:hash/peers", post(add_torrent_peers))
        .route("/api/v1/torrents/queue", post(update_torrent_queue))
        .route(
            "/api/v1/transfer/limits",
            get(transfer_limits).put(update_transfer_limits),
        )
        .route("/api/v1/transfer/info", get(transfer_info))
        .route(
            "/api/v1/session/features",
            get(session_features).put(update_session_features),
        )
        .route(
            "/api/v1/torrents/:hash/tags",
            post(add_torrent_tags)
                .patch(crate::handlers::patch_torrent_tags)
                .delete(remove_torrent_tags),
        )
        .route("/api/v1/categories", get(categories).post(upsert_category))
        .route("/api/v1/categories/:name", delete(delete_category))
        .route("/api/v1/tags", get(tags).post(create_tag))
        .route("/api/v1/tags/:name", delete(delete_tag))
        .route("/api/v1/bulk/:action", post(bulk_action))
        .route("/api/v1/cross-seed", post(cross_seed))
        .route("/api/v1/tracker-health", get(tracker_health))
        .route("/api/v1/sidebar-facets", get(sidebar_facets))
        .route("/api/v1/logs", get(logs))
        .route(
            "/api/v1/saved-views",
            get(list_json_map::<crate::handlers::SavedViewsStore>)
                .post(upsert_json_map::<crate::handlers::SavedViewsStore>),
        )
        .route(
            "/api/v1/saved-views/:id",
            delete(delete_saved_json::<crate::handlers::SavedViewsStore>),
        )
        .route(
            "/api/v1/ratio-groups",
            get(list_json_map::<crate::handlers::RatioGroupsStore>)
                .post(upsert_json_map::<crate::handlers::RatioGroupsStore>),
        )
        .route(
            "/api/v1/ratio-groups/:id",
            post(run_json_workflow::<crate::handlers::RatioGroupsStore>)
                .delete(delete_saved_json::<crate::handlers::RatioGroupsStore>),
        )
        .route(
            "/api/v1/workflows",
            get(list_json_map::<crate::handlers::WorkflowsStore>)
                .post(upsert_json_map::<crate::handlers::WorkflowsStore>),
        )
        .route(
            "/api/v1/workflows/:id",
            post(run_json_workflow::<crate::handlers::WorkflowsStore>)
                .delete(delete_saved_json::<crate::handlers::WorkflowsStore>),
        )
        .route("/api/v1/workflow-runs", get(list_workflow_runs))
        .route(
            "/api/v1/rss-rules",
            get(list_json_map::<crate::handlers::RssRulesStore>)
                .post(upsert_json_map::<crate::handlers::RssRulesStore>),
        )
        .route("/api/v1/rss-rules/test", post(test_rss_rules))
        .route("/api/v1/rss-rules/apply", post(apply_rss_rules))
        .route(
            "/api/v1/rss-rules/:id",
            delete(delete_saved_json::<crate::handlers::RssRulesStore>),
        )
        .route("/api/v1/engine", get(engine_diagnostics))
        .route("/api/v1/engine/commands", get(engine_commands))
        .route(
            "/api/v1/engine/rtorrent-settings",
            get(rtorrent_settings).put(save_rtorrent_settings),
        )
        .route("/api/v1/engine/restart", post(restart_engine))
        .route(
            "/api/v1/engine/user-agent",
            get(get_user_agent).put(set_user_agent),
        )
        .route(
            "/api/v1/settings/user-agent",
            get(get_user_agent).put(set_user_agent),
        )
        .route(
            "/api/v1/torrents/:hash/files",
            get(list_torrent_files).patch(patch_torrent_files),
        )
        .route(
            "/api/v1/torrents/:hash/trackers",
            get(list_torrent_trackers).patch(patch_torrent_trackers),
        )
        .route("/api/v1/torrents/:hash/diagnostics", get(diagnose_torrent))
        .route("/api/v1/events", get(stream_events))
        .route("/api/v1/session-events", get(list_session_events))
        .route("/api/v1/jobs", get(crate::handlers::list_jobs))
        .route("/api/v1/jobs/:id/pause", post(pause_job))
        .route("/api/v1/jobs/:id/resume", post(resume_job))
        .route("/api/v1/jobs/:id/cancel", post(cancel_job))
        .route("/api/v1/storage", get(storage))
        .route("/api/v1/storage/plan", post(storage_preview_plan))
        .route("/api/v1/storage/execute", post(storage_execute_plan))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            native_auth_guard,
        ))
        .with_state(state)
}

async fn native_auth_guard(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if native_public_path(path)
        || state.api_tokens.is_empty()
        || native_presented_token(req.headers())
            .is_some_and(|token| native_token_allowed(&state, &token))
    {
        return next.run(req).await;
    }

    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"code":"UNAUTHORIZED","message":"missing or invalid API token"}"#,
    )
        .into_response()
}

fn native_public_path(path: &str) -> bool {
    matches!(
        path,
        "/health" | "/api/v1/auth/login" | "/api/v1/auth/logout"
    )
}

fn native_token_allowed(state: &AppState, token: &str) -> bool {
    state.api_tokens.iter().any(|allowed| allowed == token)
}

fn native_presented_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(native_session_cookie)
        })
}

fn native_session_cookie(cookie: &str) -> Option<String> {
    cookie.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("tng_session=")
            .and_then(cookie_component_decode)
    })
}

fn cookie_component_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return None;
        }
        let high = hex_value(bytes[index + 1])?;
        let low = hex_value(bytes[index + 2])?;
        output.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
