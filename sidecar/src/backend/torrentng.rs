use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use reqwest::Url;
use serde_json::{json, Value};
use std::{collections::BTreeMap, net::SocketAddr};

use super::{
    BackendCapabilities, BackendPeer, BackendPieceState, BackendStatus, BackendTransferLimits,
    BackendType, QueueMove, TorrentBackend,
};
use crate::{
    config::TorrentngConfig,
    rtorrent::{files::RawFile, torrents::RawTorrent, trackers::RawTracker, TransferRates},
};

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

    fn request(&self, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let mut request = self.client.request(method, self.url(path)?);
        if let Some(token) = &self.api_token {
            request = request.bearer_auth(token);
        }
        Ok(request)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(self
            .request(reqwest::Method::GET, path)?
            .send()
            .await
            .with_context(|| format!("TorrentNG GET {path}"))?
            .error_for_status()
            .with_context(|| format!("TorrentNG GET {path}"))?
            .json()
            .await
            .with_context(|| format!("decode TorrentNG GET {path}"))?)
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value> {
        self.send_json(reqwest::Method::POST, path, body).await
    }

    async fn patch_json(&self, path: &str, body: Value) -> Result<Value> {
        self.send_json(reqwest::Method::PATCH, path, body).await
    }

    async fn send_json(&self, method: reqwest::Method, path: &str, body: Value) -> Result<Value> {
        Ok(self
            .request(method.clone(), path)?
            .json(&body)
            .send()
            .await
            .with_context(|| format!("TorrentNG {method} {path}"))?
            .error_for_status()
            .with_context(|| format!("TorrentNG {method} {path}"))?
            .json()
            .await
            .unwrap_or(Value::Null))
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
            supports_runtime_user_agent: false,
            supports_config_overlay: false,
            supports_restart: false,
        }
    }

    async fn health(&self) -> BackendStatus {
        match self.get_json::<Value>("health").await {
            Ok(_) => BackendStatus::Connected,
            Err(_) => BackendStatus::Unreachable,
        }
    }

    async fn transfer_rates(&self) -> Result<TransferRates> {
        Ok(TransferRates::default())
    }

    async fn list_torrents(&self) -> Result<Vec<RawTorrent>> {
        let torrents: Vec<Value> = self.get_json("api/v1/torrents").await?;
        Ok(torrents.iter().map(map_summary).collect())
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

    async fn remove(&self, hash: &str, delete_data: bool) -> Result<()> {
        let path = if delete_data {
            format!("api/v1/torrents/{hash}?delete_files=true")
        } else {
            format!("api/v1/torrents/{hash}")
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
        self.post_json(&format!("api/v1/torrents/{hash}/start"), json!({}))
            .await?;
        Ok(())
    }

    async fn stop(&self, hash: &str) -> Result<()> {
        self.post_json(&format!("api/v1/torrents/{hash}/stop"), json!({}))
            .await?;
        Ok(())
    }

    async fn recheck(&self, hash: &str) -> Result<()> {
        self.post_json(&format!("api/v1/torrents/{hash}/recheck"), json!({}))
            .await?;
        Ok(())
    }

    async fn reannounce(&self, hash: &str) -> Result<()> {
        self.post_json(&format!("api/v1/torrents/{hash}/reannounce"), json!({}))
            .await?;
        Ok(())
    }

    async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>> {
        let trackers: Vec<Value> = self
            .get_json(&format!("api/v1/torrents/{hash}/trackers"))
            .await?;
        Ok(trackers
            .iter()
            .enumerate()
            .map(|(index, tracker)| RawTracker {
                url: tracker.as_str().unwrap_or_default().to_owned(),
                id: index as i64,
                group: 0,
                group_index: index as i64,
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
            })
            .collect())
    }

    async fn add_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.patch_json(
            &format!("api/v1/torrents/{hash}/trackers"),
            json!({ "add": [url] }),
        )
        .await?;
        Ok(())
    }

    async fn edit_tracker(&self, hash: &str, original_url: &str, new_url: &str) -> Result<()> {
        self.patch_json(
            &format!("api/v1/torrents/{hash}/trackers"),
            json!({ "edit": [{ "orig_url": original_url, "new_url": new_url }] }),
        )
        .await?;
        Ok(())
    }

    async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.patch_json(
            &format!("api/v1/torrents/{hash}/trackers"),
            json!({ "remove": [url] }),
        )
        .await?;
        Ok(())
    }

    async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>> {
        let files: Vec<Value> = self
            .get_json(&format!("api/v1/torrents/{hash}/files"))
            .await?;
        Ok(files
            .iter()
            .map(|file| RawFile {
                index: int(file, "file_index") as usize,
                path: string(file, "path"),
                size_bytes: int(file, "length"),
                size_chunks: int(file, "length"),
                completed_chunks: 0,
                priority: int(file, "priority"),
                is_created: true,
                is_open: true,
            })
            .collect())
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
        Ok(states
            .into_iter()
            .map(|state| match state {
                2 => BackendPieceState::Complete,
                1 => BackendPieceState::Partial,
                _ => BackendPieceState::Missing,
            })
            .collect())
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
        Ok(parse_qbit_peer_response(&response))
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()> {
        self.patch_json(
            &format!("api/v1/torrents/{hash}/files"),
            json!({ "files": [{ "index": file_index, "priority": priority }] }),
        )
        .await?;
        Ok(())
    }

    async fn set_category(&self, hash: &str, category: &str) -> Result<()> {
        self.request(
            reqwest::Method::PUT,
            &format!("api/v1/torrents/{hash}/category"),
        )?
        .json(&json!({ "category": empty_to_null(category) }))
        .send()
        .await
        .with_context(|| format!("TorrentNG PUT torrent {hash} category"))?
        .error_for_status()
        .with_context(|| format!("TorrentNG PUT torrent {hash} category"))?;
        Ok(())
    }

    async fn set_location(&self, hash: &str, location: &str) -> Result<()> {
        self.request(reqwest::Method::PUT, &format!("api/v1/torrents/{hash}"))?
            .json(&json!({ "save_path": location }))
            .send()
            .await
            .with_context(|| format!("TorrentNG PUT torrent {hash} location"))?
            .error_for_status()
            .with_context(|| format!("TorrentNG PUT torrent {hash} location"))?;
        Ok(())
    }

    async fn rename_torrent(&self, hash: &str, name: &str) -> Result<()> {
        self.request(reqwest::Method::PUT, &format!("api/v1/torrents/{hash}"))?
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
            &format!("api/v1/torrents/{hash}/files"),
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
            download_limit: limits
                .get("download_limit")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0),
            upload_limit: limits
                .get("upload_limit")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0),
            speed_limits_mode: limits
                .get("speed_limits_mode")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    async fn toggle_global_speed_limits_mode(&self) -> Result<()> {
        let limits = self.global_limits().await?;
        self.put_transfer_limits(json!({ "speed_limits_mode": !limits.speed_limits_mode }))
            .await
    }

    async fn toggle_sequential_download(&self, hash: &str) -> Result<()> {
        let limits: Value = self
            .get_json(&format!("api/v1/torrents/{hash}/limits"))
            .await?;
        let next = !limits
            .get("sequential_download")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.put_limits(hash, json!({ "sequential_download": next }))
            .await
    }

    async fn toggle_first_last_piece_priority(&self, hash: &str) -> Result<()> {
        let limits: Value = self
            .get_json(&format!("api/v1/torrents/{hash}/limits"))
            .await?;
        let next = !limits
            .get("first_last_piece_prio")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.put_limits(hash, json!({ "first_last_piece_prio": next }))
            .await
    }

    async fn set_force_start(&self, hash: &str, enabled: bool) -> Result<()> {
        self.put_limits(hash, json!({ "force_start": enabled })).await
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
            &format!("api/v1/torrents/{hash}/peers"),
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
        self.request(reqwest::Method::POST, "api/qb/v2/transfer/banPeers")?
            .form(&[("peers", peers)])
            .send()
            .await
            .context("TorrentNG POST qBit banPeers")?
            .error_for_status()
            .context("TorrentNG POST qBit banPeers")?;
        Ok(())
    }

    async fn add_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        self.patch_json(
            &format!("api/v1/torrents/{hash}/tags"),
            json!({ "add": tags, "remove": [] }),
        )
        .await?;
        Ok(())
    }

    async fn remove_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        self.patch_json(
            &format!("api/v1/torrents/{hash}/tags"),
            json!({ "add": [], "remove": tags }),
        )
        .await?;
        Ok(())
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
        self.patch_json(
            &format!("api/v1/torrents/{hash}/tags"),
            json!({ "add": tags, "remove": current_tags }),
        )
        .await?;
        Ok(())
    }
}

