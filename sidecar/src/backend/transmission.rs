use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use reqwest::Url;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::{
    response_json_bounded, BackendCapabilities, BackendStatus, BackendType, TorrentBackend,
    MAX_BACKEND_JSON_BYTES,
};
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
            let payload: Value = response_json_bounded(
                response,
                MAX_BACKEND_JSON_BYTES,
                &format!("Transmission RPC {method}"),
            )
            .await?;
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
            supports_torrent_export: false,
            supports_webseed_reads: false,
            supports_piece_state_reads: false,
            supports_piece_hash_reads: false,
            supports_peer_snapshots: false,
            supports_peer_add: false,
            supports_peer_ban: false,
            supports_queue_order: false,
            supports_per_torrent_limits: true,
            supports_global_limits: true,
            supports_share_limits: true,
            supports_mode_flags: false,
            supports_location_update: true,
            supports_torrent_rename: false,
            supports_file_rename: true,
            supports_runtime_user_agent: false,
            supports_config_overlay: false,
            supports_restart: false,
        }
    }

    async fn health(&self) -> BackendStatus {
        match self.rpc("session-get", json!({})).await {
            Ok(value) if value.as_object().is_some() => BackendStatus::Connected,
            Err(_) => BackendStatus::Unreachable,
            Ok(_) => BackendStatus::Unreachable,
        }
    }

    async fn transfer_rates(&self) -> Result<TransferRates> {
        let args = self
            .rpc("session-stats", json!({}))
            .await
            .context("Transmission session-stats")?;
        Ok(TransferRates {
            download: required_nonnegative_i64(&args, "downloadSpeed")?,
            upload: required_nonnegative_i64(&args, "uploadSpeed")?,
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
        let torrents = required_array(&args, "torrents", "torrent-get")?;
        torrents.iter().map(map_torrent).collect()
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
                json!({ "ids": [hash], "fields": ["hashString", "trackerStats"] }),
            )
            .await?;
        let torrents = required_array(&args, "torrents", "torrent-get")?;
        let torrent = torrents
            .iter()
            .find(|torrent| {
                torrent
                    .get("hashString")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.eq_ignore_ascii_case(hash))
            })
            .ok_or_else(|| {
                anyhow::anyhow!("Transmission torrent-get returned no matching torrent")
            })?;
        let trackers = required_array(torrent, "trackerStats", "torrent-get")?;
        trackers.iter().enumerate().map(map_tracker).collect()
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
                json!({ "ids": [hash], "fields": ["hashString", "files", "fileStats"] }),
            )
            .await?;
        let torrents = required_array(&args, "torrents", "torrent-get")?;
        let torrent = torrents
            .iter()
            .find(|torrent| {
                torrent
                    .get("hashString")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.eq_ignore_ascii_case(hash))
            })
            .ok_or_else(|| {
                anyhow::anyhow!("Transmission torrent-get returned no matching torrent")
            })?;
        let files = required_array(torrent, "files", "torrent-get")?;
        let stats = required_array(torrent, "fileStats", "torrent-get")?;
        if files.len() != stats.len() {
            bail!(
                "Transmission torrent-get returned {} files but {} fileStats entries",
                files.len(),
                stats.len()
            );
        }
        let mut result = Vec::with_capacity(files.len());
        for (index, file) in files.iter().enumerate() {
            let stat = &stats[index];
            let size = required_nonnegative_i64(file, "length")?;
            let completed = required_nonnegative_i64(file, "bytesCompleted")?;
            if completed > size {
                bail!(
                    "Transmission file {index} reports {completed} completed bytes for size {size}"
                );
            }
            let wanted = stat.get("wanted").and_then(Value::as_bool).ok_or_else(|| {
                anyhow::anyhow!("Transmission fileStats entry {index} omitted wanted")
            })?;
            let transmission_priority = required_i64(stat, "priority")?;
            if !(-1..=1).contains(&transmission_priority) {
                bail!(
                    "Transmission fileStats entry {index} returned invalid priority {transmission_priority}"
                );
            }
            result.push(RawFile {
                index,
                path: required_nonempty_string(file, "name")?,
                size_bytes: size,
                size_chunks: size,
                completed_chunks: completed,
                // Transmission uses -1/0/1 for low/normal/high and a
                // separate wanted bit. Normalize it to the facade's
                // 0/1/2 off/normal/high representation.
                priority: if !wanted {
                    0
                } else if transmission_priority > 0 {
                    2
                } else {
                    1
                },
                is_created: true,
                is_open: wanted,
            });
        }
        Ok(result)
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()> {
        if !(0..=2).contains(&priority) {
            bail!("Transmission file priority must be between 0 and 2");
        }
        if !self
            .list_files(hash)
            .await?
            .iter()
            .any(|file| file.index == file_index)
        {
            bail!("Transmission file index not found: {file_index}");
        }
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

    async fn set_download_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        let kib = limit.filter(|value| *value > 0).map(bytes_to_kib_ceil);
        self.rpc(
            "torrent-set",
            json!({
                "ids": [hash],
                "downloadLimited": kib.is_some(),
                "downloadLimit": kib,
            }),
        )
        .await?;
        Ok(())
    }

    async fn set_upload_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        let kib = limit.filter(|value| *value > 0).map(bytes_to_kib_ceil);
        self.rpc(
            "torrent-set",
            json!({
                "ids": [hash],
                "uploadLimited": kib.is_some(),
                "uploadLimit": kib,
            }),
        )
        .await?;
        Ok(())
    }

    async fn set_global_download_limit(&self, limit: i64) -> Result<()> {
        let kib = (limit > 0).then_some(bytes_to_kib_ceil(limit));
        self.rpc(
            "session-set",
            json!({
                "speed-limit-down-enabled": kib.is_some(),
                "speed-limit-down": kib.unwrap_or(0),
            }),
        )
        .await?;
        Ok(())
    }

    async fn set_global_upload_limit(&self, limit: i64) -> Result<()> {
        let kib = (limit > 0).then_some(bytes_to_kib_ceil(limit));
        self.rpc(
            "session-set",
            json!({
                "speed-limit-up-enabled": kib.is_some(),
                "speed-limit-up": kib.unwrap_or(0),
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

fn bytes_to_kib_ceil(bytes: i64) -> i64 {
    bytes.saturating_add(1023) / 1024
}

fn required_array<'a>(value: &'a Value, key: &str, method: &str) -> Result<&'a [Value]> {
    value
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow::anyhow!("Transmission {method} result has no array field {key:?}"))
}

fn map_torrent(t: &Value) -> Result<RawTorrent> {
    let percent = required_f64(t, "percentDone")?;
    if !percent.is_finite() || !(0.0..=1.0).contains(&percent) {
        bail!("Transmission torrent returned invalid percentDone");
    }
    let complete = percent >= 1.0;
    let status = required_nonnegative_i64(t, "status")?;
    let trackers = required_array(t, "trackerStats", "torrent-get")?;
    let tracker = trackers
        .first()
        .map(|tracker| required_nonempty_string(tracker, "announce"))
        .transpose()?
        .unwrap_or_default();
    let labels = match t.get("labels") {
        None => String::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Transmission torrent labels is not an array"))?
            .iter()
            .map(|label| {
                label
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Transmission torrent label is not a string"))
            })
            .collect::<Result<Vec<_>>>()?
            .first()
            .copied()
            .unwrap_or("")
            .to_owned(),
    };
    let category = match t.get("group") {
        None => labels,
        Some(value) => value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Transmission torrent group is not a string"))?
            .to_owned(),
    };
    let size_bytes =
        required_nonnegative_i64(t, "totalSize")?.max(required_nonnegative_i64(t, "sizeWhenDone")?);
    let bytes_done = required_nonnegative_i64(t, "haveValid")?;
    if bytes_done > size_bytes {
        bail!("Transmission torrent reports {bytes_done} completed bytes for size {size_bytes}");
    }
    Ok(RawTorrent {
        hash: required_nonempty_string(t, "hashString")?,
        name: required_nonempty_string(t, "name")?,
        size_bytes,
        bytes_done,
        down_rate: required_nonnegative_i64(t, "rateDownload")?,
        up_rate: required_nonnegative_i64(t, "rateUpload")?,
        up_total: required_nonnegative_i64(t, "uploadedEver")?,
        down_total: required_nonnegative_i64(t, "downloadedEver")?,
        ratio: super::ratio_milli(Some(required_nonnegative_f64(t, "uploadRatio")?)),
        is_active: status != 0,
        is_open: status != 0,
        complete,
        state: if required_string(t, "errorString")?.is_empty() {
            1
        } else {
            3
        },
        priority: 0,
        category,
        base_path: required_nonempty_string(t, "downloadDir")?,
        directory: required_nonempty_string(t, "downloadDir")?,
        creation_date: required_i64(t, "addedDate")?,
        timestamp_finished: required_i64(t, "doneDate")?,
        tracker_focus: 0,
        peers_connected: required_nonnegative_i64(t, "peersConnected")?,
        peers_complete: 0,
        message: required_string(t, "errorString")?,
        tracker_url: tracker,
        tags: String::new(),
    })
}

fn map_tracker((index, tracker): (usize, &Value)) -> Result<RawTracker> {
    let last_succeeded = required_bool_or_int(tracker, "lastAnnounceSucceeded")?;
    Ok(RawTracker {
        url: required_nonempty_string(tracker, "announce")?,
        id: required_i64(tracker, "id")?,
        group: required_i64(tracker, "tier")?,
        group_index: index as i64,
        is_enabled: true,
        is_open: last_succeeded,
        is_extra_tracker: false,
        activity_time_last: required_i64(tracker, "lastAnnounceTime")?,
        activity_time_next: required_i64(tracker, "nextAnnounceTime")?,
        min_interval: 0,
        normal_interval: 0,
        failed_counter: i64::from(
            !last_succeeded && !optional_string(tracker, "lastAnnounceResult")?.is_empty(),
        ),
        success_counter: i64::from(last_succeeded),
        scrape_incomplete: required_nonnegative_i64(tracker, "leecherCount")?,
        scrape_complete: required_nonnegative_i64(tracker, "seederCount")?,
        scrape_downloaded: required_nonnegative_i64(tracker, "downloadCount")?,
        message: optional_string(tracker, "lastAnnounceResult")?,
    })
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Transmission response omitted valid {key}"))
}

fn optional_string(value: &Value, key: &str) -> Result<String> {
    match value.get(key) {
        None => Ok(String::new()),
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Transmission response has invalid {key}")),
    }
}

fn required_nonempty_string(value: &Value, key: &str) -> Result<String> {
    let value = required_string(value, key)?;
    if value.trim().is_empty() {
        bail!("Transmission response omitted non-empty {key}");
    }
    Ok(value)
}

fn required_i64(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("Transmission response omitted valid {key}"))
}

