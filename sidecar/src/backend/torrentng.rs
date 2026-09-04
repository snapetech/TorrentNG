use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use reqwest::Url;
use serde_json::{json, Value};
use std::{collections::BTreeMap, net::SocketAddr};

use super::{
    map_qbit_piece_state, parse_qbit_peer_response, response_bytes_bounded, response_json_bounded,
    validate_qbit_mutation_body, BackendCapabilities, BackendPeer, BackendPieceState,
    BackendStatus, BackendTransferLimits, BackendType, QueueMove, TorrentBackend,
    MAX_BACKEND_JSON_BYTES,
};
use crate::{
    config::TorrentngConfig,
    rtorrent::{files::RawFile, torrents::RawTorrent, trackers::RawTracker, TransferRates},
};

const TORRENT_PAGE_SIZE: usize = 5_000;
const MAX_TORRENTS_FROM_API: usize = 10_000;

pub struct TorrentngBackend {
    client: reqwest::Client,
    base_url: Url,
    api_token: Option<String>,
}

impl TorrentngBackend {
    pub fn new(cfg: &TorrentngConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs.max(1)))
            .danger_accept_invalid_certs(cfg.accept_invalid_certs)
            .build()
            .context("create TorrentNG native API client")?;
        Ok(Self {
            client,
            base_url: Url::parse(cfg.url.trim()).context("parse torrentng.url")?,
            api_token: cfg.api_token.clone(),
        })
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .context("build TorrentNG native URL")
    }

    fn torrent_path(hash: &str, suffix: &str) -> String {
        let hash = urlencoding::encode(hash);
        if suffix.is_empty() {
            format!("api/v1/torrents/{hash}")
        } else {
            format!("api/v1/torrents/{hash}/{suffix}")
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let mut request = self.client.request(method, self.url(path)?);
        if let Some(token) = &self.api_token {
            request = request.bearer_auth(token);
        }
        Ok(request)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        response_json_bounded(
            self.request(reqwest::Method::GET, path)?
                .send()
                .await
                .with_context(|| format!("TorrentNG GET {path}"))?,
            MAX_BACKEND_JSON_BYTES,
            &format!("TorrentNG GET {path}"),
        )
        .await
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value> {
        self.send_json(reqwest::Method::POST, path, body).await
    }

    async fn patch_json(&self, path: &str, body: Value) -> Result<Value> {
        self.send_json(reqwest::Method::PATCH, path, body).await
    }

    async fn send_json(&self, method: reqwest::Method, path: &str, body: Value) -> Result<Value> {
        let bytes = response_bytes_bounded(
            self.request(method.clone(), path)?
                .json(&body)
                .send()
                .await
                .with_context(|| format!("TorrentNG {method} {path}"))?,
            MAX_BACKEND_JSON_BYTES,
            &format!("TorrentNG {method} {path}"),
        )
        .await?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).with_context(|| format!("decode TorrentNG {method} {path}"))
    }
}

