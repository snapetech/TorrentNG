/// qBittorrent v2 API wire types (snake_case JSON with some camelCase quirks).
///
/// Field names follow qBit's actual API output so *arr clients can parse them.
use serde::{Deserialize, Serialize};

/// `GET /api/qb/v2/torrents/info` response element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QbTorrentInfo {
    pub hash: String,
    pub name: String,
    pub state: String,
    pub size: i64,
    pub total_size: i64,
    pub downloaded: i64,
    pub downloaded_session: i64,
    pub uploaded: i64,
    pub uploaded_session: i64,
    pub ratio: f64,
    pub save_path: String,
    pub content_path: String,
    pub root_path: String,
    pub category: String,
    pub tags: String, // comma-separated
    pub added_on: i64,
    pub completion_on: i64,
    pub last_activity: i64,
    pub seen_complete: i64,
    pub time_active: i64,
    pub seeding_time: i64,
    pub num_leechs: u32,
    pub num_seeds: u32,
    pub dlspeed: i64,
    pub upspeed: i64,
    pub dl_limit: i64,
    pub up_limit: i64,
    pub eta: i64,
    pub progress: f64,
    pub priority: i32,
    pub amount_left: i64,
    pub auto_tmm: bool,
    pub seq_dl: bool,
    pub f_l_piece_prio: bool,
    pub force_start: bool,
    pub super_seeding: bool,
    pub ratio_limit: f64,
    pub seeding_time_limit: i64,
    pub tracker: String,
    pub trackers_count: u32,
    pub magnet_uri: String,
    pub infohash_v1: String,
    pub infohash_v2: String,
}

/// `GET /api/qb/v2/torrents/files` response element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QbFileInfo {
    pub index: u32,
    pub name: String,
    pub size: i64,
    pub priority: u8,
    pub progress: f64,
}

/// `GET /api/qb/v2/torrents/trackers` response element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QbTrackerInfo {
    pub url: String,
    pub status: i32,
    pub tier: i32,
    pub num_peers: i32,
    pub num_seeds: i32,
    pub num_leeches: i32,
    pub num_downloaded: i32,
    pub msg: String,
}

/// `GET /api/qb/v2/torrents/properties` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QbTorrentProperties {
    pub save_path: String,
    pub creation_date: i64,
    pub piece_size: i64,
    pub comment: String,
    pub total_wasted: i64,
    pub total_uploaded: i64,
    pub total_uploaded_session: i64,
    pub total_downloaded: i64,
    pub total_downloaded_session: i64,
    pub up_limit: i64,
    pub dl_limit: i64,
    pub time_elapsed: i64,
    pub seeding_time: i64,
    pub nb_connections: i64,
    pub nb_connections_limit: i64,
    pub share_ratio: f64,
    pub addition_date: i64,
    pub completion_date: i64,
    pub created_by: String,
    pub dl_speed_avg: i64,
    pub dl_speed: i64,
    pub eta: i64,
    pub last_seen: i64,
    pub peers: i64,
    pub peers_total: i64,
    pub pieces_have: i64,
    pub pieces_num: i64,
    pub reannounce: i64,
    pub seeds: i64,
    pub seeds_total: i64,
    pub total_size: i64,
    pub up_speed_avg: i64,
    pub up_speed: i64,
}

/// `GET /api/qb/v2/torrents/categories` map value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QbCategoryInfo {
    pub name: String,
    #[serde(rename = "savePath")]
    pub save_path: String,
}

/// `GET /api/qb/v2/sync/maindata` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QbMaindata {
    pub rid: i64,
    pub full_update: bool,
    pub torrents: serde_json::Value,
    pub torrents_removed: Vec<String>,
    pub server_state: QbServerState,
}

/// `GET /api/qb/v2/transfer/info` / `server_state` within maindata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QbServerState {
    pub dl_info_speed: i64,
    pub dl_info_data: i64,
    pub up_info_speed: i64,
    pub up_info_data: i64,
    pub connection_status: String,
    pub free_space_on_disk: i64,
    pub dl_rate_limit: i64,
    pub up_rate_limit: i64,
    pub use_alt_speed_limits: bool,
}