fn required_nonnegative_i64(value: &Value, key: &str) -> Result<i64> {
    let value = required_i64(value, key)?;
    if value < 0 {
        bail!("Transmission response contains negative {key}");
    }
    Ok(value)
}

fn required_f64(value: &Value, key: &str) -> Result<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("Transmission response omitted valid {key}"))
}

fn required_nonnegative_f64(value: &Value, key: &str) -> Result<f64> {
    let value = required_f64(value, key)?;
    if !value.is_finite() || value < 0.0 {
        bail!("Transmission response contains invalid {key}");
    }
    Ok(value)
}

fn required_bool_or_int(value: &Value, key: &str) -> Result<bool> {
    match value.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(Value::Number(value)) => value
            .as_i64()
            .map(|value| value != 0)
            .ok_or_else(|| anyhow::anyhow!("Transmission response has invalid {key}")),
        None => Err(anyhow::anyhow!("Transmission response omitted valid {key}")),
        Some(_) => Err(anyhow::anyhow!("Transmission response has invalid {key}")),
    }
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
        assert!(capabilities.supports_per_torrent_limits);
        assert!(capabilities.supports_global_limits);
        assert!(capabilities.supports_share_limits);
        assert!(capabilities.supports_location_update);
        assert!(capabilities.supports_file_rename);
        assert!(!capabilities.supports_peer_add);
        assert!(!capabilities.supports_peer_ban);
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

        let mapped = map_tracker((0, &tracker)).unwrap();

        assert_eq!(mapped.url, "https://tracker.example/announce");
        assert_eq!(mapped.id, 17);
        assert_eq!(mapped.group, 2);
        assert!(mapped.is_open);
        assert_eq!(mapped.scrape_complete, 4);
    }
}
