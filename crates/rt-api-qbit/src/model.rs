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
    pub downloaded: i64,
    pub uploaded: i64,
    pub ratio: f64,
    pub save_path: String,
    pub category: String,
    pub tags: String, // comma-separated
    pub added_on: i64,
    pub completion_on: i64,
    pub num_leechs: u32,
    pub num_seeds: u32,
    pub dlspeed: i64,
    pub upspeed: i64,
    pub eta: i64,
    pub progress: f64,
    pub priority: i32,
    pub amount_left: i64,
    pub auto_tmm: bool,
    pub tracker: String,
    pub trackers_count: u32,
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
            downloaded: 1_000_000,
            uploaded: 5_000_000,
            ratio: 5.0,
            save_path: "/data/".into(),
            category: "movies".into(),
            tags: "hd,bluray".into(),
            added_on: 1_700_000_000,
            completion_on: 1_700_001_000,
            num_leechs: 2,
            num_seeds: 10,
            dlspeed: 0,
            upspeed: 1_000_000,
            eta: -1,
            progress: 1.0,
            priority: 0,
            amount_left: 0,
            auto_tmm: false,
            tracker: "http://tracker.example.com/announce".into(),
            trackers_count: 1,
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
}
