pub mod detail_row;
pub mod error;
pub mod event_row;
pub mod job_row;
pub mod peer_ban_row;
pub mod projection_row;
pub mod schema;
pub mod settings_row;
pub mod storage_row;
pub mod torrent_row;

pub use detail_row::{
    count_torrent_files, get_torrent_limits, list_all_torrent_trackers, list_torrent_files,
    list_torrent_hashes_by_tracker, list_torrent_tracker_deadlines, list_torrent_tracker_hashes,
    list_torrent_tracker_urls, list_torrent_trackers, replace_torrent_files,
    replace_torrent_files_in_tx, replace_torrent_trackers, replace_torrent_trackers_in_tx,
    torrent_hashes_by_tracker_snapshot_size, torrent_tracker_deadline, torrent_tracker_health,
    torrent_tracker_health_snapshot_size, torrent_tracker_projection, torrent_tracker_rows_exist,
    torrent_tracker_snapshot_size, torrent_tracker_status_counts,
    torrent_tracker_status_counts_for_torrent, upsert_torrent_limits, upsert_torrent_limits_in_tx,
    TorrentFileRow, TorrentLimitRow, TorrentTrackerHealthRow, TorrentTrackerRow,
    TorrentTrackerStatusCounts, MAX_TORRENT_FILE_PATH_BYTES, MAX_TORRENT_FILE_RESULT_BYTES,
    MAX_TORRENT_FILE_RESULT_ITEMS, MAX_TORRENT_TRACKER_ID_BYTES,
    MAX_TORRENT_TRACKER_INFO_HASH_BYTES, MAX_TORRENT_TRACKER_TEXT_BYTES,
};
pub use error::DbError;
pub use event_row::{
    append_job_event, append_job_event_in_tx, append_session_event, append_session_event_in_tx,
    first_job_event, list_job_events, list_session_events, list_session_events_filtered,
    prune_session_events, prune_session_events_in_tx, JobEventRow, SessionEventRow,
    MAX_JOB_EVENT_PAYLOAD_BYTES, MAX_JOB_EVENT_RESULT_ITEMS, MAX_SESSION_EVENT_PAYLOAD_BYTES,
    MAX_SESSION_EVENT_RESULT_ITEMS,
};
pub use job_row::{
    count_active_jobs, get_job, list_active_jobs, list_failed_jobs_with_error_prefix, upsert_job,
    upsert_job_in_tx, JobRow, MAX_JOB_AFFECTED_TORRENTS, MAX_JOB_AFFECTED_TORRENTS_BYTES,
    MAX_JOB_INVALID_PIECES, MAX_JOB_RESULT_BYTES, MAX_JOB_RESULT_ITEMS,
};
pub use peer_ban_row::{
    insert_peer_bans_in_tx, list_peer_bans, MAX_PEER_BAN_ITEMS, MAX_PEER_BAN_RESULT_BYTES,
};
pub use projection_row::{
    list_active_issues, record_active_issue, record_active_issue_in_tx, resolve_active_issue,
    resolve_active_issue_in_tx, ProjectionIssueRow,
};
pub use schema::migrate;
pub use settings_row::{
    get_setting, set_setting, set_setting_in_tx, MAX_SETTING_KEY_BYTES, MAX_SETTING_VALUE_BYTES,
};
pub use storage_row::{
    get_mount, get_storage_root, list_mounts, list_storage_roots, upsert_mount,
    upsert_storage_root, MountRow, StorageRootRow, MAX_MOUNT_RESULT_BYTES, MAX_MOUNT_RESULT_ITEMS,
    MAX_STORAGE_ROOT_RESULT_BYTES, MAX_STORAGE_ROOT_RESULT_ITEMS,
};
pub use torrent_row::{
    create_category_in_tx, delete, delete_in_tx, delete_torrent_metadata_v2_hash_in_tx, get,
    list_all, list_all_torrent_labels, list_all_torrent_tags, list_by_state, list_categories,
    list_category_definitions, list_torrent_labels_page, list_torrent_metadata_v2_hashes,
    list_torrent_privacy_for_hashes, list_torrent_tags, remove_categories_in_tx,
    rename_category_in_tx, torrent_exists, torrent_is_private, torrent_metadata_projection,
    torrent_piece_count, torrent_promotion_projection, torrent_state, torrent_tracker_override,
    torrent_transfer_counters, update_fields_in_tx, update_labels_in_tx, update_runtime_in_tx,
    update_state_in_tx, update_state_only_in_tx, update_trackers_in_tx, upsert, upsert_in_tx,
    upsert_torrent_metadata_v2_hash_in_tx, TorrentLabelsRow, TorrentMetadataProjection,
    TorrentPromotionProjection, TorrentRow, MAX_CATEGORY_DEFINITION_BYTES,
    MAX_CATEGORY_DEFINITION_ITEMS, MAX_TORRENT_LABEL_RESULT_BYTES, MAX_TORRENT_LABEL_RESULT_ITEMS,
    MAX_TORRENT_LABEL_ROW_BYTES, MAX_TORRENT_RESULT_BYTES, MAX_TORRENT_RESULT_ITEMS,
    MAX_TORRENT_TRACKER_RESULT_BYTES, MAX_TORRENT_TRACKER_RESULT_ITEMS,
    MAX_TORRENT_TRACKER_ROW_BYTES, MAX_TORRENT_TRACKER_URL_BYTES, TORRENT_LABEL_PAGE_SIZE,
};