#[async_trait]
impl TorrentBackend for TorrentngBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::Torrentng
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
            // force_start/auto-management are deliberately rejected by the
            // native engine until they have real scheduler semantics. Do not
            // advertise a mutation that only persists an inert flag.
            supports_mode_flags: false,
            supports_location_update: true,
            supports_torrent_rename: true,
            supports_file_rename: true,
            supports_runtime_user_agent: true,
            supports_config_overlay: false,
            supports_restart: false,
        }
    }

    async fn health(&self) -> BackendStatus {
        match self.get_json::<Value>("health").await {
            Ok(value) if value.get("ready").and_then(Value::as_bool) == Some(true) => {
                BackendStatus::Connected
            }
            Err(_) => BackendStatus::Unreachable,
            Ok(_) => BackendStatus::Unreachable,
        }
    }

    async fn transfer_rates(&self) -> Result<TransferRates> {
        let info: Value = self.get_json("api/v1/transfer/info").await?;
        Ok(TransferRates {
            download: required_nonnegative_i64(&info, "dl_info_speed")?,
            upload: required_nonnegative_i64(&info, "up_info_speed")?,
        })
    }

    async fn list_torrents(&self) -> Result<Vec<RawTorrent>> {
        let mut offset = 0usize;
        let mut snapshot = None::<u64>;
        let mut result = Vec::new();
        let mut expected_total = None::<usize>;
        loop {
            let path = match snapshot {
                Some(snapshot) => format!(
                    "api/v1/torrents?limit={TORRENT_PAGE_SIZE}&offset={offset}&snapshot={snapshot}"
                ),
                None => format!("api/v1/torrents?limit={TORRENT_PAGE_SIZE}&offset={offset}"),
            };
            let body: Value = self.get_json(&path).await?;
            let page = body
                .get("torrents")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("TorrentNG list response missing torrents array"))?;
            let page_snapshot = body
                .get("snapshot")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("TorrentNG list response missing snapshot"))?;
            if let Some(previous_snapshot) = snapshot {
                if previous_snapshot != page_snapshot {
                    return Err(anyhow::anyhow!(
                        "TorrentNG list response changed snapshot during pagination"
                    ));
                }
            } else {
                snapshot = Some(page_snapshot);
            }
            let total = body
                .get("total")
                .and_then(Value::as_u64)
                .and_then(|total| usize::try_from(total).ok())
                .ok_or_else(|| anyhow::anyhow!("TorrentNG list response missing total"))?;
            if total > MAX_TORRENTS_FROM_API {
                return Err(anyhow::anyhow!(
                    "TorrentNG list response exceeds {MAX_TORRENTS_FROM_API} torrents"
                ));
            }
            if expected_total
                .replace(total)
                .is_some_and(|previous| previous != total)
            {
                return Err(anyhow::anyhow!(
                    "TorrentNG list response changed total during pagination"
                ));
            }
            if page.is_empty() && result.len() < total {
                return Err(anyhow::anyhow!(
                    "TorrentNG list response stopped before total was reached"
                ));
            }
            result.extend(page.iter().map(map_summary).collect::<Result<Vec<_>>>()?);
            if result.len() >= total {
                result.truncate(total);
                return Ok(result);
            }
            let next_offset = offset.saturating_add(page.len());
            if next_offset <= offset {
                return Err(anyhow::anyhow!(
                    "TorrentNG list response made no pagination progress"
                ));
            }
            offset = next_offset;
        }
    }

    async fn has_bounded_sync(&self) -> bool {
        true
    }

    async fn list_torrents_range(
        &self,
        view: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<RawTorrent>> {
        self.list_torrents_range_with_snapshot(view, offset, limit, None)
            .await
            .map(|(torrents, _)| torrents)
    }

    async fn list_torrents_range_with_snapshot(
        &self,
        view: &str,
        offset: i64,
        limit: i64,
        snapshot: Option<u64>,
    ) -> Result<(Vec<RawTorrent>, Option<u64>)> {
        let offset = offset.max(0);
        let limit = usize::try_from(limit)
            .unwrap_or(1)
            .clamp(1, TORRENT_PAGE_SIZE);
        let mut path = match snapshot {
            Some(snapshot) => {
                format!("api/v1/torrents?limit={limit}&offset={offset}&snapshot={snapshot}")
            }
            None => format!("api/v1/torrents?limit={limit}&offset={offset}"),
        };
        if let Some(status) = native_status_for_view(view)? {
            path.push_str("&status=");
            path.push_str(urlencoding::encode(&status).as_ref());
        }
        let body: Value = self.get_json(&path).await?;
        let torrents = body
            .get("torrents")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("TorrentNG range response missing torrents array"))?;
        if torrents.len() > limit {
            bail!("TorrentNG range response exceeded requested page size {limit}");
        }
        let page_snapshot = body
            .get("snapshot")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("TorrentNG range response missing snapshot"))?;
        if snapshot.is_some_and(|expected| expected != page_snapshot) {
            bail!("TorrentNG range response changed snapshot during pagination");
        }
        Ok((
            torrents
                .iter()
                .map(map_summary)
                .collect::<Result<Vec<_>>>()?,
            Some(page_snapshot),
        ))
    }

    async fn live_summary(&self, _view: &str, _limit: i64) -> Result<super::LiveSummary> {
        Ok(super::LiveSummary {
            rates: self.transfer_rates().await?,
            moving: Vec::new(),
        })
    }

    async fn add_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        self.post_json(
            "api/v1/torrents",
            json!({
                "magnet": magnet,
                "save_path": save_path,
                "category": empty_to_null(category),
                "tags": [],
                "start": start,
            }),
        )
        .await?;
        Ok(())
    }

    async fn add_torrent(
        &self,
        data: &[u8],
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        self.post_json(
            "api/v1/torrents",
            json!({
                "torrent_b64": general_purpose::STANDARD.encode(data),
                "save_path": save_path,
                "category": empty_to_null(category),
                "tags": [],
                "start": start,
            }),
        )
        .await?;
        Ok(())
    }

    async fn torrent_blob(&self, hash: &str) -> Result<Vec<u8>> {
        response_bytes_bounded(
            self.request(
                reqwest::Method::GET,
                &format!(
                    "api/qb/v2/torrents/export?hash={}",
                    urlencoding::encode(hash)
                ),
            )?
            .send()
            .await
            .with_context(|| format!("TorrentNG GET torrent export {hash}"))?,
            MAX_BACKEND_JSON_BYTES,
            &format!("TorrentNG GET torrent export {hash}"),
        )
        .await
    }

    async fn remove(&self, hash: &str, delete_data: bool) -> Result<()> {
        let path = if delete_data {
            format!("{}?delete_files=true", Self::torrent_path(hash, ""))
        } else {
            Self::torrent_path(hash, "")
        };
        self.request(reqwest::Method::DELETE, &path)?
            .send()
            .await
            .with_context(|| format!("TorrentNG DELETE torrent {hash}"))?
            .error_for_status()
            .with_context(|| format!("TorrentNG DELETE torrent {hash}"))?;
        Ok(())
    }

    async fn start(&self, hash: &str) -> Result<()> {
        self.post_json(&Self::torrent_path(hash, "start"), json!({}))
            .await?;
        Ok(())
    }

    async fn stop(&self, hash: &str) -> Result<()> {
        self.post_json(&Self::torrent_path(hash, "stop"), json!({}))
            .await?;
        Ok(())
    }

    async fn recheck(&self, hash: &str) -> Result<()> {
        self.post_json(&Self::torrent_path(hash, "recheck"), json!({}))
            .await?;
        Ok(())
    }

    async fn reannounce(&self, hash: &str) -> Result<()> {
        self.post_json(&Self::torrent_path(hash, "reannounce"), json!({}))
            .await?;
        Ok(())
    }

    async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>> {
        let trackers: Vec<Value> = self.get_json(&Self::torrent_path(hash, "trackers")).await?;
        let mut out = Vec::with_capacity(trackers.len());
        for (index, tracker) in trackers.iter().enumerate() {
            let url = tracker
                .as_str()
                .filter(|url| !url.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("TorrentNG tracker response contains an invalid URL")
                })?
                .to_owned();
            let id = i64::try_from(index).context("TorrentNG tracker index exceeds i64")?;
            out.push(RawTracker {
                url,
                id,
                group: 0,
                group_index: id,
                is_enabled: true,
                is_open: false,
                is_extra_tracker: false,
                activity_time_last: 0,
                activity_time_next: 0,
                min_interval: 0,
                normal_interval: 0,
                failed_counter: 0,
                success_counter: 0,
                scrape_incomplete: 0,
                scrape_complete: 0,
                scrape_downloaded: 0,
                message: String::new(),
            });
        }
        Ok(out)
    }

    async fn add_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.patch_json(
            &Self::torrent_path(hash, "trackers"),
            json!({ "add": [url] }),
        )
        .await?;
        Ok(())
    }

    async fn edit_tracker(&self, hash: &str, original_url: &str, new_url: &str) -> Result<()> {
        self.patch_json(
            &Self::torrent_path(hash, "trackers"),
            json!({ "edit": [{ "orig_url": original_url, "new_url": new_url }] }),
        )
        .await?;
        Ok(())
    }

    async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.patch_json(
            &Self::torrent_path(hash, "trackers"),
            json!({ "remove": [url] }),
        )
        .await?;
        Ok(())
    }

    async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>> {
        let files: Vec<Value> = self.get_json(&Self::torrent_path(hash, "files")).await?;
        let mut out = Vec::with_capacity(files.len());
        for file in &files {
            let index = required_nonnegative_usize(file, "file_index")?;
            let path = required_string(file, "path")?;
            let length = required_nonnegative_i64(file, "length")?;
            let priority = required_i64(file, "priority")?;
            if !(0..=2).contains(&priority) {
                bail!("TorrentNG file {index} returned invalid priority {priority}");
            }
            out.push(RawFile {
                index,
                path,
                size_bytes: length,
                size_chunks: length,
                completed_chunks: 0,
                priority,
                is_created: true,
                is_open: true,
            });
        }
        Ok(out)
    }

    async fn list_webseeds(&self, hash: &str) -> Result<Vec<String>> {
        self.get_json(&format!(
            "api/qb/v2/torrents/webseeds?hash={}",
            urlencoding::encode(hash)
        ))
        .await
    }

    async fn piece_states(&self, hash: &str) -> Result<Vec<BackendPieceState>> {
        let states: Vec<i64> = self
            .get_json(&format!(
                "api/qb/v2/torrents/pieceStates?hash={}",
                urlencoding::encode(hash)
            ))
            .await?;
        states.into_iter().map(map_qbit_piece_state).collect()
    }

    async fn piece_hashes(&self, hash: &str) -> Result<Vec<String>> {
        self.get_json(&format!(
            "api/qb/v2/torrents/pieceHashes?hash={}",
            urlencoding::encode(hash)
        ))
        .await
    }

    async fn list_peers(&self, hash: &str) -> Result<Vec<BackendPeer>> {
        let response: Value = self
            .get_json(&format!(
                "api/qb/v2/sync/torrentPeers?hash={}",
                urlencoding::encode(hash)
            ))
            .await?;
        parse_qbit_peer_response(&response)
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()> {
        if !(0..=2).contains(&priority) {
            bail!("TorrentNG file priority must be between 0 and 2");
        }
        self.patch_json(
            &Self::torrent_path(hash, "files"),
            json!({ "files": [{ "index": file_index, "priority": priority }] }),
        )
        .await?;
        Ok(())
    }

    async fn set_category(&self, hash: &str, category: &str) -> Result<()> {
        self.request(reqwest::Method::PUT, &Self::torrent_path(hash, "category"))?
            .json(&json!({ "category": empty_to_null(category) }))
            .send()
            .await
            .with_context(|| format!("TorrentNG PUT torrent {hash} category"))?
            .error_for_status()
            .with_context(|| format!("TorrentNG PUT torrent {hash} category"))?;
        Ok(())
    }

    async fn set_location(&self, hash: &str, location: &str) -> Result<()> {
        self.request(reqwest::Method::PUT, &Self::torrent_path(hash, ""))?
            .json(&json!({ "save_path": location }))
            .send()
            .await
            .with_context(|| format!("TorrentNG PUT torrent {hash} location"))?
            .error_for_status()
            .with_context(|| format!("TorrentNG PUT torrent {hash} location"))?;
        Ok(())
    }

    async fn rename_torrent(&self, hash: &str, name: &str) -> Result<()> {
        self.request(reqwest::Method::PUT, &Self::torrent_path(hash, ""))?
            .json(&json!({ "name": name }))
            .send()
            .await
            .with_context(|| format!("TorrentNG PUT torrent {hash} name"))?
            .error_for_status()
            .with_context(|| format!("TorrentNG PUT torrent {hash} name"))?;
        Ok(())
    }

    async fn rename_file(&self, hash: &str, file_index: usize, name: &str) -> Result<()> {
        self.patch_json(
            &Self::torrent_path(hash, "files"),
            json!({ "files": [{ "index": file_index, "path": name }] }),
        )
        .await?;
        Ok(())
    }

    async fn set_share_limits(
        &self,
        hash: &str,
        ratio_limit_milli: i64,
        seeding_time_limit: i64,
    ) -> Result<()> {
        self.put_limits(
            hash,
            json!({
                "seed_ratio_limit": if ratio_limit_milli >= 0 {
                    json!(ratio_limit_milli as f64 / 1000.0)
                } else {
                    Value::Null
                },
                "seed_idle_limit": if seeding_time_limit >= 0 {
                    json!(seeding_time_limit)
                } else {
                    Value::Null
                },
            }),
        )
        .await
    }

    async fn set_download_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        self.put_limits(
            hash,
            json!({
                "download_limit": limit.filter(|value| *value > 0).map_or(Value::Null, Value::from),
            }),
        )
        .await
    }

    async fn set_upload_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        self.put_limits(
            hash,
            json!({
                "upload_limit": limit.filter(|value| *value > 0).map_or(Value::Null, Value::from),
            }),
        )
        .await
    }

    async fn download_limits(&self, hashes: &[String]) -> Result<BTreeMap<String, i64>> {
        self.torrent_limit_map(hashes, "download_limit").await
    }

    async fn upload_limits(&self, hashes: &[String]) -> Result<BTreeMap<String, i64>> {
        self.torrent_limit_map(hashes, "upload_limit").await
    }

    async fn set_global_download_limit(&self, limit: i64) -> Result<()> {
        self.put_transfer_limits(json!({ "download_limit": limit.max(0) }))
            .await
    }

    async fn set_global_upload_limit(&self, limit: i64) -> Result<()> {
        self.put_transfer_limits(json!({ "upload_limit": limit.max(0) }))
            .await
    }

    async fn global_limits(&self) -> Result<BackendTransferLimits> {
        let limits: Value = self.get_json("api/v1/transfer/limits").await?;
        Ok(BackendTransferLimits {
            download_limit: required_nonnegative_i64(&limits, "download_limit")?,
            upload_limit: required_nonnegative_i64(&limits, "upload_limit")?,
            speed_limits_mode: limits
                .get("speed_limits_mode")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    anyhow::anyhow!("TorrentNG response omitted valid speed_limits_mode")
                })?,
        })
    }

    async fn feature_status(&self) -> (String, String) {
        match self.get_json::<Value>("api/v1/session/features").await {
            Ok(features) => (
                bool_status(features.get("dht").and_then(Value::as_bool)),
                bool_status(features.get("pex").and_then(Value::as_bool)),
            ),
            Err(_) => ("unknown".to_owned(), "unknown".to_owned()),
        }
    }

    async fn set_dht(&self, enabled: bool) -> Result<()> {
        self.send_json(
            reqwest::Method::PUT,
            "api/v1/session/features",
            json!({ "dht": enabled }),
        )
        .await
        .map(|_| ())
    }

    async fn set_pex(&self, enabled: bool) -> Result<()> {
        self.send_json(
            reqwest::Method::PUT,
            "api/v1/session/features",
            json!({ "pex": enabled }),
        )
        .await
        .map(|_| ())
    }

    async fn toggle_global_speed_limits_mode(&self) -> Result<()> {
        let limits = self.global_limits().await?;
        self.put_transfer_limits(json!({ "speed_limits_mode": !limits.speed_limits_mode }))
            .await
    }

    async fn toggle_sequential_download(&self, hash: &str) -> Result<()> {
        let limits: Value = self.get_json(&Self::torrent_path(hash, "limits")).await?;
        let current = limits
            .get("sequential_download")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                anyhow::anyhow!("TorrentNG response omitted valid sequential_download")
            })?;
        let next = !current;
        self.put_limits(hash, json!({ "sequential_download": next }))
            .await
    }

    async fn toggle_first_last_piece_priority(&self, hash: &str) -> Result<()> {
        let limits: Value = self.get_json(&Self::torrent_path(hash, "limits")).await?;
        let current = limits
            .get("first_last_piece_prio")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                anyhow::anyhow!("TorrentNG response omitted valid first_last_piece_prio")
            })?;
        let next = !current;
        self.put_limits(hash, json!({ "first_last_piece_prio": next }))
            .await
    }

    async fn set_force_start(&self, hash: &str, enabled: bool) -> Result<()> {
        self.put_limits(hash, json!({ "force_start": enabled }))
            .await
    }

    async fn set_super_seeding(&self, hash: &str, enabled: bool) -> Result<()> {
        self.put_limits(hash, json!({ "super_seeding": enabled }))
            .await
    }

    async fn set_auto_tmm(&self, hash: &str, enabled: bool) -> Result<()> {
        self.put_limits(hash, json!({ "auto_tmm": enabled })).await
    }

    async fn set_auto_management(&self, hash: &str, enabled: bool) -> Result<()> {
        self.put_limits(hash, json!({ "auto_management": enabled }))
            .await
    }

    async fn add_peers(&self, hash: &str, peers: &[SocketAddr]) -> Result<()> {
        self.post_json(
            &Self::torrent_path(hash, "peers"),
            json!({ "peers": peers.iter().map(ToString::to_string).collect::<Vec<_>>() }),
        )
        .await?;
        Ok(())
    }

    async fn update_queue_order(&self, hashes: &[String], queue_move: QueueMove) -> Result<()> {
        let queue_move = match queue_move {
            QueueMove::Up => "up",
            QueueMove::Down => "down",
            QueueMove::Top => "top",
            QueueMove::Bottom => "bottom",
        };
        self.post_json(
            "api/v1/torrents/queue",
            json!({ "hashes": hashes, "move": queue_move }),
        )
        .await?;
        Ok(())
    }

    async fn ban_peers(&self, peers: &[SocketAddr]) -> Result<()> {
        let peers = peers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("|");
        let response = self
            .request(reqwest::Method::POST, "api/qb/v2/transfer/banPeers")?
            .form(&[("peers", peers)])
            .send()
            .await
            .context("TorrentNG POST qBit banPeers")?;
        let body =
            response_bytes_bounded(response, 16 * 1024, "TorrentNG POST qBit banPeers").await?;
        validate_qbit_mutation_body(&body, "TorrentNG POST qBit banPeers")?;
        Ok(())
    }

    async fn add_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        self.patch_json(
            &Self::torrent_path(hash, "tags"),
            json!({ "add": tags, "remove": [] }),
        )
        .await?;
        Ok(())
    }

    async fn remove_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        self.patch_json(
            &Self::torrent_path(hash, "tags"),
            json!({ "add": [], "remove": tags }),
        )
        .await?;
        Ok(())
    }

    async fn set_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        let torrent: Value = self
            .get_json(&Self::torrent_path(hash, ""))
            .await
            .with_context(|| format!("read TorrentNG torrent tags for {hash}"))?;
        if torrent.is_null() {
            bail!("TorrentNG torrent not found");
        }
        let current_tags = torrent
            .get("tags")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("TorrentNG torrent response omitted tags array"))?;
        let current_tags = current_tags
            .iter()
            .map(|tag| {
                tag.as_str().ok_or_else(|| {
                    anyhow::anyhow!("TorrentNG torrent response contains a non-string tag")
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.patch_json(
            &Self::torrent_path(hash, "tags"),
            json!({ "add": tags, "remove": current_tags }),
        )
        .await?;
        Ok(())
    }

    async fn get_user_agent(&self) -> Result<String> {
        let response: Value = self.get_json("api/v1/engine/rtorrent-settings").await?;
        response
            .get("settings")
            .and_then(|settings| settings.get("system.user_agent"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("TorrentNG response omitted system.user_agent"))
    }

    async fn set_user_agent(&self, user_agent: &str) -> Result<()> {
        self.send_json(
            reqwest::Method::PUT,
            "api/v1/engine/rtorrent-settings",
            json!({ "settings": { "system.user_agent": user_agent } }),
        )
        .await?;
        Ok(())
    }
}

impl TorrentngBackend {
    async fn put_limits(&self, hash: &str, body: Value) -> Result<()> {
        self.request(reqwest::Method::PUT, &Self::torrent_path(hash, "limits"))?
            .json(&body)
            .send()
            .await
            .with_context(|| format!("TorrentNG PUT torrent {hash} limits"))?
            .error_for_status()
            .with_context(|| format!("TorrentNG PUT torrent {hash} limits"))?;
        Ok(())
    }

    async fn put_transfer_limits(&self, body: Value) -> Result<()> {
        self.request(reqwest::Method::PUT, "api/v1/transfer/limits")?
            .json(&body)
            .send()
            .await
            .context("TorrentNG PUT transfer limits")?
            .error_for_status()
            .context("TorrentNG PUT transfer limits")?;
        Ok(())
    }

    async fn torrent_limit_map(
        &self,
        hashes: &[String],
        field: &str,
    ) -> Result<BTreeMap<String, i64>> {
        let mut out = BTreeMap::new();
        for hash in hashes {
            let limits: Value = self.get_json(&Self::torrent_path(hash, "limits")).await?;
            let value = match limits.get(field) {
                Some(Value::Null) => 0,
                Some(_) => required_nonnegative_i64(&limits, field)?,
                None => {
                    bail!("TorrentNG response omitted valid {field}");
                }
            };
            out.insert(hash.clone(), value);
        }
        Ok(out)
    }
}

fn map_summary(t: &Value) -> Result<RawTorrent> {
    let state = required_string(t, "state")?;
    let size = required_nonnegative_i64(t, "total_length")?;
    let downloaded = required_nonnegative_i64(t, "downloaded")?;
    if downloaded > size {
        bail!("TorrentNG torrent reports {downloaded} downloaded bytes for size {size}");
    }
    let uploaded = required_nonnegative_i64(t, "uploaded")?;
    let ratio = t
        .get("ratio")
        .and_then(Value::as_f64)
        .filter(|ratio| ratio.is_finite() && *ratio >= 0.0)
        .ok_or_else(|| anyhow::anyhow!("TorrentNG response omitted valid ratio"))?;
    let category = match t.get("category") {
        None | Some(Value::Null) => String::new(),
        Some(value) => value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("TorrentNG response contains invalid category"))?
            .to_owned(),
    };
    let tags = t
        .get("tags")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("TorrentNG response omitted tags array"))?
        .iter()
        .map(|tag| {
            tag.as_str()
                .ok_or_else(|| anyhow::anyhow!("TorrentNG response contains an invalid tag"))
        })
        .collect::<Result<Vec<_>>>()?
        .join(",");
    let peers_connected = u32::try_from(required_nonnegative_i64(t, "num_peers")?)
        .context("TorrentNG response num_peers exceeds u32")?;
    let peers_complete = u32::try_from(required_nonnegative_i64(t, "num_seeds")?)
        .context("TorrentNG response num_seeds exceeds u32")?;
    let completed_at = match t.get("completed_at") {
        None | Some(Value::Null) => 0,
        Some(value) => required_nonnegative_i64_value(value, "completed_at")?,
    };
    let message = match t.get("tracker_message") {
        None | Some(Value::Null) => String::new(),
        Some(value) => value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("TorrentNG response contains invalid tracker_message"))?
            .to_owned(),
    };
    let hash = required_string(t, "info_hash")?;
    let name = required_string(t, "name")?;
    let save_path = required_string(t, "save_path")?;
    let creation_date = required_nonnegative_i64(t, "added_at")?;
    Ok(RawTorrent {
        hash,
        name,
        size_bytes: size,
        bytes_done: downloaded,
        down_rate: 0,
        up_rate: 0,
        up_total: uploaded,
        down_total: downloaded,
        ratio: super::ratio_milli(Some(ratio)),
        is_active: !matches!(state.as_str(), "paused" | "stopped" | "error"),
        is_open: !matches!(state.as_str(), "paused" | "stopped" | "error"),
        complete: matches!(state.as_str(), "seeding" | "complete") || downloaded >= size,
        state: if state == "error" { 3 } else { 1 },
        priority: 0,
        category,
        base_path: save_path.clone(),
        directory: save_path,
        creation_date,
        timestamp_finished: completed_at,
        tracker_focus: 0,
        peers_connected: i64::from(peers_connected),
        peers_complete: i64::from(peers_complete),
        message,
        tracker_url: String::new(),
        tags,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("TorrentNG response omitted valid {field}"))
}

fn required_i64(value: &Value, field: &str) -> Result<i64> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("TorrentNG response omitted valid {field}"))
}

