use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Url;

use super::{BackendCapabilities, BackendStatus, BackendType, TorrentBackend};
use crate::{
    config::QbittorrentConfig,
    rtorrent::{files::RawFile, torrents::RawTorrent, trackers::RawTracker, TransferRates},
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

    async fn post_form(&self, path: &str, form: &[(&str, &str)]) -> Result<()> {
        self.ensure_login().await?;
        self.client
            .post(self.url(path)?)
            .form(form)
            .send()
            .await
            .with_context(|| format!("qBittorrent POST {path}"))?
            .error_for_status()
            .with_context(|| format!("qBittorrent POST {path}"))?;
        Ok(())
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

#[derive(Debug, serde::Deserialize)]
struct QbitTracker {
    url: String,
    status: Option<i64>,
    msg: Option<String>,
    num_seeds: Option<i64>,
    num_leeches: Option<i64>,
    num_downloaded: Option<i64>,
}

#[derive(Debug, serde::Deserialize)]
struct QbitFile {
    name: String,
    size: i64,
    progress: f64,
    priority: i64,
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
        match self
            .get_json::<serde_json::Value>("api/v2/app/version")
            .await
        {
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

    async fn add_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        self.post_form(
            "api/v2/torrents/add",
            &[
                ("urls", magnet),
                ("savepath", save_path),
                ("category", category),
                ("paused", if start { "false" } else { "true" }),
            ],
        )
        .await
    }

    async fn add_torrent(
        &self,
        data: &[u8],
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        self.ensure_login().await?;
        let part = reqwest::multipart::Part::bytes(data.to_vec()).file_name("upload.torrent");
        let form = reqwest::multipart::Form::new()
            .part("torrents", part)
            .text("savepath", save_path.to_owned())
            .text("category", category.to_owned())
            .text("paused", if start { "false" } else { "true" });
        self.client
            .post(self.url("api/v2/torrents/add")?)
            .multipart(form)
            .send()
            .await
            .context("qBittorrent POST api/v2/torrents/add")?
            .error_for_status()
            .context("qBittorrent POST api/v2/torrents/add")?;
        Ok(())
    }

    async fn add_url(&self, url: &str, save_path: &str, category: &str, start: bool) -> Result<()> {
        self.add_magnet(url, save_path, category, start).await
    }

    async fn remove(&self, hash: &str, delete_data: bool) -> Result<()> {
        self.post_form(
            "api/v2/torrents/delete",
            &[
                ("hashes", hash),
                ("deleteFiles", if delete_data { "true" } else { "false" }),
            ],
        )
        .await
    }

    async fn start(&self, hash: &str) -> Result<()> {
        self.post_form("api/v2/torrents/resume", &[("hashes", hash)])
            .await
    }

    async fn stop(&self, hash: &str) -> Result<()> {
        self.post_form("api/v2/torrents/pause", &[("hashes", hash)])
            .await
    }

    async fn recheck(&self, hash: &str) -> Result<()> {
        self.post_form("api/v2/torrents/recheck", &[("hashes", hash)])
            .await
    }

    async fn reannounce(&self, hash: &str) -> Result<()> {
        self.post_form("api/v2/torrents/reannounce", &[("hashes", hash)])
            .await
    }

    async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>> {
        let trackers: Vec<QbitTracker> = self
            .get_json(&format!(
                "api/v2/torrents/trackers?hash={}",
                urlencoding::encode(hash)
            ))
            .await?;
        Ok(trackers.into_iter().enumerate().map(map_tracker).collect())
    }

    async fn add_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.post_form(
            "api/v2/torrents/addTrackers",
            &[("hash", hash), ("urls", url)],
        )
        .await
    }

    async fn edit_tracker(&self, hash: &str, original_url: &str, new_url: &str) -> Result<()> {
        self.post_form(
            "api/v2/torrents/editTracker",
            &[
                ("hash", hash),
                ("origUrl", original_url),
                ("newUrl", new_url),
            ],
        )
        .await
    }

    async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.post_form(
            "api/v2/torrents/removeTrackers",
            &[("hash", hash), ("urls", url)],
        )
        .await
    }

    async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>> {
        let files: Vec<QbitFile> = self
            .get_json(&format!(
                "api/v2/torrents/files?hash={}",
                urlencoding::encode(hash)
            ))
            .await?;
        Ok(files.into_iter().enumerate().map(map_file).collect())
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()> {
        self.post_form(
            "api/v2/torrents/filePrio",
            &[
                ("hash", hash),
                ("id", &file_index.to_string()),
                ("priority", &priority.to_string()),
            ],
        )
        .await
    }

    async fn set_category(&self, hash: &str, category: &str) -> Result<()> {
        self.post_form(
            "api/v2/torrents/setCategory",
            &[("hashes", hash), ("category", category)],
        )
        .await
    }

    async fn set_location(&self, hash: &str, location: &str) -> Result<()> {
        self.post_form(
            "api/v2/torrents/setLocation",
            &[("hashes", hash), ("location", location)],
        )
        .await
    }

    async fn rename_torrent(&self, hash: &str, name: &str) -> Result<()> {
        self.post_form("api/v2/torrents/rename", &[("hash", hash), ("name", name)])
            .await
    }

    async fn rename_file(&self, hash: &str, file_index: usize, name: &str) -> Result<()> {
        self.post_form(
            "api/v2/torrents/renameFile",
            &[
                ("hash", hash),
                ("id", &file_index.to_string()),
                ("name", name),
            ],
        )
        .await
    }

    async fn set_share_limits(
        &self,
        hash: &str,
        ratio_limit_milli: i64,
        seeding_time_limit: i64,
    ) -> Result<()> {
        let ratio_limit = if ratio_limit_milli >= 0 {
            (ratio_limit_milli as f64 / 1000.0).to_string()
        } else {
            ratio_limit_milli.to_string()
        };
        self.post_form(
            "api/v2/torrents/setShareLimits",
            &[
                ("hashes", hash),
                ("ratioLimit", &ratio_limit),
                ("seedingTimeLimit", &seeding_time_limit.to_string()),
            ],
        )
        .await
    }

    async fn set_download_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        let limit = limit.unwrap_or(0).max(0).to_string();
        self.post_form(
            "api/v2/torrents/setDownloadLimit",
            &[("hashes", hash), ("limit", &limit)],
        )
        .await
    }

    async fn set_upload_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        let limit = limit.unwrap_or(0).max(0).to_string();
        self.post_form(
            "api/v2/torrents/setUploadLimit",
            &[("hashes", hash), ("limit", &limit)],
        )
        .await
    }

    async fn set_global_download_limit(&self, limit: i64) -> Result<()> {
        self.post_form(
            "api/v2/transfer/setDownloadLimit",
            &[("limit", &limit.max(0).to_string())],
        )
        .await
    }

    async fn set_global_upload_limit(&self, limit: i64) -> Result<()> {
        self.post_form(
            "api/v2/transfer/setUploadLimit",
            &[("limit", &limit.max(0).to_string())],
        )
        .await
    }

    async fn toggle_sequential_download(&self, hash: &str) -> Result<()> {
        self.post_form(
            "api/v2/torrents/toggleSequentialDownload",
            &[("hashes", hash)],
        )
        .await
    }

    async fn toggle_first_last_piece_priority(&self, hash: &str) -> Result<()> {
        self.post_form(
            "api/v2/torrents/toggleFirstLastPiecePrio",
            &[("hashes", hash)],
        )
        .await
    }

    async fn set_force_start(&self, hash: &str, enabled: bool) -> Result<()> {
        let value = if enabled { "true" } else { "false" };
        self.post_form(
            "api/v2/torrents/setForceStart",
            &[("hashes", hash), ("value", value)],
        )
        .await
    }

    async fn set_super_seeding(&self, hash: &str, enabled: bool) -> Result<()> {
        let value = if enabled { "true" } else { "false" };
        self.post_form(
            "api/v2/torrents/setSuperSeeding",
            &[("hashes", hash), ("value", value)],
        )
        .await
    }

    async fn set_auto_tmm(&self, hash: &str, enabled: bool) -> Result<()> {
        let value = if enabled { "true" } else { "false" };
        self.post_form(
            "api/v2/torrents/setAutoTMM",
            &[("hashes", hash), ("enable", value)],
        )
        .await
    }

    async fn set_auto_management(&self, hash: &str, enabled: bool) -> Result<()> {
        let value = if enabled { "true" } else { "false" };
        self.post_form(
            "api/v2/torrents/setAutoManagement",
            &[("hashes", hash), ("enable", value)],
        )
        .await
    }

    async fn add_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        self.post_form(
            "api/v2/torrents/addTags",
            &[("hashes", hash), ("tags", &tags.join(","))],
        )
        .await
    }

    async fn remove_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        self.post_form(
            "api/v2/torrents/removeTags",
            &[("hashes", hash), ("tags", &tags.join(","))],
        )
        .await
    }

    async fn set_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        let current = self
            .list_torrents()
            .await?
            .into_iter()
            .find(|torrent| torrent.hash.eq_ignore_ascii_case(hash))
            .map(|torrent| torrent.tags)
            .unwrap_or_default();
        let current_tags: Vec<&str> = current
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .collect();
        if !current_tags.is_empty() {
            self.remove_tags(hash, &current_tags).await?;
        }
        if !tags.is_empty() {
            self.add_tags(hash, tags).await?;
        }
        Ok(())
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
        peers_connected: t
            .num_seeds
            .unwrap_or(0)
            .saturating_add(t.num_leechs.unwrap_or(0)),
        peers_complete: t.num_complete.unwrap_or(0),
        message,
        tracker_url: t.tracker.unwrap_or_default(),
        tags: t.tags.unwrap_or_default(),
    }
}

fn map_tracker((idx, tracker): (usize, QbitTracker)) -> RawTracker {
    let status = tracker.status.unwrap_or(0);
    RawTracker {
        url: tracker.url,
        id: idx as i64,
        group: 0,
        group_index: idx as i64,
        is_enabled: status != 4,
        is_open: status == 2,
        is_extra_tracker: false,
        activity_time_last: 0,
        activity_time_next: 0,
        min_interval: 0,
        normal_interval: 0,
        failed_counter: 0,
        success_counter: 0,
        scrape_incomplete: tracker.num_leeches.unwrap_or(0),
        scrape_complete: tracker.num_seeds.unwrap_or(0),
        scrape_downloaded: tracker.num_downloaded.unwrap_or(0),
        message: tracker.msg.unwrap_or_default(),
    }
}

fn map_file((index, file): (usize, QbitFile)) -> RawFile {
    RawFile {
        index,
        path: file.name,
        size_bytes: file.size,
        size_chunks: file.size,
        completed_chunks: (file.size as f64 * file.progress.clamp(0.0, 1.0)) as i64,
        priority: file.priority,
        is_created: true,
        is_open: true,
    }
}
