use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use reqwest::Url;
use serde_json::{json, Value};

use super::{BackendCapabilities, BackendStatus, BackendType, TorrentBackend};
use crate::{
    config::DelugeConfig,
    rtorrent::{files::RawFile, torrents::RawTorrent, trackers::RawTracker, TransferRates},
};

pub struct DelugeBackend {
    client: reqwest::Client,
    url: Url,
    password: Option<String>,
}

impl DelugeBackend {
    pub fn new(cfg: &DelugeConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs.max(1)))
            .danger_accept_invalid_certs(cfg.accept_invalid_certs)
            .build()
            .context("create Deluge Web client")?;
        Ok(Self {
            client,
            url: Url::parse(cfg.url.trim()).context("parse deluge.url")?,
            password: cfg.password.clone(),
        })
    }

    async fn ensure_login(&self) -> Result<()> {
        if let Some(password) = &self.password {
            let ok = self
                .rpc_raw("auth.login", json!([password]))
                .await?
                .as_bool()
                .unwrap_or(false);
            if !ok {
                bail!("Deluge auth.login failed");
            }
        }
        Ok(())
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        self.ensure_login().await?;
        self.rpc_raw(method, params).await
    }

    async fn rpc_raw(&self, method: &str, params: Value) -> Result<Value> {
        let response: Value = self
            .client
            .post(self.url.clone())
            .json(&json!({ "id": 1, "method": method, "params": params }))
            .send()
            .await
            .with_context(|| format!("Deluge RPC {method}"))?
            .error_for_status()
            .with_context(|| format!("Deluge RPC {method}"))?
            .json()
            .await
            .with_context(|| format!("decode Deluge RPC {method}"))?;
        if !response.get("error").unwrap_or(&Value::Null).is_null() {
            bail!("Deluge RPC {method} failed: {}", response["error"]);
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }
}