fn required_nonnegative_i64_value(value: &Value, field: &str) -> Result<i64> {
    let number = value
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("TorrentNG response omitted valid {field}"))?;
    if number < 0 {
        bail!("TorrentNG response contains negative {field}");
    }
    Ok(number)
}

fn required_nonnegative_i64(value: &Value, field: &str) -> Result<i64> {
    let number = required_i64(value, field)?;
    if number < 0 {
        bail!("TorrentNG response contains negative {field}");
    }
    Ok(number)
}

fn required_nonnegative_usize(value: &Value, field: &str) -> Result<usize> {
    usize::try_from(required_nonnegative_i64(value, field)?)
        .with_context(|| format!("TorrentNG response {field} exceeds usize"))
}

fn native_status_for_view(view: &str) -> Result<Option<String>> {
    let view = view.trim();
    if view.is_empty() || view.eq_ignore_ascii_case("main") || view.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let normalized = if view.eq_ignore_ascii_case("complete") {
        "completed"
    } else if view.eq_ignore_ascii_case("errored") {
        "error"
    } else {
        view
    };
    if matches!(
        normalized,
        "active"
            | "completed"
            | "stopped"
            | "checking"
            | "downloading"
            | "error"
            | "metadata_pending"
            | "paused"
            | "queued"
            | "seeding"
    ) {
        Ok(Some(normalized.to_owned()))
    } else {
        bail!("TorrentNG native backend does not support torrent view {view}")
    }
}

