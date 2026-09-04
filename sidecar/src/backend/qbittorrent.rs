use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Url;
use std::{collections::BTreeMap, net::SocketAddr};
use tokio::sync::Mutex;

use super::{
    map_qbit_piece_state, parse_qbit_peer_response, response_bytes_bounded, response_json_bounded,
    validate_qbit_mutation_body, BackendCapabilities, BackendPeer, BackendPieceState,
    BackendStatus, BackendTransferLimits, BackendType, QueueMove, TorrentBackend,
    MAX_BACKEND_JSON_BYTES,
};
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
    tag_mutation: Mutex<()>,
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
            tag_mutation: Mutex::new(()),
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
        let body = super::response_bytes_bounded(
            response,
            MAX_BACKEND_JSON_BYTES.min(16 * 1024),
            "qBittorrent login",
        )
        .await?;
        if !status.is_success() || std::str::from_utf8(&body).ok().map(str::trim) != Some("Ok.") {
            bail!("qBittorrent login failed with status {status}");
        }
        Ok(())
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url.join(path).context("build qBittorrent URL")
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.ensure_login().await?;
        response_json_bounded(
            self.client
                .get(self.url(path)?)
                .send()
                .await
                .with_context(|| format!("qBittorrent GET {path}"))?,
            MAX_BACKEND_JSON_BYTES,
            &format!("qBittorrent GET {path}"),
        )
        .await
    }

    async fn post_form(&self, path: &str, form: &[(&str, &str)]) -> Result<()> {
        self.ensure_login().await?;
        let response = self
            .client
            .post(self.url(path)?)
            .form(form)
            .send()
            .await
            .with_context(|| format!("qBittorrent POST {path}"))?;
        let body = response_bytes_bounded(response, 16 * 1024, &format!("qBittorrent POST {path}"))
            .await?;
        validate_qbit_mutation_body(&body, path)?;
        Ok(())
    }

    async fn limit_map(&self, path: &str, hashes: &[String]) -> Result<BTreeMap<String, i64>> {
        self.ensure_login().await?;
        let hashes_param = hashes.join("|");
        let mut url = self.url(path)?;
        url.query_pairs_mut().append_pair("hashes", &hashes_param);
        let limits: BTreeMap<String, i64> = response_json_bounded(
            self.client
                .get(url)
                .send()
                .await
                .with_context(|| format!("qBittorrent GET {path}"))?,
            MAX_BACKEND_JSON_BYTES,
            &format!("qBittorrent GET {path}"),
        )
        .await?;
        let mut result = BTreeMap::new();
        for hash in hashes {
            let value = limits
                .iter()
                .find(|(returned_hash, _)| returned_hash.eq_ignore_ascii_case(hash))
                .map(|(_, value)| *value)
                .ok_or_else(|| {
                    anyhow::anyhow!("qBittorrent {path} response omitted requested torrent {hash}")
                })?;
            if value < 0 {
                bail!("qBittorrent {path} returned negative limit for {hash}");
            }
            result.insert(hash.clone(), value);
        }
        Ok(result)
    }

    async fn add_tags_unlocked(&self, hash: &str, tags: &[&str]) -> Result<()> {
        self.post_form(
            "api/v2/torrents/addTags",
            &[("hashes", hash), ("tags", &tags.join(","))],
        )
        .await
    }

    async fn remove_tags_unlocked(&self, hash: &str, tags: &[&str]) -> Result<()> {
        self.post_form(
            "api/v2/torrents/removeTags",
            &[("hashes", hash), ("tags", &tags.join(","))],
        )
        .await
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
    dl_info_speed: i64,
    up_info_speed: i64,
    dl_rate_limit: i64,
    up_rate_limit: i64,
    use_alt_speed_limits: bool,
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
            supports_torrent_export: true,
            supports_webseed_reads: true,
            supports_piece_state_reads: true,
            supports_piece_hash_reads: true,
            supports_peer_snapshots: true,
            supports_peer_add: true,
            supports_peer_ban: true,
            supports_queue_order: true,
            supports_per_torrent_limits: true,
            supports_global_limits: true,
            supports_share_limits: true,
            supports_mode_flags: true,
            supports_location_update: true,
            supports_torrent_rename: true,
            supports_file_rename: true,
            supports_runtime_user_agent: false,
            supports_config_overlay: false,
            supports_restart: false,
        }
    }

    async fn health(&self) -> BackendStatus {
        match self.get_json::<String>("api/v2/app/version").await {
            Ok(version) if !version.trim().is_empty() => BackendStatus::Connected,
            Err(_) => BackendStatus::Unreachable,
            Ok(_) => BackendStatus::Unreachable,
        }
    }

    async fn transfer_rates(&self) -> Result<TransferRates> {
        let info: QbitTransferInfo = self.get_json("api/v2/transfer/info").await?;
        Ok(TransferRates {
            download: qbit_nonnegative_i64(Some(info.dl_info_speed), "dl_info_speed")?,
            upload: qbit_nonnegative_i64(Some(info.up_info_speed), "up_info_speed")?,
        })
    }

    async fn list_torrents(&self) -> Result<Vec<RawTorrent>> {
        let torrents: Vec<QbitTorrent> = self.get_json("api/v2/torrents/info").await?;
        torrents.into_iter().map(map_torrent).collect()
    }

    async fn has_bounded_sync(&self) -> bool {
        // qBittorrent's torrents/info endpoint has supported offset/limit
        // pagination for the v2 API used by this facade. The sync loop still
        // treats the result as eventually consistent because qBittorrent has
        // no server-side snapshot token for this endpoint.
        true
    }

    async fn list_torrents_range(
        &self,
        view: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<RawTorrent>> {
        let mut url = self.url("api/v2/torrents/info")?;
        let filter = qbit_sync_filter(view);
        url.query_pairs_mut()
            .append_pair("filter", filter)
            .append_pair("sort", "hash")
            .append_pair("offset", &offset.max(0).to_string())
            .append_pair("limit", &limit.clamp(1, 5_000).to_string());
        self.ensure_login().await?;
        let torrents: Vec<QbitTorrent> = response_json_bounded(
            self.client
                .get(url)
                .send()
                .await
                .context("qBittorrent paged torrents/info request")?,
            MAX_BACKEND_JSON_BYTES,
            "qBittorrent paged torrents/info",
        )
        .await?;
        if torrents.len() > limit.clamp(1, 5_000) as usize {
            bail!("qBittorrent paged torrents/info exceeded requested page size");
        }
        torrents.into_iter().map(map_torrent).collect()
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
        let response = self
            .client
            .post(self.url("api/v2/torrents/add")?)
            .multipart(form)
            .send()
            .await
            .context("qBittorrent POST api/v2/torrents/add")?;
        let body =
            response_bytes_bounded(response, 16 * 1024, "qBittorrent POST api/v2/torrents/add")
                .await?;
        validate_qbit_mutation_body(&body, "api/v2/torrents/add")?;
        Ok(())
    }

    async fn torrent_blob(&self, hash: &str) -> Result<Vec<u8>> {
        self.ensure_login().await?;
        response_bytes_bounded(
            self.client
                .get(self.url(&format!(
                    "api/v2/torrents/export?hash={}",
                    urlencoding::encode(hash)
                ))?)
                .send()
                .await
                .context("qBittorrent GET api/v2/torrents/export")?,
            MAX_BACKEND_JSON_BYTES,
            "qBittorrent GET api/v2/torrents/export",
        )
        .await
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
        trackers.into_iter().enumerate().map(map_tracker).collect()
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
        files.into_iter().enumerate().map(map_file).collect()
    }

    async fn list_webseeds(&self, hash: &str) -> Result<Vec<String>> {
        self.get_json(&format!(
            "api/v2/torrents/webseeds?hash={}",
            urlencoding::encode(hash)
        ))
        .await
    }

    async fn piece_states(&self, hash: &str) -> Result<Vec<BackendPieceState>> {
        let states: Vec<i64> = self
            .get_json(&format!(
                "api/v2/torrents/pieceStates?hash={}",
                urlencoding::encode(hash)
            ))
            .await?;
        states.into_iter().map(map_qbit_piece_state).collect()
    }

    async fn piece_hashes(&self, hash: &str) -> Result<Vec<String>> {
        self.get_json(&format!(
            "api/v2/torrents/pieceHashes?hash={}",
            urlencoding::encode(hash)
        ))
        .await
    }

    async fn list_peers(&self, hash: &str) -> Result<Vec<BackendPeer>> {
        let response: serde_json::Value = self
            .get_json(&format!(
                "api/v2/sync/torrentPeers?hash={}",
                urlencoding::encode(hash)
            ))
            .await?;
        parse_qbit_peer_response(&response)
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()> {
        let wire_priority = match priority {
            0 => 0,
            1 => 1,
            2 => 6,
            _ => bail!("qBittorrent file priority must be between 0 and 2"),
        };
        self.post_form(
            "api/v2/torrents/filePrio",
            &[
                ("hash", hash),
                ("id", &file_index.to_string()),
                ("priority", &wire_priority.to_string()),
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

    async fn download_limits(&self, hashes: &[String]) -> Result<BTreeMap<String, i64>> {
        self.limit_map("api/v2/torrents/downloadLimit", hashes)
            .await
    }

    async fn upload_limits(&self, hashes: &[String]) -> Result<BTreeMap<String, i64>> {
        self.limit_map("api/v2/torrents/uploadLimit", hashes).await
    }

    async fn set_global_download_limit(&self, limit: i64) -> Result<()> {
        self.post_form(
            "api/v2/transfer/setDownloadLimit",
            &[("limit", &limit.max(0).to_string())],
        )
        .await
    }

    async fn global_limits(&self) -> Result<BackendTransferLimits> {
        let info: QbitTransferInfo = self.get_json("api/v2/transfer/info").await?;
        Ok(BackendTransferLimits {
            download_limit: qbit_nonnegative_i64(Some(info.dl_rate_limit), "dl_rate_limit")?,
            upload_limit: qbit_nonnegative_i64(Some(info.up_rate_limit), "up_rate_limit")?,
            speed_limits_mode: info.use_alt_speed_limits,
        })
    }

    async fn set_global_upload_limit(&self, limit: i64) -> Result<()> {
        self.post_form(
            "api/v2/transfer/setUploadLimit",
            &[("limit", &limit.max(0).to_string())],
        )
        .await
    }

    async fn toggle_global_speed_limits_mode(&self) -> Result<()> {
        self.post_form("api/v2/transfer/toggleSpeedLimitsMode", &[])
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

    async fn add_peers(&self, hash: &str, peers: &[SocketAddr]) -> Result<()> {
        let peers = peers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("|");
        self.post_form(
            "api/v2/torrents/addPeers",
            &[("hashes", hash), ("peers", &peers)],
        )
        .await
    }

    async fn update_queue_order(&self, hashes: &[String], queue_move: QueueMove) -> Result<()> {
        let hashes = hashes.join("|");
        let path = match queue_move {
            QueueMove::Up => "api/v2/torrents/increasePrio",
            QueueMove::Down => "api/v2/torrents/decreasePrio",
            QueueMove::Top => "api/v2/torrents/topPrio",
            QueueMove::Bottom => "api/v2/torrents/bottomPrio",
        };
        self.post_form(path, &[("hashes", &hashes)]).await
    }

    async fn ban_peers(&self, peers: &[SocketAddr]) -> Result<()> {
        let peers = peers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("|");
        self.post_form("api/v2/transfer/banPeers", &[("peers", &peers)])
            .await
    }

    async fn add_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        let _guard = self.tag_mutation.lock().await;
        self.add_tags_unlocked(hash, tags).await
    }

    async fn remove_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        let _guard = self.tag_mutation.lock().await;
        self.remove_tags_unlocked(hash, tags).await
    }

    async fn set_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        // qBittorrent has no atomic replace-tags endpoint. Serialize the
        // read/remove/add sequence so concurrent compatibility requests do
        // not erase one another's changes, and restore the previous set when
        // the add step fails after removal succeeded.
        let _guard = self.tag_mutation.lock().await;
        let mut url = self.url("api/v2/torrents/info")?;
        url.query_pairs_mut().append_pair("hashes", hash);
        self.ensure_login().await?;
        let torrents: Vec<QbitTorrent> = response_json_bounded(
            self.client
                .get(url)
                .send()
                .await
                .context("qBittorrent torrent tag lookup")?,
            MAX_BACKEND_JSON_BYTES,
            "qBittorrent torrent tag lookup",
        )
        .await?;
        let current = torrents
            .into_iter()
            .find(|torrent| torrent.hash.eq_ignore_ascii_case(hash))
            .ok_or_else(|| anyhow::anyhow!("qBittorrent torrent not found"))?
            .tags
            .unwrap_or_default();
        let current_tags: Vec<&str> = current
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .collect();
        if !current_tags.is_empty() {
            self.remove_tags_unlocked(hash, &current_tags).await?;
        }
        if !tags.is_empty() {
            if let Err(error) = self.add_tags_unlocked(hash, tags).await {
                if !current_tags.is_empty() {
                    if let Err(rollback_error) = self.add_tags_unlocked(hash, &current_tags).await {
                        bail!(
                            "qBittorrent tag replacement failed: {error}; rollback failed: {rollback_error}"
                        );
                    }
                }
                return Err(error);
            }
        }
        Ok(())
    }
}

fn qbit_sync_filter(view: &str) -> &str {
    match view {
        "active" | "downloading" | "completed" | "paused" | "inactive" | "errored" | "resumed"
        | "stalled" | "seeding" | "checking" | "moving" | "missingFiles" => view,
        _ => "all",
    }
}

fn map_torrent(t: QbitTorrent) -> Result<RawTorrent> {
    let hash = t.hash;
    if hash.trim().is_empty() {
        bail!("qBittorrent torrent omitted hash");
    }
    let state_name = t
        .state
        .filter(|state| !state.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("qBittorrent torrent omitted state"))?;
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
    let size_bytes = qbit_nonnegative_i64(t.size, "size")?;
    let bytes_done = qbit_nonnegative_i64(t.completed, "completed")?;
    if bytes_done > size_bytes {
        bail!(
            "qBittorrent torrent {} reports {} completed bytes for size {}",
            hash,
            bytes_done,
            size_bytes
        );
    }
    let category = t.category.unwrap_or_default();
    let save_path = t
        .save_path
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("qBittorrent torrent {} omitted save_path", hash))?;
    let ratio = t
        .ratio
        .ok_or_else(|| anyhow::anyhow!("qBittorrent torrent {} omitted ratio", hash))?;
    if !ratio.is_finite() || ratio < 0.0 {
        bail!("qBittorrent torrent {} returned invalid ratio", hash);
    }
    let num_seeds = qbit_nonnegative_i64(t.num_seeds, "num_seeds")?;
    let num_leechs = qbit_nonnegative_i64(t.num_leechs, "num_leechs")?;
    let num_complete = qbit_nonnegative_i64(t.num_complete, "num_complete")?;
    Ok(RawTorrent {
        hash: hash.clone(),
        name: if t.name.trim().is_empty() {
            bail!("qBittorrent torrent omitted name")
        } else {
            t.name
        },
        size_bytes,
        bytes_done,
        down_rate: qbit_nonnegative_i64(t.dlspeed, "dlspeed")?,
        up_rate: qbit_nonnegative_i64(t.upspeed, "upspeed")?,
        up_total: qbit_nonnegative_i64(t.uploaded, "uploaded")?,
        down_total: qbit_nonnegative_i64(t.downloaded, "downloaded")?,
        ratio: super::ratio_milli(Some(ratio)),
        is_active,
        is_open: is_active,
        complete,
        state: if message.is_empty() { 1 } else { 3 },
        priority: t
            .priority
            .ok_or_else(|| anyhow::anyhow!("qBittorrent torrent {} omitted priority", hash))?,
        category,
        base_path: save_path.clone(),
        directory: save_path,
        creation_date: t
            .added_on
            .ok_or_else(|| anyhow::anyhow!("qBittorrent torrent {} omitted added_on", hash))?,
        timestamp_finished: t
            .completion_on
            .ok_or_else(|| anyhow::anyhow!("qBittorrent torrent {} omitted completion_on", hash))?,
        tracker_focus: 0,
        peers_connected: num_seeds.saturating_add(num_leechs),
        peers_complete: num_complete,
        message,
        tracker_url: t.tracker.unwrap_or_default(),
        tags: t.tags.unwrap_or_default(),
    })
}

fn map_tracker((idx, tracker): (usize, QbitTracker)) -> Result<RawTracker> {
    if tracker.url.trim().is_empty() {
        bail!("qBittorrent tracker {idx} returned an empty URL");
    }
    let status = tracker
        .status
        .ok_or_else(|| anyhow::anyhow!("qBittorrent tracker {idx} omitted status"))?;
    if !(0..=6).contains(&status) {
        bail!("qBittorrent tracker {idx} returned invalid status {status}");
    }
    Ok(RawTracker {
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
        scrape_incomplete: qbit_optional_nonnegative_i64(tracker.num_leeches, idx, "num_leeches")?,
        scrape_complete: qbit_optional_nonnegative_i64(tracker.num_seeds, idx, "num_seeds")?,
        scrape_downloaded: qbit_optional_nonnegative_i64(
            tracker.num_downloaded,
            idx,
            "num_downloaded",
        )?,
        message: tracker.msg.unwrap_or_default(),
    })
}

fn map_file((index, file): (usize, QbitFile)) -> Result<RawFile> {
    if file.name.trim().is_empty() {
        bail!("qBittorrent file {index} returned an empty name");
    }
    if file.size < 0 {
        bail!("qBittorrent file {index} returned negative size");
    }
    if !file.progress.is_finite() || !(0.0..=1.0).contains(&file.progress) {
        bail!("qBittorrent file {index} returned invalid progress");
    }
    let priority = match file.priority {
        0 => 0,
        1 => 1,
        6 | 7 => 2,
        other => bail!("qBittorrent file {index} returned invalid priority {other}"),
    };
    Ok(RawFile {
        index,
        path: file.name,
        size_bytes: file.size,
        size_chunks: file.size,
        completed_chunks: (file.size as f64 * file.progress).round() as i64,
        priority,
        is_created: true,
        is_open: true,
    })
}

fn qbit_nonnegative_i64(value: Option<i64>, field: &str) -> Result<i64> {
    let value = value.ok_or_else(|| anyhow::anyhow!("qBittorrent response omitted {field}"))?;
    if value < 0 {
        bail!("qBittorrent response contains negative {field}");
    }
    Ok(value)
}

fn qbit_optional_nonnegative_i64(value: Option<i64>, index: usize, field: &str) -> Result<i64> {
    match value {
        None => Ok(0),
        Some(value) if value >= 0 => Ok(value),
        Some(value) => bail!("qBittorrent tracker {index} returned negative {field}: {value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qbit_mutation_failures_in_http_200_bodies_are_rejected() {
        assert!(validate_qbit_mutation_body(b"", "api/v2/torrents/pause").is_ok());
        assert!(validate_qbit_mutation_body(b"Ok.\n", "api/v2/torrents/pause").is_ok());
        assert!(validate_qbit_mutation_body(b"Fails.", "api/v2/torrents/pause").is_err());
        assert!(validate_qbit_mutation_body(b"unexpected", "api/v2/torrents/pause").is_err());
    }

    #[test]
    fn parses_qbit_peer_response() {
        let response = serde_json::json!({
            "peers": {
                "127.0.0.1:6881": {
                    "client": "TorrentNG",
                    "progress": 0.5,
                    "dl_speed": 1024,
                    "up_speed": 512,
                    "downloaded": 4096,
                    "uploaded": 2048
                }
            }
        });

        let peers = parse_qbit_peer_response(&response).unwrap();

        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].addr, "127.0.0.1:6881".parse().unwrap());
        assert_eq!(peers[0].client, "TorrentNG");
        assert_eq!(peers[0].download_rate, 1024);
        assert_eq!(peers[0].upload_rate, 512);
    }

    #[test]
    fn bounded_sync_uses_qbittorrent_filters_and_safe_default() {
        assert_eq!(qbit_sync_filter("seeding"), "seeding");
        assert_eq!(qbit_sync_filter("missingFiles"), "missingFiles");
        assert_eq!(qbit_sync_filter("main"), "all");
        assert_eq!(qbit_sync_filter("unexpected"), "all");
    }

    #[test]
    fn qbit_piece_states_reject_unknown_values() {
        assert_eq!(map_qbit_piece_state(0).unwrap(), BackendPieceState::Missing);
        assert_eq!(map_qbit_piece_state(1).unwrap(), BackendPieceState::Partial);
        assert_eq!(
            map_qbit_piece_state(2).unwrap(),
            BackendPieceState::Complete
        );
        assert!(map_qbit_piece_state(3).is_err());
    }

    #[test]
    fn malformed_qbit_peer_snapshots_fail_closed() {
        assert!(parse_qbit_peer_response(&serde_json::json!({})).is_err());
        assert!(parse_qbit_peer_response(&serde_json::json!({
            "peers": { "bad": {} }
        }))
        .is_err());
        assert!(parse_qbit_peer_response(&serde_json::json!({
            "peers": { "127.0.0.1:6881": { "progress": 2.0 } }
        }))
        .is_err());
    }
}