impl TorrentngBackend {
    async fn put_limits(&self, hash: &str, body: Value) -> Result<()> {
        self.request(
            reqwest::Method::PUT,
            &format!("api/v1/torrents/{hash}/limits"),
        )?
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
            let limits: Value = self
                .get_json(&format!("api/v1/torrents/{hash}/limits"))
                .await?;
            out.insert(
                hash.clone(),
                limits
                    .get(field)
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .max(0),
            );
        }
        Ok(out)
    }
}

fn map_summary(t: &Value) -> RawTorrent {
    let state = string(t, "state");
    let size = int(t, "total_length");
    let downloaded = int(t, "downloaded");
    RawTorrent {
        hash: string(t, "info_hash"),
        name: string(t, "name"),
        size_bytes: size,
        bytes_done: downloaded,
        down_rate: 0,
        up_rate: 0,
        up_total: int(t, "uploaded"),
        down_total: downloaded,
        ratio: (t.get("ratio").and_then(Value::as_f64).unwrap_or(0.0) * 1000.0) as i64,
        is_active: !matches!(state.as_str(), "paused" | "stopped" | "error"),
        is_open: !matches!(state.as_str(), "paused" | "stopped" | "error"),
        complete: matches!(state.as_str(), "seeding" | "complete") || downloaded >= size,
        state: if state == "error" { 3 } else { 1 },
        priority: 0,
        category: t
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        base_path: string(t, "save_path"),
        directory: string(t, "save_path"),
        creation_date: int(t, "added_at"),
        timestamp_finished: t.get("completed_at").and_then(Value::as_i64).unwrap_or(0),
        tracker_focus: 0,
        peers_connected: int(t, "num_peers"),
        peers_complete: int(t, "num_seeds"),
        message: String::new(),
        tracker_url: String::new(),
        tags: t
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default(),
    }
}

