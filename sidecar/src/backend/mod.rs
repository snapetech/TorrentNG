use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::Serialize;
use std::{collections::BTreeMap, net::SocketAddr};

use crate::rtorrent::files::RawFile;
use crate::rtorrent::torrents::{LiveSummary, RawTorrent};
use crate::rtorrent::trackers::RawTracker;
use crate::rtorrent::TransferRates;

pub mod deluge;
pub mod qbittorrent;
pub mod rtorrent;
pub mod torrentng;
pub mod transmission;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendType {
    Rtorrent,
    Qbittorrent,
    Transmission,
    Deluge,
    Torrentng,
}

impl BackendType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rtorrent => "rtorrent",
            Self::Qbittorrent => "qbittorrent",
            Self::Transmission => "transmission",
            Self::Deluge => "deluge",
            Self::Torrentng => "torrentng",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendStatus {
    Connected,
    Unreachable,
}

impl BackendStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Unreachable => "unreachable",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BackendHealth {
    #[serde(rename = "type")]
    pub backend_type: &'static str,
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackendCapabilities {
    pub supports_tags: bool,
    pub supports_categories: bool,
    pub supports_file_priority: bool,
    pub supports_tracker_edit: bool,
    pub supports_recheck: bool,
    pub supports_torrent_export: bool,
    pub supports_webseed_reads: bool,
    pub supports_piece_state_reads: bool,
    pub supports_piece_hash_reads: bool,
    pub supports_peer_snapshots: bool,
    pub supports_peer_add: bool,
    pub supports_peer_ban: bool,
    pub supports_queue_order: bool,
    pub supports_per_torrent_limits: bool,
    pub supports_global_limits: bool,
    pub supports_share_limits: bool,
    pub supports_mode_flags: bool,
    pub supports_location_update: bool,
    pub supports_torrent_rename: bool,
    pub supports_file_rename: bool,
    pub supports_runtime_user_agent: bool,
    pub supports_config_overlay: bool,
    pub supports_restart: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BackendTransferLimits {
    pub download_limit: i64,
    pub upload_limit: i64,
    pub speed_limits_mode: bool,
}

#[async_trait]
pub trait TorrentBackend: Send + Sync {
    fn backend_type(&self) -> BackendType;
    fn capabilities(&self) -> BackendCapabilities;

    async fn health(&self) -> BackendStatus;
    async fn transfer_rates(&self) -> Result<TransferRates>;
    async fn list_torrents(&self) -> Result<Vec<RawTorrent>>;
    async fn add_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()>;
    async fn add_torrent(
        &self,
        data: &[u8],
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()>;
    async fn torrent_blob(&self, _hash: &str) -> Result<Vec<u8>> {
        bail!(
            "{} backend does not support torrent export",
            self.backend_type().as_str()
        )
    }
    async fn add_url(&self, url: &str, save_path: &str, category: &str, start: bool) -> Result<()> {
        if url.starts_with("magnet:") {
            return self.add_magnet(url, save_path, category, start).await;
        }
        let data = reqwest::get(url).await?.error_for_status()?.bytes().await?;
        self.add_torrent(&data, save_path, category, start).await
    }
    async fn remove(&self, hash: &str, delete_data: bool) -> Result<()>;
    async fn start(&self, hash: &str) -> Result<()>;
    async fn stop(&self, hash: &str) -> Result<()>;
    async fn recheck(&self, hash: &str) -> Result<()>;
    async fn reannounce(&self, hash: &str) -> Result<()>;
    async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>>;
    async fn add_tracker(&self, hash: &str, url: &str) -> Result<()>;
    async fn edit_tracker(&self, hash: &str, original_url: &str, new_url: &str) -> Result<()>;
    async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()>;
    async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>>;
    async fn list_webseeds(&self, _hash: &str) -> Result<Vec<String>> {
        bail!(
            "{} backend does not support webseed reads",
            self.backend_type().as_str()
        )
    }

    async fn piece_states(&self, _hash: &str) -> Result<Vec<BackendPieceState>> {
        bail!(
            "{} backend does not support piece state reads",
            self.backend_type().as_str()
        )
    }

    async fn piece_hashes(&self, _hash: &str) -> Result<Vec<String>> {
        bail!(
            "{} backend does not support piece hash reads",
            self.backend_type().as_str()
        )
    }

    async fn list_peers(&self, _hash: &str) -> Result<Vec<BackendPeer>> {
        bail!(
            "{} backend does not support peer snapshot reads",
            self.backend_type().as_str()
        )
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()>;
    async fn set_category(&self, hash: &str, category: &str) -> Result<()>;
    async fn set_location(&self, _hash: &str, _location: &str) -> Result<()> {
        bail!(
            "{} backend does not support location updates",
            self.backend_type().as_str()
        )
    }
    async fn rename_torrent(&self, _hash: &str, _name: &str) -> Result<()> {
        bail!(
            "{} backend does not support torrent renames",
            self.backend_type().as_str()
        )
    }
    async fn rename_file(&self, _hash: &str, _file_index: usize, _name: &str) -> Result<()> {
        bail!(
            "{} backend does not support file renames",
            self.backend_type().as_str()
        )
    }
    async fn set_share_limits(
        &self,
        _hash: &str,
        _ratio_limit_milli: i64,
        _seeding_time_limit: i64,
    ) -> Result<()> {
        bail!(
            "{} backend does not support share limits",
            self.backend_type().as_str()
        )
    }
    async fn set_download_limit(&self, _hash: &str, _limit: Option<i64>) -> Result<()> {
        bail!(
            "{} backend does not support per-torrent download limits",
            self.backend_type().as_str()
        )
    }

    async fn set_upload_limit(&self, _hash: &str, _limit: Option<i64>) -> Result<()> {
        bail!(
            "{} backend does not support per-torrent upload limits",
            self.backend_type().as_str()
        )
    }

    async fn download_limits(&self, _hashes: &[String]) -> Result<BTreeMap<String, i64>> {
        bail!(
            "{} backend does not support per-torrent download limit reads",
            self.backend_type().as_str()
        )
    }

    async fn upload_limits(&self, _hashes: &[String]) -> Result<BTreeMap<String, i64>> {
        bail!(
            "{} backend does not support per-torrent upload limit reads",
            self.backend_type().as_str()
        )
    }

    async fn set_global_download_limit(&self, _limit: i64) -> Result<()> {
        bail!(
            "{} backend does not support global download limits",
            self.backend_type().as_str()
        )
    }

    async fn global_limits(&self) -> Result<BackendTransferLimits> {
        bail!(
            "{} backend does not support global transfer limit reads",
            self.backend_type().as_str()
        )
    }

    async fn set_global_upload_limit(&self, _limit: i64) -> Result<()> {
        bail!(
            "{} backend does not support global upload limits",
            self.backend_type().as_str()
        )
    }

    async fn toggle_global_speed_limits_mode(&self) -> Result<()> {
        bail!(
            "{} backend does not support global speed-limit mode toggles",
            self.backend_type().as_str()
        )
    }

    async fn toggle_sequential_download(&self, _hash: &str) -> Result<()> {
        bail!(
            "{} backend does not support sequential download toggles",
            self.backend_type().as_str()
        )
    }

    async fn toggle_first_last_piece_priority(&self, _hash: &str) -> Result<()> {
        bail!(
            "{} backend does not support first/last piece priority toggles",
            self.backend_type().as_str()
        )
    }

    async fn set_force_start(&self, _hash: &str, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support force-start updates",
            self.backend_type().as_str()
        )
    }

    async fn set_super_seeding(&self, _hash: &str, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support super-seeding updates",
            self.backend_type().as_str()
        )
    }

    async fn set_auto_tmm(&self, _hash: &str, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support automatic torrent management updates",
            self.backend_type().as_str()
        )
    }

    async fn set_auto_management(&self, _hash: &str, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support automatic management updates",
            self.backend_type().as_str()
        )
    }

