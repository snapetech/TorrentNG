use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;

use crate::rtorrent::files::RawFile;
use crate::rtorrent::torrents::{LiveSummary, RawTorrent};
use crate::rtorrent::trackers::RawTracker;
use crate::rtorrent::TransferRates;

pub mod deluge;
pub mod qbittorrent;
pub mod rtorrent;
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
    pub supports_runtime_user_agent: bool,
    pub supports_config_overlay: bool,
    pub supports_restart: bool,
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
    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()>;
    async fn set_category(&self, hash: &str, category: &str) -> Result<()>;

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
}