fn empty_to_null(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn int(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .or_else(|| {
            value
                .get(key)
                .and_then(Value::as_u64)
                .map(|value| value as i64)
        })
        .unwrap_or(0)
}

fn parse_qbit_peer_response(response: &Value) -> Vec<BackendPeer> {
    response
        .get("peers")
        .and_then(Value::as_object)
        .map(|peers| {
            peers
                .iter()
                .filter_map(|(key, peer)| parse_qbit_peer(key, peer))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_qbit_peer(key: &str, peer: &Value) -> Option<BackendPeer> {
    let addr = if let Some(ip) = peer.get("ip").and_then(Value::as_str) {
        let port = peer.get("port").and_then(Value::as_u64).unwrap_or(0);
        format!("{ip}:{port}").parse().ok()?
    } else {
        key.parse().ok()?
    };
    Some(BackendPeer {
        addr,
        client: peer
            .get("client")
            .or_else(|| peer.get("peer_id_client"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        progress: peer
            .get("progress")
            .or_else(|| peer.get("relevance"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        download_rate: int(peer, "dl_speed"),
        upload_rate: int(peer, "up_speed"),
        downloaded: int(peer, "downloaded"),
        uploaded: int(peer, "uploaded"),
    })
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
            "added_at": 10,
            "completed_at": 20,
            "num_peers": 3,
            "num_seeds": 4
        });

        let mapped = map_summary(&raw);

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
    fn torrentng_backend_capabilities_match_native_mutation_routes() {
        let backend = TorrentngBackend::new(&TorrentngConfig::default()).unwrap();

        let capabilities = backend.capabilities();

        assert!(capabilities.supports_categories);
        assert!(capabilities.supports_file_priority);
        assert!(capabilities.supports_tracker_edit);
        assert!(capabilities.supports_recheck);
    }
}