fn empty_to_null(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

fn bool_status(value: Option<bool>) -> String {
    match value {
        Some(true) => "enabled".to_owned(),
        Some(false) => "disabled".to_owned(),
        None => "unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_summary_maps_core_fields() {
        let raw = json!({
            "info_hash": "abc",
            "name": "ubuntu.iso",
            "state": "seeding",
            "total_length": 1024,
            "downloaded": 1024,
            "uploaded": 2048,
            "ratio": 2.0,
            "save_path": "/data",
            "category": "linux",
            "tags": [],
            "added_at": 10,
            "completed_at": 20,
            "num_peers": 3,
            "num_seeds": 4
        });

        let mapped = map_summary(&raw).unwrap();

        assert_eq!(mapped.hash, "abc");
        assert_eq!(mapped.name, "ubuntu.iso");
        assert_eq!(mapped.size_bytes, 1024);
        assert_eq!(mapped.bytes_done, 1024);
        assert_eq!(mapped.up_total, 2048);
        assert_eq!(mapped.ratio, 2000);
        assert!(mapped.complete);
        assert_eq!(mapped.category, "linux");
        assert_eq!(mapped.peers_connected, 3);
        assert_eq!(mapped.peers_complete, 4);
    }

    #[test]
    fn torrent_paths_escape_untrusted_hash_segments() {
        assert_eq!(
            TorrentngBackend::torrent_path("../private/file", "start"),
            "api/v1/torrents/..%2Fprivate%2Ffile/start"
        );
    }

    #[test]
    fn torrentng_backend_capabilities_match_native_mutation_routes() {
        let backend = TorrentngBackend::new(&TorrentngConfig::default()).unwrap();

        let capabilities = backend.capabilities();

        assert!(capabilities.supports_categories);
        assert!(capabilities.supports_file_priority);
        assert!(capabilities.supports_tracker_edit);
        assert!(capabilities.supports_recheck);
        assert!(capabilities.supports_torrent_export);
        assert!(capabilities.supports_webseed_reads);
        assert!(capabilities.supports_piece_state_reads);
        assert!(capabilities.supports_piece_hash_reads);
        assert!(capabilities.supports_peer_snapshots);
        assert!(capabilities.supports_peer_add);
        assert!(capabilities.supports_peer_ban);
        assert!(capabilities.supports_queue_order);
        assert!(capabilities.supports_per_torrent_limits);
        assert!(capabilities.supports_global_limits);
        assert!(capabilities.supports_share_limits);
        assert!(!capabilities.supports_mode_flags);
        assert!(capabilities.supports_location_update);
        assert!(capabilities.supports_torrent_rename);
        assert!(capabilities.supports_file_rename);
        assert!(capabilities.supports_runtime_user_agent);
    }
}
