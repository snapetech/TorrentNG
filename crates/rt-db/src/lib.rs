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
    list_torrent_hashes_by_tracker, list_torrent_trackers, replace_torrent_files,
    replace_torrent_files_in_tx, replace_torrent_trackers, replace_torrent_trackers_in_tx,
    torrent_tracker_health, torrent_tracker_status_counts, upsert_torrent_limits,
    upsert_torrent_limits_in_tx, TorrentFileRow, TorrentLimitRow, TorrentTrackerHealthRow,
    TorrentTrackerRow, TorrentTrackerStatusCounts,
};
pub use error::DbError;
pub use event_row::{
    append_job_event, append_job_event_in_tx, append_session_event, append_session_event_in_tx,
    first_job_event, list_job_events, list_session_events, list_session_events_filtered,
    prune_session_events, prune_session_events_in_tx, JobEventRow, SessionEventRow,
};
pub use job_row::{
    count_active_jobs, get_job, list_active_jobs, upsert_job, upsert_job_in_tx, JobRow,
};
pub use peer_ban_row::{insert_peer_bans_in_tx, list_peer_bans};
pub use projection_row::{
    list_active_issues, record_active_issue, record_active_issue_in_tx, resolve_active_issue,
    resolve_active_issue_in_tx, ProjectionIssueRow,
};
pub use schema::migrate;
pub use settings_row::{get_setting, set_setting, set_setting_in_tx};
pub use storage_row::{
    get_mount, get_storage_root, list_mounts, list_storage_roots, upsert_mount,
    upsert_storage_root, MountRow, StorageRootRow,
};
pub use torrent_row::{
    create_category_in_tx, delete, delete_in_tx, get, list_all, list_by_state, list_categories,
    list_category_definitions, list_torrent_tags, remove_categories_in_tx, rename_category_in_tx,
    upsert, upsert_in_tx, TorrentRow,
};
