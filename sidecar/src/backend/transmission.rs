use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use reqwest::Url;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::{BackendCapabilities, BackendStatus, BackendType, TorrentBackend};
use crate::{
    config::TransmissionConfig,
    rtorrent::{files::RawFile, torrents::RawTorrent, trackers::RawTracker, TransferRates},
};

pub struct TransmissionBackend {
    client: reqwest::Client,
    url: Url,
    username: Option<String>,
    password: Option<String>,
    session_id: Mutex<Option<String>>,
}

impl TransmissionBackend {
    pub fn new(cfg: &TransmissionConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs.max(1)))
            .danger_accept_invalid_certs(cfg.accept_invalid_certs)
            .build()
            .context("create Transmission RPC client")?;
        Ok(Self {
            client,
            url: Url::parse(cfg.url.trim()).context("parse transmission.url")?,
            username: cfg.username.clone(),
            password: cfg.password.clone(),
            session_id: Mutex::new(None),
        })
    }

    async fn rpc(&self, method: &str, arguments: Value) -> Result<Value> {
        let body = json!({ "method": method, "arguments": arguments });
        for _ in 0..2 {
            let mut req = self.client.post(self.url.clone()).json(&body);
            if let Some(username) = &self.username {
                req = req.basic_auth(username, self.password.as_deref());
            }
            if let Some(session_id) = self.session_id.lock().await.clone() {
                req = req.header("x-transmission-session-id", session_id);
            }
            let response = req
                .send()
                .await
                .with_context(|| format!("Transmission RPC {method}"))?;
            if response.status() == reqwest::StatusCode::CONFLICT {
                if let Some(value) = response
                    .headers()
                    .get("x-transmission-session-id")
                    .and_then(|value| value.to_str().ok())
                {
                    *self.session_id.lock().await = Some(value.to_owned());
                    continue;
                }
            }
            let payload: Value = response
                .error_for_status()
                .with_context(|| format!("Transmission RPC {method}"))?
                .json()
                .await
                .with_context(|| format!("decode Transmission RPC {method}"))?;
            if payload.get("result").and_then(Value::as_str) == Some("success") {
                return Ok(payload.get("arguments").cloned().unwrap_or(Value::Null));
            }
            bail!(
                "Transmission RPC {method} failed: {}",
                payload
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            );
        }
        bail!("Transmission RPC {method} failed to acquire session id")
    }
}