    async fn add_peers(&self, _hash: &str, _peers: &[SocketAddr]) -> Result<()> {
        bail!(
            "{} backend does not support explicit peer adds",
            self.backend_type().as_str()
        )
    }

    async fn update_queue_order(&self, _hashes: &[String], _queue_move: QueueMove) -> Result<()> {
        bail!(
            "{} backend does not support queue order updates",
            self.backend_type().as_str()
        )
    }

    async fn ban_peers(&self, _peers: &[SocketAddr]) -> Result<()> {
        bail!(
            "{} backend does not support peer bans",
            self.backend_type().as_str()
        )
    }

    async fn add_tags(&self, _hash: &str, _tags: &[&str]) -> Result<()> {
        Ok(())
    }
    async fn remove_tags(&self, _hash: &str, _tags: &[&str]) -> Result<()> {
        Ok(())
    }
    async fn set_tags(&self, _hash: &str, _tags: &[&str]) -> Result<()> {
        Ok(())
    }

    async fn has_bounded_sync(&self) -> bool {
        false
    }

    async fn list_torrents_range(
        &self,
        _view: &str,
        _offset: i64,
        _limit: i64,
    ) -> Result<Vec<RawTorrent>> {
        self.list_torrents().await
    }

    async fn live_summary(&self, _view: &str, _limit: i64) -> Result<LiveSummary> {
        Ok(LiveSummary {
            rates: self.transfer_rates().await?,
            moving: Vec::new(),
        })
    }

    async fn feature_status(&self) -> (String, String) {
        ("unknown".to_owned(), "unknown".to_owned())
    }

    async fn set_dht(&self, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support runtime DHT toggles",
            self.backend_type().as_str()
        )
    }

    async fn set_pex(&self, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support runtime peer exchange toggles",
            self.backend_type().as_str()
        )
    }

    async fn get_user_agent(&self) -> Result<String> {
        bail!(
            "{} backend does not support runtime user-agent reads",
            self.backend_type().as_str()
        )
    }

    async fn set_user_agent(&self, _user_agent: &str) -> Result<()> {
        bail!(
            "{} backend does not support runtime user-agent updates",
            self.backend_type().as_str()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueMove {
    Up,
    Down,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendPieceState {
    Missing,
    Partial,
    Complete,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackendPeer {
    pub addr: SocketAddr,
    pub client: String,
    pub progress: f64,
    pub download_rate: i64,
    pub upload_rate: i64,
    pub downloaded: i64,
    pub uploaded: i64,
}
