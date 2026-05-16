use axum::{
    routing::{get, post},
    Router,
};

use crate::{
    handlers::{
        app_build_info, app_default_save_path, app_preferences, app_version, app_webapi_version,
        auth_login, sync_maindata, torrents_add, torrents_add_tags, torrents_categories,
        torrents_delete, torrents_files, torrents_info, torrents_pause, torrents_properties,
        torrents_reannounce, torrents_recheck, torrents_remove_tags, torrents_resume,
        torrents_set_category, torrents_tags, torrents_trackers, transfer_info,
    },
    state::AppState,
};

pub fn build_qbit_router(state: AppState) -> Router {
    Router::new()
        .route("/api/qb/v2/auth/login", post(auth_login))
        .route("/api/qb/v2/app/version", get(app_version))
        .route("/api/qb/v2/app/webapiVersion", get(app_webapi_version))
        .route("/api/qb/v2/app/buildInfo", get(app_build_info))
        .route("/api/qb/v2/app/preferences", get(app_preferences))
        .route("/api/qb/v2/app/defaultSavePath", get(app_default_save_path))
        .route("/api/qb/v2/torrents/info", get(torrents_info))
        .route("/api/qb/v2/torrents/add", post(torrents_add))
        .route("/api/qb/v2/torrents/pause", post(torrents_pause))
        .route("/api/qb/v2/torrents/resume", post(torrents_resume))
        .route("/api/qb/v2/torrents/delete", post(torrents_delete))
        .route("/api/qb/v2/torrents/reannounce", post(torrents_reannounce))
        .route("/api/qb/v2/torrents/recheck", post(torrents_recheck))
        .route("/api/qb/v2/torrents/trackers", get(torrents_trackers))
        .route("/api/qb/v2/torrents/files", get(torrents_files))
        .route("/api/qb/v2/torrents/properties", get(torrents_properties))
        .route("/api/qb/v2/torrents/categories", get(torrents_categories))
        .route("/api/qb/v2/torrents/tags", get(torrents_tags))
        .route(
            "/api/qb/v2/torrents/setCategory",
            post(torrents_set_category),
        )
        .route("/api/qb/v2/torrents/addTags", post(torrents_add_tags))
        .route("/api/qb/v2/torrents/removeTags", post(torrents_remove_tags))
        .route("/api/qb/v2/sync/maindata", get(sync_maindata))
        .route("/api/qb/v2/transfer/info", get(transfer_info))
        .with_state(state)
}
