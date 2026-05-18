use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Url;

use super::{BackendCapabilities, BackendStatus, BackendType, TorrentBackend};
use crate::{
    config::QbittorrentConfig,
    rtorrent::{
        torrents::RawTorrent,
        TransferRates,
    },
};

pub struct QbittorrentBackend {
    client: reqwest::Client,
    base_url: Url,
    username: Option<String>,
    password: Option<String>,
    no_auth: bool,
}

impl QbittorrentBackend {
    pub fn new(cfg: &QbittorrentConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs.max(1)))
            .danger_accept_invalid_certs(cfg.accept_invalid_certs)
            .build()
            .context("create qBittorrent Web API client")?;
        let base_url = Url::parse(cfg.url.trim()).context("parse qbittorrent.url")?;
        Ok(Self {
            client,
            base_url,
            username: cfg.username.clone(),
            password: cfg.password.clone(),
            no_auth: cfg.no_auth,
        })
    }

    async fn ensure_login(&self) -> Result<()> {
        if self.no_auth {
            return Ok(());
        }
        let Some(username) = &self.username else {
            return Ok(());
        };
        let password = self.password.as_deref().unwrap_or("");
        let response = self
            .client
            .post(self.url("api/v2/auth/login")?)
            .form(&[("username", username.as_str()), ("password", password)])
            .send()
            .await
            .context("qBittorrent login request")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() || body.trim() != "Ok." {
            bail!("qBittorrent login failed with status {status}");
        }
        Ok(())
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url.join(path).context("build qBittorrent URL")
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.ensure_login().await?;
        Ok(self
            .client
            .get(self.url(path)?)
            .send()
            .await
            .with_context(|| format!("qBittorrent GET {path}"))?
            .error_for_status()
            .with_context(|| format!("qBittorrent GET {path}"))?
            .json()
            .await
            .with_context(|| format!("decode qBittorrent GET {path}"))?)
    }
}

#[derive(Debug, serde::Deserialize)]
struct QbitTorrent {
    hash: String,
    name: String,
    size: Option<i64>,
    completed: Option<i64>,
    dlspeed: Option<i64>,
    upspeed: Option<i64>,
    uploaded: Option<i64>,
    downloaded: Option<i64>,
    ratio: Option<f64>,
    state: Option<String>,
    priority: Option<i64>,
    category: Option<String>,
    save_path: Option<String>,
    added_on: Option<i64>,
    completion_on: Option<i64>,
    num_complete: Option<i64>,
    num_seeds: Option<i64>,
    num_leechs: Option<i64>,
    tracker: Option<String>,
    tags: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct QbitTransferInfo {
    dl_info_speed: Option<i64>,
    up_info_speed: Option<i64>,
}

#[async_trait]
impl TorrentBackend for QbittorrentBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::Qbittorrent
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_tags: true,
            supports_categories: true,
            supports_file_priority: true,
            supports_tracker_edit: true,
            supports_recheck: true,
            supports_runtime_user_agent: false,
            supports_config_overlay: false,
            supports_restart: false,
        }
    }

    async fn health(&self) -> BackendStatus {
        match self.get_json::<serde_json::Value>("api/v2/app/version").await {
            Ok(_) => BackendStatus::Connected,
            Err(_) => BackendStatus::Unreachable,
        }
    }

    async fn transfer_rates(&self) -> Result<TransferRates> {
        let info: QbitTransferInfo = self.get_json("api/v2/transfer/info").await?;
        Ok(TransferRates {
            download: info.dl_info_speed.unwrap_or(0).max(0),
            upload: info.up_info_speed.unwrap_or(0).max(0),
        })
    }

    async fn list_torrents(&self) -> Result<Vec<RawTorrent>> {
        let torrents: Vec<QbitTorrent> = self.get_json("api/v2/torrents/info").await?;
        Ok(torrents.into_iter().map(map_torrent).collect())
    }
}

fn map_torrent(t: QbitTorrent) -> RawTorrent {
    let state_name = t.state.unwrap_or_default();
    let complete = matches!(
        state_name.as_str(),
        "uploading" | "stalledUP" | "queuedUP" | "checkingUP" | "forcedUP"
    );
    let is_active = !matches!(
        state_name.as_str(),
        "pausedUP" | "pausedDL" | "stoppedUP" | "stoppedDL" | "error" | "missingFiles"
    );
    let message = if matches!(state_name.as_str(), "error" | "missingFiles") {
        state_name.clone()
    } else {
        String::new()
    };
    RawTorrent {
        hash: t.hash,
        name: t.name,
        size_bytes: t.size.unwrap_or(0),
        bytes_done: t.completed.unwrap_or(0),
        down_rate: t.dlspeed.unwrap_or(0),
        up_rate: t.upspeed.unwrap_or(0),
        up_total: t.uploaded.unwrap_or(0),
        down_total: t.downloaded.unwrap_or(0),
        ratio: (t.ratio.unwrap_or(0.0) * 1000.0) as i64,
        is_active,
        is_open: is_active,
        complete,
        state: if message.is_empty() { 1 } else { 3 },
        priority: t.priority.unwrap_or(0),
        category: t.category.unwrap_or_default(),
        base_path: t.save_path.clone().unwrap_or_default(),
        directory: t.save_path.unwrap_or_default(),
        creation_date: t.added_on.unwrap_or(0),
        timestamp_finished: t.completion_on.unwrap_or(0),
        tracker_focus: 0,
        peers_connected: t.num_seeds.unwrap_or(0).saturating_add(t.num_leechs.unwrap_or(0)),
        peers_complete: t.num_complete.unwrap_or(0),
        message,
        tracker_url: t.tracker.unwrap_or_default(),
    }
}
