use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;

use crate::rtorrent::torrents::{LiveSummary, RawTorrent};
use crate::rtorrent::TransferRates;

pub mod qbittorrent;
pub mod rtorrent;

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