#[async_trait]
impl TorrentBackend for TransmissionBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::Transmission
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_tags: false,
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
        match self.rpc("session-get", json!({})).await {
            Ok(_) => BackendStatus::Connected,
            Err(_) => BackendStatus::Unreachable,
        }
    }

    async fn transfer_rates(&self) -> Result<TransferRates> {
        let args = self
            .rpc("session-stats", json!({}))
            .await
            .context("Transmission session-stats")?;
        Ok(TransferRates {
            download: int(&args, "downloadSpeed").max(0),
            upload: int(&args, "uploadSpeed").max(0),
        })
    }

    async fn list_torrents(&self) -> Result<Vec<RawTorrent>> {
        let args = self
            .rpc(
                "torrent-get",
                json!({
                    "fields": [
                        "hashString", "name", "totalSize", "sizeWhenDone", "haveValid",
                        "rateDownload", "rateUpload", "uploadedEver", "downloadedEver",
                        "uploadRatio", "status", "downloadDir", "addedDate", "doneDate",
                        "peersConnected", "errorString", "percentDone", "trackerStats", "labels", "group"
                    ]
                }),
            )
            .await?;
        Ok(args
            .get("torrents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(map_torrent)
            .collect())
    }

    async fn add_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        self.rpc(
            "torrent-add",
            json!({
                "filename": magnet,
                "download-dir": empty_to_null(save_path),
                "paused": !start,
                "labels": if category.is_empty() { json!([]) } else { json!([category]) },
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
        self.rpc(
            "torrent-add",
            json!({
                "metainfo": general_purpose::STANDARD.encode(data),
                "download-dir": empty_to_null(save_path),
                "paused": !start,
                "labels": if category.is_empty() { json!([]) } else { json!([category]) },
            }),
        )
        .await?;
        Ok(())
    }

    async fn add_url(&self, url: &str, save_path: &str, category: &str, start: bool) -> Result<()> {
        self.add_magnet(url, save_path, category, start).await
    }

    async fn remove(&self, hash: &str, delete_data: bool) -> Result<()> {
        self.rpc(
            "torrent-remove",
            json!({ "ids": [hash], "delete-local-data": delete_data }),
        )
        .await?;
        Ok(())
    }

    async fn start(&self, hash: &str) -> Result<()> {
        self.rpc("torrent-start", json!({ "ids": [hash] })).await?;
        Ok(())
    }

    async fn stop(&self, hash: &str) -> Result<()> {
        self.rpc("torrent-stop", json!({ "ids": [hash] })).await?;
        Ok(())
    }

    async fn recheck(&self, hash: &str) -> Result<()> {
        self.rpc("torrent-verify", json!({ "ids": [hash] })).await?;
        Ok(())
    }

    async fn reannounce(&self, hash: &str) -> Result<()> {
        self.rpc("torrent-reannounce", json!({ "ids": [hash] }))
            .await?;
        Ok(())
    }

    async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>> {
        let args = self
            .rpc(
                "torrent-get",
                json!({ "ids": [hash], "fields": ["trackerStats"] }),
            )
            .await?;
        let trackers = args
            .get("torrents")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|torrent| torrent.get("trackerStats"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .map(map_tracker)
            .collect();
        Ok(trackers)
    }

    async fn add_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.rpc("torrent-set", json!({ "ids": [hash], "trackerAdd": [url] }))
            .await?;
        Ok(())
    }

    async fn edit_tracker(&self, hash: &str, original_url: &str, new_url: &str) -> Result<()> {
        let tracker_id = self.tracker_id_by_url(hash, original_url).await?;
        self.rpc(
            "torrent-set",
            json!({ "ids": [hash], "trackerReplace": [[tracker_id, new_url]] }),
        )
        .await?;
        Ok(())
    }

    async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()> {
        let tracker_id = self.tracker_id_by_url(hash, url).await?;
        self.rpc(
            "torrent-set",
            json!({ "ids": [hash], "trackerRemove": [tracker_id] }),
        )
        .await?;
        Ok(())
    }

    async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>> {
        let args = self
            .rpc(
                "torrent-get",
                json!({ "ids": [hash], "fields": ["files", "fileStats"] }),
            )
            .await?;
        let Some(torrent) = args
            .get("torrents")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
        else {
            return Ok(Vec::new());
        };
        let files = torrent
            .get("files")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let stats = torrent
            .get("fileStats")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let stat = stats.get(index).unwrap_or(&Value::Null);
                let size = int(file, "length");
                let completed = int(file, "bytesCompleted");
                RawFile {
                    index,
                    path: string(file, "name"),
                    size_bytes: size,
                    size_chunks: size,
                    completed_chunks: completed,
                    priority: int(stat, "priority"),
                    is_created: true,
                    is_open: stat.get("wanted").and_then(Value::as_bool).unwrap_or(true),
                }
            })
            .collect())
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()> {
        let key = match priority {
            0 => "files-unwanted",
            2.. => "priority-high",
            _ => "priority-normal",
        };
        self.rpc("torrent-set", json!({ "ids": [hash], key: [file_index] }))
            .await?;
        Ok(())
    }

    async fn set_category(&self, hash: &str, category: &str) -> Result<()> {
        self.rpc(
            "torrent-set",
            json!({ "ids": [hash], "labels": if category.is_empty() { json!([]) } else { json!([category]) } }),
        )
        .await?;
        Ok(())
    }

    async fn set_location(&self, hash: &str, location: &str) -> Result<()> {
        self.rpc(
            "torrent-set-location",
            json!({ "ids": [hash], "location": location, "move": true }),
        )
        .await?;
        Ok(())
    }

    async fn rename_file(&self, hash: &str, file_index: usize, name: &str) -> Result<()> {
        let files = self.list_files(hash).await?;
        let Some(file) = files.into_iter().find(|file| file.index == file_index) else {
            bail!("Transmission file index not found: {file_index}");
        };
        self.rpc(
            "torrent-rename-path",
            json!({ "ids": [hash], "path": file.path, "name": name }),
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
        let ratio_limit = (ratio_limit_milli >= 0).then_some(ratio_limit_milli as f64 / 1000.0);
        let seed_ratio_mode = if ratio_limit.is_some() { 1 } else { 0 };
        let seed_idle_limit = (seeding_time_limit >= 0).then_some(seeding_time_limit);
        let seed_idle_mode = if seed_idle_limit.is_some() { 1 } else { 0 };
        self.rpc(
            "torrent-set",
            json!({
                "ids": [hash],
                "seedRatioLimit": ratio_limit,
                "seedRatioMode": seed_ratio_mode,
                "seedIdleLimit": seed_idle_limit,
                "seedIdleMode": seed_idle_mode,
            }),
        )
        .await?;
        Ok(())
    }
}

impl TransmissionBackend {
    async fn tracker_id_by_url(&self, hash: &str, url: &str) -> Result<i64> {
        let trackers = self.list_trackers(hash).await?;
        trackers
            .into_iter()
            .find(|tracker| tracker.url == url)
            .map(|tracker| tracker.id)
            .with_context(|| format!("Transmission tracker not found: {url}"))
    }
}

fn empty_to_null(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

fn map_torrent(t: &Value) -> RawTorrent {
    let percent = t.get("percentDone").and_then(Value::as_f64).unwrap_or(0.0);
    let complete = percent >= 1.0;
    let status = int(t, "status");
    let tracker = t
        .get("trackerStats")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .map(|tracker| string(tracker, "announce"))
        .unwrap_or_default();
    RawTorrent {
        hash: string(t, "hashString"),
        name: string(t, "name"),
        size_bytes: int(t, "totalSize").max(int(t, "sizeWhenDone")),
        bytes_done: int(t, "haveValid"),
        down_rate: int(t, "rateDownload"),
        up_rate: int(t, "rateUpload"),
        up_total: int(t, "uploadedEver"),
        down_total: int(t, "downloadedEver"),
        ratio: (t.get("uploadRatio").and_then(Value::as_f64).unwrap_or(0.0) * 1000.0) as i64,
        is_active: status != 0,
        is_open: status != 0,
        complete,
        state: if string(t, "errorString").is_empty() {
            1
        } else {
            3
        },
        priority: 0,
        category: t
            .get("labels")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
            .or_else(|| t.get("group").and_then(Value::as_str))
            .unwrap_or("")
            .to_owned(),
        base_path: string(t, "downloadDir"),
        directory: string(t, "downloadDir"),
        creation_date: int(t, "addedDate"),
        timestamp_finished: int(t, "doneDate"),
        tracker_focus: 0,
        peers_connected: int(t, "peersConnected"),
        peers_complete: 0,
        message: string(t, "errorString"),
        tracker_url: tracker,
        tags: String::new(),
    }
}

fn map_tracker((index, tracker): (usize, &Value)) -> RawTracker {
    RawTracker {
        url: string(tracker, "announce"),
        id: int(tracker, "id"),
        group: int(tracker, "tier"),
        group_index: index as i64,
        is_enabled: true,
        is_open: int(tracker, "lastAnnounceSucceeded") != 0,
        is_extra_tracker: false,
        activity_time_last: int(tracker, "lastAnnounceTime"),
        activity_time_next: int(tracker, "nextAnnounceTime"),
        min_interval: 0,
        normal_interval: 0,
        failed_counter: i64::from(
            int(tracker, "lastAnnounceSucceeded") == 0
                && !string(tracker, "lastAnnounceResult").is_empty(),
        ),
        success_counter: int(tracker, "lastAnnounceSucceeded"),
        scrape_incomplete: int(tracker, "leecherCount"),
        scrape_complete: int(tracker, "seederCount"),
        scrape_downloaded: int(tracker, "downloadCount"),
        message: string(tracker, "lastAnnounceResult"),
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
        .and_then(|value| value.as_i64().or_else(|| value.as_bool().map(i64::from)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transmission_backend_advertises_tracker_mutations() {
        let backend = TransmissionBackend::new(&TransmissionConfig::default()).unwrap();

        let capabilities = backend.capabilities();

        assert!(capabilities.supports_file_priority);
        assert!(capabilities.supports_tracker_edit);
        assert!(capabilities.supports_recheck);
    }

    #[test]
    fn maps_transmission_tracker_ids_for_mutation_calls() {
        let tracker = json!({
            "announce": "https://tracker.example/announce",
            "id": 17,
            "tier": 2,
            "lastAnnounceSucceeded": true,
            "lastAnnounceTime": 100,
            "nextAnnounceTime": 200,
            "leecherCount": 3,
            "seederCount": 4,
            "downloadCount": 5
        });

        let mapped = map_tracker((0, &tracker));

        assert_eq!(mapped.url, "https://tracker.example/announce");
        assert_eq!(mapped.id, 17);
        assert_eq!(mapped.group, 2);
        assert!(mapped.is_open);
        assert_eq!(mapped.scrape_complete, 4);
    }
}
