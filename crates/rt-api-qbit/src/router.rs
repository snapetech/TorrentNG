use axum::{
    routing::{get, post},
    Router,
};

use crate::{handlers::*, state::AppState};

pub fn build_qbit_router(state: AppState) -> Router {
    Router::new()
        .nest("/api/qb/v2", qbit_routes())
        .nest("/api/v2", qbit_routes())
        .with_state(state)
}

fn qbit_routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(auth_login))
        .route("/auth/logout", post(auth_logout))
        .route("/app/version", get(app_version))
        .route("/app/webapiVersion", get(app_webapi_version))
        .route("/app/buildInfo", get(app_build_info))
        .route("/app/preferences", get(app_preferences))
        .route("/app/setPreferences", post(app_set_preferences))
        .route("/app/shutdown", post(app_shutdown))
        .route(
            "/app/sendTestEmail",
            get(app_send_test_email).post(app_send_test_email),
        )
        .route("/app/getCookies", get(app_get_cookies))
        .route("/app/setCookies", post(app_set_cookies))
        .route("/app/rotateAPIKey", post(app_rotate_api_key))
        .route("/app/deleteAPIKey", post(app_delete_api_key))
        .route("/app/networkInterfaceList", get(app_network_interface_list))
        .route(
            "/app/networkInterfaceAddressList",
            get(app_network_interface_address_list),
        )
        .route("/app/defaultSavePath", get(app_default_save_path))
        .route("/torrents/info", get(torrents_info))
        .route("/torrents/add", post(torrents_add))
        .route("/torrents/pause", post(torrents_pause))
        .route("/torrents/resume", post(torrents_resume))
        .route("/torrents/start", post(torrents_start))
        .route("/torrents/stop", post(torrents_stop))
        .route("/torrents/delete", post(torrents_delete))
        .route("/torrents/reannounce", post(torrents_reannounce))
        .route("/torrents/recheck", post(torrents_recheck))
        .route("/torrents/trackers", get(torrents_trackers))
        .route("/torrents/addTrackers", post(torrents_add_trackers))
        .route("/torrents/editTracker", post(torrents_edit_tracker))
        .route("/torrents/removeTrackers", post(torrents_remove_trackers))
        .route("/torrents/addPeers", post(torrents_add_peers))
        .route("/torrents/files", get(torrents_files))
        .route("/torrents/webseeds", get(torrents_webseeds))
        .route("/torrents/pieceStates", get(torrents_piece_states))
        .route("/torrents/pieceHashes", get(torrents_piece_hashes))
        .route("/torrents/export", get(torrents_export))
        .route("/torrents/filePrio", post(torrents_file_prio))
        .route("/torrents/increasePrio", post(torrents_increase_prio))
        .route("/torrents/decreasePrio", post(torrents_decrease_prio))
        .route("/torrents/topPrio", post(torrents_top_prio))
        .route("/torrents/bottomPrio", post(torrents_bottom_prio))
        .route("/torrents/properties", get(torrents_properties))
        .route("/torrents/categories", get(torrents_categories))
        .route("/torrents/tags", get(torrents_tags))
        .route("/torrents/rename", post(torrents_rename))
        .route("/torrents/renameFile", post(torrents_rename_file))
        .route("/torrents/renameFolder", post(torrents_rename_folder))
        .route("/torrents/setLocation", post(torrents_set_location))
        .route("/torrents/setSavePath", post(torrents_set_save_path))
        .route("/torrents/setCategory", post(torrents_set_category))
        .route("/torrents/createCategory", post(torrents_create_category))
        .route("/torrents/editCategory", post(torrents_edit_category))
        .route(
            "/torrents/removeCategories",
            post(torrents_remove_categories),
        )
        .route("/torrents/addTags", post(torrents_add_tags))
        .route("/torrents/setTags", post(torrents_set_tags))
        .route("/torrents/removeTags", post(torrents_remove_tags))
        .route("/torrents/createTags", post(torrents_create_tags))
        .route("/torrents/deleteTags", post(torrents_delete_tags))
        .route("/torrents/downloadLimit", get(torrents_download_limit))
        .route(
            "/torrents/setDownloadLimit",
            post(torrents_set_download_limit),
        )
        .route("/torrents/uploadLimit", get(torrents_upload_limit))
        .route("/torrents/setUploadLimit", post(torrents_set_upload_limit))
        .route("/torrents/setShareLimits", post(torrents_set_share_limits))
        .route("/torrents/setForceStart", post(torrents_set_force_start))
        .route(
            "/torrents/setSuperSeeding",
            post(torrents_set_super_seeding),
        )
        .route("/torrents/setAutoTMM", post(torrents_set_auto_tmm))
        .route(
            "/torrents/setAutoManagement",
            post(torrents_set_auto_management),
        )
        .route(
            "/torrents/toggleSequentialDownload",
            post(torrents_toggle_sequential_download),
        )
        .route(
            "/torrents/toggleFirstLastPiecePrio",
            post(torrents_toggle_first_last_piece_prio),
        )
        .route("/sync/maindata", get(sync_maindata))
        .route("/sync/torrentPeers", get(sync_torrent_peers))
        .route("/transfer/info", get(transfer_info))
        .route("/transfer/downloadLimit", get(transfer_download_limit))
        .route("/transfer/uploadLimit", get(transfer_upload_limit))
        .route("/transfer/speedLimitsMode", get(transfer_speed_limits_mode))
        .route(
            "/transfer/toggleSpeedLimitsMode",
            post(transfer_toggle_speed_limits_mode),
        )
        .route(
            "/transfer/setDownloadLimit",
            post(transfer_set_download_limit),
        )
        .route("/transfer/setUploadLimit", post(transfer_set_upload_limit))
        .route("/transfer/banPeers", post(transfer_ban_peers))
        .route("/log/main", get(log_main))
        .route("/log/peers", get(log_peers))
        .route("/search/status", get(search_status))
        .route("/search/categories", get(search_categories))
        .route("/search/plugins", get(search_plugins))
        .route("/search/installPlugin", post(search_install_plugin))
        .route("/search/uninstallPlugin", post(search_uninstall_plugin))
        .route("/search/enablePlugin", post(search_enable_plugin))
        .route("/search/updatePlugins", post(search_update_plugins))
        .route("/search/start", post(search_start))
        .route("/search/stop", post(search_stop))
        .route("/search/results", get(search_results))
        .route("/search/delete", post(search_delete))
        .route("/rss/items", get(rss_items))
        .route("/rss/rules", get(rss_rules))
        .route("/rss/matchingArticles", get(rss_matching_articles))
        .route("/rss/addFolder", post(rss_add_folder))
        .route("/rss/addFeed", post(rss_add_feed))
        .route("/rss/removeItem", post(rss_remove_item))
        .route("/rss/moveItem", post(rss_move_item))
        .route("/rss/markAsRead", post(rss_mark_as_read))
        .route("/rss/refreshItem", post(rss_refresh_item))
        .route("/rss/setRule", post(rss_set_rule))
        .route("/rss/renameRule", post(rss_rename_rule))
        .route("/rss/removeRule", post(rss_remove_rule))
}
