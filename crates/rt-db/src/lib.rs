pub mod detail_row;
pub mod error;
pub mod event_row;
pub mod job_row;
pub mod schema;
pub mod settings_row;
pub mod storage_row;
pub mod torrent_row;

pub use detail_row::{
    get_torrent_limits, list_all_torrent_trackers, list_torrent_files, list_torrent_trackers,
    replace_torrent_files, replace_torrent_trackers, upsert_torrent_limits, TorrentFileRow,
    TorrentLimitRow, TorrentTrackerRow,
};
pub use error::DbError;
pub use event_row::{
    append_job_event, append_session_event, list_job_events, list_session_events, JobEventRow,
    SessionEventRow,
};
pub use job_row::{get_job, list_active_jobs, upsert_job, JobRow};
pub use schema::migrate;
pub use settings_row::{get_setting, set_setting};
pub use storage_row::{
    get_mount, get_storage_root, list_mounts, list_storage_roots, upsert_mount,
    upsert_storage_root, MountRow, StorageRootRow,
};
pub use torrent_row::{delete, get, list_all, list_by_state, upsert, TorrentRow};