#[async_trait]
impl TorrentBackend for DelugeBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::Deluge
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_tags: false,
            supports_categories: false,
            supports_file_priority: true,
            supports_tracker_edit: true,
            supports_recheck: true,
            supports_runtime_user_agent: false,
            supports_config_overlay: false,
            supports_restart: false,
        }
    }

    async fn health(&self) -> BackendStatus {
        match self.rpc("web.connected", json!([])).await {
            Ok(_) => BackendStatus::Connected,
            Err(_) => BackendStatus::Unreachable,
        }
    }

    async fn transfer_rates(&self) -> Result<TransferRates> {
        let status = self
            .rpc(
                "core.get_session_status",
                json!([["payload_download_rate", "payload_upload_rate"]]),
            )
            .await?;
        Ok(TransferRates {
            download: int(&status, "payload_download_rate").max(0),
            upload: int(&status, "payload_upload_rate").max(0),
        })
    }

    async fn list_torrents(&self) -> Result<Vec<RawTorrent>> {
        let fields = [
            "name",
            "total_size",
            "total_done",
            "download_payload_rate",
            "upload_payload_rate",
            "total_uploaded",
            "total_downloaded",
            "ratio",
            "state",
            "progress",
            "save_path",
            "time_added",
            "completed_time",
            "num_peers",
            "num_seeds",
            "tracker_status",
            "trackers",
            "message",
        ];
        let value = self
            .rpc("core.get_torrents_status", json!([{}, fields]))
            .await?;
        Ok(value
            .as_object()
            .into_iter()
            .flat_map(|items| items.iter())
            .map(|(hash, torrent)| map_torrent(hash, torrent))
            .collect())
    }

    async fn add_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        _category: &str,
        start: bool,
    ) -> Result<()> {
        self.rpc(
            "core.add_torrent_magnet",
            json!([magnet, { "download_location": save_path, "add_paused": !start }]),
        )
        .await?;
        Ok(())
    }

    async fn add_torrent(
        &self,
        data: &[u8],
        save_path: &str,
        _category: &str,
        start: bool,
    ) -> Result<()> {
        self.rpc(
            "core.add_torrent_file",
            json!([
                "upload.torrent",
                general_purpose::STANDARD.encode(data),
                { "download_location": save_path, "add_paused": !start }
            ]),
        )
        .await?;
        Ok(())
    }

    async fn remove(&self, hash: &str, delete_data: bool) -> Result<()> {
        self.rpc("core.remove_torrent", json!([hash, delete_data]))
            .await?;
        Ok(())
    }

    async fn start(&self, hash: &str) -> Result<()> {
        self.rpc("core.resume_torrent", json!([[hash]])).await?;
        Ok(())
    }

    async fn stop(&self, hash: &str) -> Result<()> {
        self.rpc("core.pause_torrent", json!([[hash]])).await?;
        Ok(())
    }

    async fn recheck(&self, hash: &str) -> Result<()> {
        self.rpc("core.force_recheck", json!([[hash]])).await?;
        Ok(())
    }

    async fn reannounce(&self, hash: &str) -> Result<()> {
        self.rpc("core.force_reannounce", json!([[hash]])).await?;
        Ok(())
    }

    async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>> {
        let torrent = self
            .rpc(
                "core.get_torrent_status",
                json!([hash, ["trackers", "tracker_status"]]),
            )
            .await?;
        Ok(torrent
            .get("trackers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .map(map_tracker)
            .collect())
    }

    async fn add_tracker(&self, hash: &str, url: &str) -> Result<()> {
        let mut trackers = self.list_trackers(hash).await?;
        trackers.push(RawTracker {
            url: url.to_owned(),
            id: trackers.len() as i64,
            group: trackers.len() as i64,
            group_index: trackers.len() as i64,
            is_enabled: true,
            is_open: false,
            is_extra_tracker: true,
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
        self.set_trackers(hash, &trackers).await
    }

    async fn edit_tracker(&self, hash: &str, original_url: &str, new_url: &str) -> Result<()> {
        let mut trackers = self.list_trackers(hash).await?;
        for tracker in &mut trackers {
            if tracker.url == original_url {
                tracker.url = new_url.to_owned();
            }
        }
        self.set_trackers(hash, &trackers).await
    }

    async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()> {
        let trackers: Vec<_> = self
            .list_trackers(hash)
            .await?
            .into_iter()
            .filter(|tracker| tracker.url != url)
            .collect();
        self.set_trackers(hash, &trackers).await
    }

    async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>> {
        let torrent = self
            .rpc(
                "core.get_torrent_status",
                json!([hash, ["files", "file_priorities"]]),
            )
            .await?;
        let priorities = torrent
            .get("file_priorities")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(torrent
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, file)| {
                let size = int(file, "size");
                RawFile {
                    index,
                    path: string(file, "path"),
                    size_bytes: size,
                    size_chunks: size,
                    completed_chunks: int(file, "progress"),
                    priority: priorities.get(index).and_then(Value::as_i64).unwrap_or(1),
                    is_created: true,
                    is_open: true,
                }
            })
            .collect())
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()> {
        let mut files = self.list_files(hash).await?;
        let max_index = files
            .iter()
            .map(|file| file.index)
            .max()
            .unwrap_or(file_index);
        let mut priorities = vec![1_i64; max_index + 1];
        for file in files.drain(..) {
            priorities[file.index] = file.priority;
        }
        if file_index < priorities.len() {
            priorities[file_index] = priority;
        }
        self.rpc(
            "core.set_torrent_file_priorities",
            json!([hash, priorities]),
        )
        .await?;
        Ok(())
    }

    async fn set_category(&self, _hash: &str, _category: &str) -> Result<()> {
        Ok(())
    }

    async fn set_location(&self, hash: &str, location: &str) -> Result<()> {
        self.rpc("core.move_storage", json!([[hash], location]))
            .await?;
        Ok(())
    }

    async fn rename_file(&self, hash: &str, file_index: usize, name: &str) -> Result<()> {
        self.rpc(
            "core.rename_files",
            json!([hash, [[file_index as i64, name]]]),
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
        let options = deluge_share_options(ratio_limit_milli, seeding_time_limit);
        if !options.is_empty() {
            self.rpc(
                "core.set_torrent_options",
                json!([[hash], Value::Object(options)]),
            )
            .await?;
        }
        Ok(())
    }

    async fn set_download_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        let value = limit
            .filter(|value| *value > 0)
            .map(bytes_to_kib)
            .unwrap_or(-1.0);
        self.rpc(
            "core.set_torrent_options",
            json!([[hash], { "max_download_speed": value }]),
        )
        .await?;
        Ok(())
    }

    async fn set_upload_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        let value = limit
            .filter(|value| *value > 0)
            .map(bytes_to_kib)
            .unwrap_or(-1.0);
        self.rpc(
            "core.set_torrent_options",
            json!([[hash], { "max_upload_speed": value }]),
        )
        .await?;
        Ok(())
    }

    async fn set_global_download_limit(&self, limit: i64) -> Result<()> {
        let value = if limit > 0 { bytes_to_kib(limit) } else { -1.0 };
        self.rpc("core.set_config", json!([{ "max_download_speed": value }]))
            .await?;
        Ok(())
    }

    async fn set_global_upload_limit(&self, limit: i64) -> Result<()> {
        let value = if limit > 0 { bytes_to_kib(limit) } else { -1.0 };
        self.rpc("core.set_config", json!([{ "max_upload_speed": value }]))
            .await?;
        Ok(())
    }
}