/// `POST /api/qb/v2/torrents/add` form fields.
#[derive(Debug, Clone, Deserialize)]
pub struct QbAddTorrentForm {
    pub torrents: Option<String>, // base64 .torrent
    pub urls: Option<String>,     // newline-sep magnet/http URLs
    #[serde(rename = "savepath")]
    pub save_path: Option<String>,
    pub category: Option<String>,
    pub tags: Option<String>,
    pub paused: Option<String>, // "true"/"false"
}

/// State mapping from internal TorrentState → qBit state string.
pub fn to_qbit_state(state: &str) -> &'static str {
    match state {
        "seeding" => "uploading",
        "metadata_pending" => "metaDL",
        "downloading" => "downloading",
        "checking" => "checkingUP",
        "paused" => "pausedUP",
        "stopped" => "pausedUP",
        "queued" => "queuedUP",
        "error" => "error",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torrent_info_serializes() {
        let t = QbTorrentInfo {
            hash: "a".repeat(40),
            name: "test".into(),
            state: "uploading".into(),
            size: 1_000_000,
            total_size: 1_000_000,
            downloaded: 1_000_000,
            downloaded_session: 1_000_000,
            uploaded: 5_000_000,
            uploaded_session: 5_000_000,
            ratio: 5.0,
            save_path: "/data/".into(),
            content_path: "/data/test".into(),
            root_path: "/data/test".into(),
            category: "movies".into(),
            tags: "hd,bluray".into(),
            added_on: 1_700_000_000,
            completion_on: 1_700_001_000,
            last_activity: 1_700_000_000,
            seen_complete: 1_700_001_000,
            time_active: 0,
            seeding_time: 0,
            num_leechs: 2,
            num_seeds: 10,
            dlspeed: 0,
            upspeed: 1_000_000,
            dl_limit: 0,
            up_limit: 0,
            eta: -1,
            progress: 1.0,
            priority: 0,
            amount_left: 0,
            auto_tmm: false,
            seq_dl: false,
            f_l_piece_prio: false,
            force_start: false,
            super_seeding: false,
            ratio_limit: -1.0,
            seeding_time_limit: -1,
            tracker: "http://tracker.example.com/announce".into(),
            trackers_count: 1,
            magnet_uri: "magnet:?xt=urn:btih:test".into(),
            infohash_v1: "a".repeat(40),
            infohash_v2: String::new(),
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"uploading\""));
        assert!(json.contains("num_leechs"));
    }

    #[test]
    fn state_mapping_covers_all_internal_states() {
        for state in &[
            "seeding",
            "downloading",
            "checking",
            "paused",
            "stopped",
            "queued",
            "error",
        ] {
            let qb = to_qbit_state(state);
            assert!(!qb.is_empty());
            assert_ne!(qb, "unknown");
        }
    }

    #[test]
    fn unknown_state_maps_to_unknown() {
        assert_eq!(to_qbit_state("garbage"), "unknown");
    }

    #[test]
    fn torrent_properties_serializes_qbit_fields() {
        let props = QbTorrentProperties {
            save_path: "/data/".into(),
            creation_date: 1,
            piece_size: 16_384,
            comment: String::new(),
            total_wasted: 0,
            total_uploaded: 20,
            total_uploaded_session: 20,
            total_downloaded: 10,
            total_downloaded_session: 10,
            up_limit: -1,
            dl_limit: -1,
            time_elapsed: 0,
            seeding_time: 0,
            nb_connections: 0,
            nb_connections_limit: -1,
            share_ratio: 2.0,
            addition_date: 1,
            completion_date: -1,
            created_by: String::new(),
            dl_speed_avg: 0,
            dl_speed: 0,
            eta: -1,
            last_seen: -1,
            peers: 0,
            peers_total: 0,
            pieces_have: 0,
            pieces_num: 3,
            reannounce: -1,
            seeds: 0,
            seeds_total: 0,
            total_size: 30,
            up_speed_avg: 0,
            up_speed: 0,
        };
        let json = serde_json::to_string(&props).unwrap();
        assert!(json.contains("save_path"));
        assert!(json.contains("piece_size"));
        assert!(json.contains("share_ratio"));
    }
}
