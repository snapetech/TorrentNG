use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

use super::{BackendCapabilities, BackendStatus, BackendType, TorrentBackend};
use crate::rtorrent::{
    files::RawFile,
    torrents::{LiveSummary, RawTorrent},
    trackers::RawTracker,
    Client, TransferRates,
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

    async fn add_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        self.client
            .load_magnet(magnet, save_path, category, start)
            .await
    }

    async fn add_torrent(
        &self,
        data: &[u8],
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        self.client
            .load_torrent(data, save_path, category, start)
            .await
    }

    async fn remove(&self, hash: &str, delete_data: bool) -> Result<()> {
        self.client.remove(hash, delete_data).await
    }

    async fn start(&self, hash: &str) -> Result<()> {
        self.client.start(hash).await
    }

    async fn stop(&self, hash: &str) -> Result<()> {
        self.client.stop(hash).await
    }

    async fn recheck(&self, hash: &str) -> Result<()> {
        self.client.recheck(hash).await
    }

    async fn reannounce(&self, hash: &str) -> Result<()> {
        self.client.reannounce(hash).await
    }

    async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>> {
        self.client.list_trackers(hash).await
    }

    async fn add_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.client.add_tracker(hash, url).await
    }

    async fn edit_tracker(&self, hash: &str, original_url: &str, new_url: &str) -> Result<()> {
        self.client.edit_tracker(hash, original_url, new_url).await
    }

    async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.client.remove_tracker(hash, url).await
    }

    async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>> {
        self.client.list_files(hash).await
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()> {
        self.client
            .set_file_priority(hash, file_index, priority)
            .await
    }

    async fn set_category(&self, hash: &str, category: &str) -> Result<()> {
        self.client.set_category(hash, category).await
    }

    async fn has_bounded_sync(&self) -> bool {
        self.client.has_multicall_range().await
    }

    async fn list_torrents_range(
        &self,
        view: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<RawTorrent>> {
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