fn deluge_share_options(
    ratio_limit_milli: i64,
    seeding_time_limit: i64,
) -> serde_json::Map<String, Value> {
    let mut options = serde_json::Map::new();
    if ratio_limit_milli >= 0 {
        options.insert("stop_at_ratio".to_owned(), json!(true));
        options.insert(
            "stop_ratio".to_owned(),
            json!(ratio_limit_milli as f64 / 1000.0),
        );
    } else if ratio_limit_milli == -1 {
        options.insert("stop_at_ratio".to_owned(), json!(false));
    }

    if seeding_time_limit >= 0 {
        options.insert("seed_time_limit".to_owned(), json!(seeding_time_limit));
        options.insert("max_seed_time".to_owned(), json!(seeding_time_limit));
    } else if seeding_time_limit == -1 {
        options.insert("seed_time_limit".to_owned(), json!(-1));
        options.insert("max_seed_time".to_owned(), json!(-1));
    }
    options
}

fn bytes_to_kib(bytes: i64) -> f64 {
    bytes as f64 / 1024.0
}

impl DelugeBackend {
    async fn set_trackers(&self, hash: &str, trackers: &[RawTracker]) -> Result<()> {
        let payload: Vec<_> = trackers
            .iter()
            .enumerate()
            .map(|(index, tracker)| json!({ "url": tracker.url, "tier": index }))
            .collect();
        self.rpc("core.set_torrent_trackers", json!([hash, payload]))
            .await?;
        Ok(())
    }
}

fn map_torrent(hash: &str, t: &Value) -> RawTorrent {
    let state = string(t, "state");
    let complete = t.get("progress").and_then(Value::as_f64).unwrap_or(0.0) >= 100.0
        || state.eq_ignore_ascii_case("seeding");
    let message = string(t, "message");
    let tracker_url = t
        .get("trackers")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .map(|tracker| string(tracker, "url"))
        .unwrap_or_else(|| string(t, "tracker_status"));
    RawTorrent {
        hash: hash.to_owned(),
        name: string(t, "name"),
        size_bytes: int(t, "total_size"),
        bytes_done: int(t, "total_done"),
        down_rate: int(t, "download_payload_rate"),
        up_rate: int(t, "upload_payload_rate"),
        up_total: int(t, "total_uploaded"),
        down_total: int(t, "total_downloaded"),
        ratio: (t.get("ratio").and_then(Value::as_f64).unwrap_or(0.0) * 1000.0) as i64,
        is_active: !matches!(state.as_str(), "Paused" | "Error"),
        is_open: !matches!(state.as_str(), "Paused" | "Error"),
        complete,
        state: if state == "Error" { 3 } else { 1 },
        priority: 0,
        category: String::new(),
        base_path: string(t, "save_path"),
        directory: string(t, "save_path"),
        creation_date: int(t, "time_added"),
        timestamp_finished: int(t, "completed_time"),
        tracker_focus: 0,
        peers_connected: int(t, "num_peers"),
        peers_complete: int(t, "num_seeds"),
        message,
        tracker_url,
        tags: String::new(),
    }
}

fn map_tracker((index, tracker): (usize, &Value)) -> RawTracker {
    RawTracker {
        url: string(tracker, "url"),
        id: index as i64,
        group: int(tracker, "tier"),
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
                .and_then(Value::as_f64)
                .map(|value| value as i64)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deluge_share_options_map_ratio_and_seed_time_limits() {
        let options = deluge_share_options(1250, 1440);

        assert_eq!(options["stop_at_ratio"], true);
        assert_eq!(options["stop_ratio"], 1.25);
        assert_eq!(options["seed_time_limit"], 1440);
        assert_eq!(options["max_seed_time"], 1440);
    }

    #[test]
    fn deluge_share_options_disable_ratio_and_seed_time() {
        let options = deluge_share_options(-1, -1);

        assert_eq!(options["stop_at_ratio"], false);
        assert_eq!(options["seed_time_limit"], -1);
        assert_eq!(options["max_seed_time"], -1);
    }
}
