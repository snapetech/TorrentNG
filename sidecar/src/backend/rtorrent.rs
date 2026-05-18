use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

use super::{BackendCapabilities, BackendStatus, BackendType, TorrentBackend};
use crate::rtorrent::{
    torrents::{LiveSummary, RawTorrent},
    Client, TransferRates, XmlValue,
};

pub struct RtorrentBackend {
    client: Arc<Client>,
}

impl RtorrentBackend {
    pub fn new(client: Arc<Client>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TorrentBackend for RtorrentBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::Rtorrent
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_tags: true,
            supports_categories: true,
            supports_file_priority: true,
            supports_tracker_edit: true,
            supports_recheck: true,
            supports_runtime_user_agent: true,
            supports_config_overlay: true,
            supports_restart: true,
        }
    }

    async fn health(&self) -> BackendStatus {
        if self.client.call("system.client_version", &[]).await.is_ok() {
            BackendStatus::Connected
        } else {
            BackendStatus::Unreachable
        }
    }

    async fn transfer_rates(&self) -> Result<TransferRates> {
        self.client.transfer_rates().await
    }

    async fn list_torrents(&self) -> Result<Vec<RawTorrent>> {
        self.client.list_torrents().await
    }

    async fn has_bounded_sync(&self) -> bool {
        self.client.has_multicall_range().await
    }

    async fn list_torrents_range(&self, view: &str, offset: i64, limit: i64) -> Result<Vec<RawTorrent>> {
        self.client.list_torrents_range(view, offset, limit).await
    }

    async fn live_summary(&self, view: &str, limit: i64) -> Result<LiveSummary> {
        self.client.live_summary(view, limit).await
    }

    async fn feature_status(&self) -> (String, String) {
        let mut dht = "unknown".to_owned();
        let mut pex = "unknown".to_owned();

        if let Ok(value) = self.client.call_sync("dht.mode", &[]).await {
            if let Some(mode) = value.as_str() {
                dht = if matches!(mode, "disable" | "off" | "no") {
                    "off".to_owned()
                } else {
                    "on".to_owned()
                };
            }
        }

        if let Ok(value) = self.client.call_sync("protocol.pex", &[]).await {
            if let Some(enabled) = value.as_bool() {
                pex = if enabled { "on" } else { "off" }.to_owned();
            }
        }

        (dht, pex)
    }
}
