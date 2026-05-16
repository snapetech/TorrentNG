use axum::{
    routing::{get, post},
    Router,
};

use crate::{handlers::*, state::AppState};

pub fn build_qbit_router(state: AppState) -> Router {
    Router::new()
        .route("/api/qb/v2/auth/login", post(auth_login))
        .route("/api/qb/v2/auth/logout", post(auth_logout))
        .route("/api/qb/v2/app/version", get(app_version))
        .route("/api/qb/v2/app/webapiVersion", get(app_webapi_version))
        .route("/api/qb/v2/app/buildInfo", get(app_build_info))
        .route("/api/qb/v2/app/preferences", get(app_preferences))
        .route("/api/qb/v2/app/setPreferences", post(app_set_preferences))
        .route("/api/qb/v2/app/defaultSavePath", get(app_default_save_path))
        .route("/api/qb/v2/torrents/info", get(torrents_info))
        .route("/api/qb/v2/torrents/add", post(torrents_add))
        .route("/api/qb/v2/torrents/pause", post(torrents_pause))
        .route("/api/qb/v2/torrents/resume", post(torrents_resume))
        .route("/api/qb/v2/torrents/start", post(torrents_start))
        .route("/api/qb/v2/torrents/stop", post(torrents_stop))
        .route("/api/qb/v2/torrents/delete", post(torrents_delete))
        .route("/api/qb/v2/torrents/reannounce", post(torrents_reannounce))
        .route("/api/qb/v2/torrents/recheck", post(torrents_recheck))
        .route("/api/qb/v2/torrents/trackers", get(torrents_trackers))
        .route(
            "/api/qb/v2/torrents/addTrackers",
            post(torrents_add_trackers),
        )
        .route(
            "/api/qb/v2/torrents/editTracker",
            post(torrents_edit_tracker),
        )
        .route(
            "/api/qb/v2/torrents/removeTrackers",
            post(torrents_remove_trackers),
        )
        .route("/api/qb/v2/torrents/files", get(torrents_files))
        .route("/api/qb/v2/torrents/filePrio", post(torrents_file_prio))
        .route(
            "/api/qb/v2/torrents/increasePrio",
            post(torrents_increase_prio),
        )
        .route(
            "/api/qb/v2/torrents/decreasePrio",
            post(torrents_decrease_prio),
        )
        .route("/api/qb/v2/torrents/topPrio", post(torrents_top_prio))
        .route("/api/qb/v2/torrents/bottomPrio", post(torrents_bottom_prio))
        .route("/api/qb/v2/torrents/properties", get(torrents_properties))
        .route("/api/qb/v2/torrents/categories", get(torrents_categories))
        .route("/api/qb/v2/torrents/tags", get(torrents_tags))
        .route("/api/qb/v2/torrents/rename", post(torrents_rename))
        .route(
            "/api/qb/v2/torrents/setLocation",
            post(torrents_set_location),
        )
        .route(
            "/api/qb/v2/torrents/setCategory",
            post(torrents_set_category),
        )
        .route(
            "/api/qb/v2/torrents/createCategory",
            post(torrents_create_category),
        )
        .route(
            "/api/qb/v2/torrents/editCategory",
            post(torrents_edit_category),
        )
        .route(
            "/api/qb/v2/torrents/removeCategories",
            post(torrents_remove_categories),
        )
        .route("/api/qb/v2/torrents/addTags", post(torrents_add_tags))
        .route("/api/qb/v2/torrents/setTags", post(torrents_set_tags))
        .route("/api/qb/v2/torrents/removeTags", post(torrents_remove_tags))
        .route("/api/qb/v2/torrents/createTags", post(torrents_create_tags))
        .route("/api/qb/v2/torrents/deleteTags", post(torrents_delete_tags))
        .route(
            "/api/qb/v2/torrents/setDownloadLimit",
            post(torrents_set_download_limit),
        )
        .route(
            "/api/qb/v2/torrents/setUploadLimit",
            post(torrents_set_upload_limit),
        )
        .route(
            "/api/qb/v2/torrents/setShareLimits",
            post(torrents_set_share_limits),
        )
        .route(
            "/api/qb/v2/torrents/setForceStart",
            post(torrents_set_force_start),
        )
        .route(
            "/api/qb/v2/torrents/setSuperSeeding",
            post(torrents_set_super_seeding),
        )
        .route(
            "/api/qb/v2/torrents/setAutoTMM",
            post(torrents_set_auto_tmm),
        )
        .route(
            "/api/qb/v2/torrents/toggleSequentialDownload",
            post(torrents_toggle_sequential_download),
        )
        .route(
            "/api/qb/v2/torrents/toggleFirstLastPiecePrio",
            post(torrents_toggle_first_last_piece_prio),
        )
        .route("/api/qb/v2/sync/maindata", get(sync_maindata))
        .route("/api/qb/v2/sync/torrentPeers", get(sync_torrent_peers))
        .route("/api/qb/v2/transfer/info", get(transfer_info))
        .route(
            "/api/qb/v2/transfer/downloadLimit",
            get(transfer_download_limit),
        )
        .route(
            "/api/qb/v2/transfer/uploadLimit",
            get(transfer_upload_limit),
        )
        .route(
            "/api/qb/v2/transfer/setDownloadLimit",
            post(transfer_set_download_limit),
        )
        .route(
            "/api/qb/v2/transfer/setUploadLimit",
            post(transfer_set_upload_limit),
        )
        .route("/api/qb/v2/transfer/banPeers", post(transfer_ban_peers))
        .route("/api/qb/v2/log/main", get(log_main))
        .route("/api/qb/v2/log/peers", get(log_peers))
        .route("/api/qb/v2/search/status", get(search_status))
        .route("/api/qb/v2/search/plugins", get(search_plugins))
        .route("/api/qb/v2/search/start", post(search_start))
        .route("/api/qb/v2/search/stop", post(search_stop))
        .route("/api/qb/v2/search/results", get(search_results))
        .route("/api/qb/v2/rss/items", get(rss_items))
        .route("/api/qb/v2/rss/rules", get(rss_rules))
        .route(
            "/api/qb/v2/rss/matchingArticles",
            get(rss_matching_articles),
        )
        .route("/api/qb/v2/rss/addFolder", post(rss_noop))
        .route("/api/qb/v2/rss/addFeed", post(rss_noop))
        .route("/api/qb/v2/rss/removeItem", post(rss_noop))
        .route("/api/qb/v2/rss/moveItem", post(rss_noop))
        .route("/api/qb/v2/rss/markAsRead", post(rss_noop))
        .route("/api/qb/v2/rss/refreshItem", post(rss_noop))
        .route("/api/qb/v2/rss/setRule", post(rss_noop))
        .route("/api/qb/v2/rss/renameRule", post(rss_noop))
        .route("/api/qb/v2/rss/removeRule", post(rss_noop))
        .with_state(state)
}
