use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use reqwest::Url;
use serde_json::{json, Value};

use super::{
    response_json_bounded, BackendCapabilities, BackendStatus, BackendType, TorrentBackend,
    MAX_BACKEND_JSON_BYTES,
};
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
        let response: Value = response_json_bounded(
            self.client
                .post(self.url.clone())
                .json(&json!({ "id": 1, "method": method, "params": params }))
                .send()
                .await
                .with_context(|| format!("Deluge RPC {method}"))?,
            MAX_BACKEND_JSON_BYTES,
            &format!("Deluge RPC {method}"),
        )
        .await?;
        let response = response
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Deluge RPC {method} returned a non-object response"))?;
        if !response.get("error").unwrap_or(&Value::Null).is_null() {
            bail!("Deluge RPC {method} failed: {}", response["error"]);
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Deluge RPC {method} response has no result"))
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
        match self.rpc("web.connected", json!([])).await {
            Ok(value) if value.as_bool() == Some(true) => BackendStatus::Connected,
            Err(_) => BackendStatus::Unreachable,
            Ok(_) => BackendStatus::Unreachable,
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
            download: required_nonnegative_i64(&status, "payload_download_rate")?,
            upload: required_nonnegative_i64(&status, "payload_upload_rate")?,
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
        let items = value.as_object().ok_or_else(|| {
            anyhow::anyhow!("Deluge core.get_torrents_status returned a non-object result")
        })?;
        items
            .iter()
            .map(|(hash, torrent)| map_torrent(hash, torrent))
            .collect()
    }

    async fn add_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        _category: &str,
        start: bool,
    ) -> Result<()> {
        let result = self
            .rpc(
                "core.add_torrent_magnet",
                json!([magnet, { "download_location": save_path, "add_paused": !start }]),
            )
            .await?;
        require_torrent_id(result, "core.add_torrent_magnet")?;
        Ok(())
    }

    async fn add_torrent(
        &self,
        data: &[u8],
        save_path: &str,
        _category: &str,
        start: bool,
    ) -> Result<()> {
        let result = self
            .rpc(
                "core.add_torrent_file",
                json!([
                    "upload.torrent",
                    general_purpose::STANDARD.encode(data),
                    { "download_location": save_path, "add_paused": !start }
                ]),
            )
            .await?;
        require_torrent_id(result, "core.add_torrent_file")?;
        Ok(())
    }

    async fn remove(&self, hash: &str, delete_data: bool) -> Result<()> {
        let result = self
            .rpc("core.remove_torrent", json!([hash, delete_data]))
            .await?;
        require_true(result, "core.remove_torrent")?;
        Ok(())
    }

    async fn start(&self, hash: &str) -> Result<()> {
        let result = self
            .rpc("core.resume_torrent", single_torrent_params(hash))
            .await?;
        require_not_false(result, "core.resume_torrent")?;
        Ok(())
    }

    async fn stop(&self, hash: &str) -> Result<()> {
        let result = self
            .rpc("core.pause_torrent", single_torrent_params(hash))
            .await?;
        require_not_false(result, "core.pause_torrent")?;
        Ok(())
    }

    async fn recheck(&self, hash: &str) -> Result<()> {
        let result = self
            .rpc("core.force_recheck", torrent_ids_params(hash))
            .await?;
        require_not_false(result, "core.force_recheck")?;
        Ok(())
    }

    async fn reannounce(&self, hash: &str) -> Result<()> {
        let result = self
            .rpc("core.force_reannounce", torrent_ids_params(hash))
            .await?;
        require_not_false(result, "core.force_reannounce")?;
        Ok(())
    }

    async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>> {
        let torrent = self
            .rpc(
                "core.get_torrent_status",
                json!([hash, ["trackers", "tracker_status"]]),
            )
            .await?;
        let trackers = required_array(&torrent, "trackers", "core.get_torrent_status")?;
        trackers.iter().enumerate().map(map_tracker).collect()
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
        let mut changed = false;
        for tracker in &mut trackers {
            if tracker.url == original_url {
                tracker.url = new_url.to_owned();
                changed = true;
            }
        }
        if !changed {
            bail!("Deluge tracker not found: {original_url}");
        }
        self.set_trackers(hash, &trackers).await
    }

    async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()> {
        let trackers = self.list_trackers(hash).await?;
        let original_len = trackers.len();
        let trackers: Vec<_> = trackers
            .into_iter()
            .filter(|tracker| tracker.url != url)
            .collect();
        if trackers.len() == original_len {
            bail!("Deluge tracker not found: {url}");
        }
        self.set_trackers(hash, &trackers).await
    }

    async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>> {
        let torrent = self
            .rpc(
                "core.get_torrent_status",
                json!([hash, ["files", "file_priorities"]]),
            )
            .await?;
        let priorities = required_array(&torrent, "file_priorities", "core.get_torrent_status")?;
        let files = required_array(&torrent, "files", "core.get_torrent_status")?;
        if priorities.len() != files.len() {
            bail!(
                "Deluge torrent status returned {} files but {} file priorities",
                files.len(),
                priorities.len()
            );
        }
        let mut result = Vec::with_capacity(files.len());
        for (index, file) in files.iter().enumerate() {
            let size = required_nonnegative_i64(file, "size")?;
            let progress = required_f64(file, "progress")?;
            if !progress.is_finite() || !(0.0..=100.0).contains(&progress) {
                bail!("Deluge file {index} returned invalid progress");
            }
            let priority = priorities[index].as_i64().ok_or_else(|| {
                anyhow::anyhow!(
                    "Deluge torrent status returned a non-integer priority for file {index}"
                )
            })?;
            if !(0..=2).contains(&priority) {
                bail!("Deluge file {index} returned invalid priority {priority}");
            }
            let completed = ((size as f64 * (progress / 100.0)).round() as i64).min(size);
            result.push(RawFile {
                index,
                path: required_nonempty_string(file, "path")?,
                size_bytes: size,
                size_chunks: size,
                completed_chunks: completed,
                priority,
                is_created: true,
                is_open: true,
            });
        }
        Ok(result)
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()> {
        if !(0..=2).contains(&priority) {
            bail!("Deluge file priority must be between 0 and 2");
        }
        let mut files = self.list_files(hash).await?;
        let max_index = files
            .iter()
            .map(|file| file.index)
            .max()
            .ok_or_else(|| anyhow::anyhow!("Deluge torrent has no files"))?;
        if file_index > max_index {
            bail!("Deluge file index not found: {file_index}");
        }
        let mut priorities = vec![1_i64; max_index + 1];
        for file in files.drain(..) {
            priorities[file.index] = file.priority;
        }
        priorities[file_index] = priority;
        let result = self
            .rpc(
                "core.set_torrent_file_priorities",
                json!([hash, priorities]),
            )
            .await?;
        require_not_false(result, "core.set_torrent_file_priorities")?;
        Ok(())
    }

    async fn set_category(&self, _hash: &str, _category: &str) -> Result<()> {
        bail!("Deluge backend does not support categories")
    }

    async fn set_location(&self, hash: &str, location: &str) -> Result<()> {
        let result = self
            .rpc("core.move_storage", json!([[hash], location]))
            .await?;
        require_not_false(result, "core.move_storage")?;
        Ok(())
    }

    async fn rename_file(&self, hash: &str, file_index: usize, name: &str) -> Result<()> {
        let result = self
            .rpc(
                "core.rename_files",
                json!([hash, [[file_index as i64, name]]]),
            )
            .await?;
        require_not_false(result, "core.rename_files")?;
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
            let result = self
                .rpc(
                    "core.set_torrent_options",
                    json!([[hash], Value::Object(options)]),
                )
                .await?;
            require_not_false(result, "core.set_torrent_options")?;
        }
        Ok(())
    }

    async fn set_download_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        let value = limit
            .filter(|value| *value > 0)
            .map(bytes_to_kib)
            .unwrap_or(-1.0);
        let result = self
            .rpc(
                "core.set_torrent_options",
                json!([[hash], { "max_download_speed": value }]),
            )
            .await?;
        require_not_false(result, "core.set_torrent_options")?;
        Ok(())
    }

    async fn set_upload_limit(&self, hash: &str, limit: Option<i64>) -> Result<()> {
        let value = limit
            .filter(|value| *value > 0)
            .map(bytes_to_kib)
            .unwrap_or(-1.0);
        let result = self
            .rpc(
                "core.set_torrent_options",
                json!([[hash], { "max_upload_speed": value }]),
            )
            .await?;
        require_not_false(result, "core.set_torrent_options")?;
        Ok(())
    }

    async fn set_global_download_limit(&self, limit: i64) -> Result<()> {
        let value = if limit > 0 { bytes_to_kib(limit) } else { -1.0 };
        let result = self
            .rpc("core.set_config", json!([{ "max_download_speed": value }]))
            .await?;
        require_not_false(result, "core.set_config")?;
        Ok(())
    }

    async fn set_global_upload_limit(&self, limit: i64) -> Result<()> {
        let value = if limit > 0 { bytes_to_kib(limit) } else { -1.0 };
        let result = self
            .rpc("core.set_config", json!([{ "max_upload_speed": value }]))
            .await?;
        require_not_false(result, "core.set_config")?;
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

fn single_torrent_params(hash: &str) -> Value {
    json!([hash])
}

fn torrent_ids_params(hash: &str) -> Value {
    json!([[hash]])
}

fn require_true(value: Value, method: &str) -> Result<()> {
    if value.as_bool() == Some(true) {
        Ok(())
    } else {
        bail!("Deluge RPC {method} returned a non-success result: {value}")
    }
}

/// Deluge mutators have returned both `null` and `true` for successful calls
/// across Web API versions. A boolean false is nevertheless an explicit
/// failed mutation and must not be projected as success.
fn require_not_false(value: Value, method: &str) -> Result<()> {
    if value == Value::Bool(false) {
        bail!("Deluge RPC {method} returned a failed result")
    }
    Ok(())
}

fn require_torrent_id(value: Value, method: &str) -> Result<()> {
    if value.as_str().is_some_and(|id| !id.trim().is_empty()) {
        Ok(())
    } else {
        bail!("Deluge RPC {method} did not return a torrent id")
    }
}

fn required_array<'a>(value: &'a Value, key: &str, method: &str) -> Result<&'a [Value]> {
    value
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow::anyhow!("Deluge RPC {method} result has no array field {key:?}"))
}

impl DelugeBackend {
    async fn set_trackers(&self, hash: &str, trackers: &[RawTracker]) -> Result<()> {
        let payload: Vec<_> = trackers
            .iter()
            .enumerate()
            .map(|(index, tracker)| json!({ "url": tracker.url, "tier": index }))
            .collect();
        let result = self
            .rpc("core.set_torrent_trackers", json!([hash, payload]))
            .await?;
        require_not_false(result, "core.set_torrent_trackers")?;
        Ok(())
    }
}

fn map_torrent(hash: &str, t: &Value) -> Result<RawTorrent> {
    let state = required_string(t, "state")?;
    let progress = required_f64(t, "progress")?;
    if !progress.is_finite() || !(0.0..=100.0).contains(&progress) {
        bail!("Deluge torrent {hash} returned invalid progress");
    }
    let trackers = required_array(t, "trackers", "core.get_torrents_status")?;
    let tracker_url = trackers
        .first()
        .map(|tracker| required_nonempty_string(tracker, "url"))
        .transpose()?
        .unwrap_or_default();
    let size_bytes = required_nonnegative_i64(t, "total_size")?;
    let bytes_done = required_nonnegative_i64(t, "total_done")?;
    if bytes_done > size_bytes {
        bail!("Deluge torrent {hash} reports {bytes_done} completed bytes for size {size_bytes}");
    }
    let complete = progress >= 100.0 || state.eq_ignore_ascii_case("seeding");
    let stopped = state.eq_ignore_ascii_case("paused") || state.eq_ignore_ascii_case("error");
    let message = required_string(t, "message")?;
    Ok(RawTorrent {
        hash: hash.to_owned(),
        name: required_nonempty_string(t, "name")?,
        size_bytes,
        bytes_done,
        down_rate: required_nonnegative_i64(t, "download_payload_rate")?,
        up_rate: required_nonnegative_i64(t, "upload_payload_rate")?,
        up_total: required_nonnegative_i64(t, "total_uploaded")?,
        down_total: required_nonnegative_i64(t, "total_downloaded")?,
        ratio: super::ratio_milli(Some(required_nonnegative_f64(t, "ratio")?)),
        is_active: !stopped,
        is_open: !stopped,
        complete,
        state: if state.eq_ignore_ascii_case("error") {
            3
        } else {
            1
        },
        priority: 0,
        category: String::new(),
        base_path: required_nonempty_string(t, "save_path")?,
        directory: required_nonempty_string(t, "save_path")?,
        creation_date: required_i64(t, "time_added")?,
        timestamp_finished: required_i64(t, "completed_time")?,
        tracker_focus: 0,
        peers_connected: required_nonnegative_i64(t, "num_peers")?,
        peers_complete: required_nonnegative_i64(t, "num_seeds")?,
        message,
        tracker_url,
        tags: String::new(),
    })
}

fn map_tracker((index, tracker): (usize, &Value)) -> Result<RawTracker> {
    Ok(RawTracker {
        url: required_nonempty_string(tracker, "url")?,
        id: index as i64,
        group: required_i64(tracker, "tier")?,
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
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Deluge response omitted valid {key}"))
}

fn required_nonempty_string(value: &Value, key: &str) -> Result<String> {
    let value = required_string(value, key)?;
    if value.trim().is_empty() {
        bail!("Deluge response omitted non-empty {key}");
    }
    Ok(value)
}

fn required_i64(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("Deluge response omitted valid {key}"))
}

fn required_nonnegative_i64(value: &Value, key: &str) -> Result<i64> {
    let value = required_i64(value, key)?;
    if value < 0 {
        bail!("Deluge response contains negative {key}");
    }
    Ok(value)
}

fn required_f64(value: &Value, key: &str) -> Result<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("Deluge response omitted valid {key}"))
}

fn required_nonnegative_f64(value: &Value, key: &str) -> Result<f64> {
    let value = required_f64(value, key)?;
    if !value.is_finite() || value < 0.0 {
        bail!("Deluge response contains invalid {key}");
    }
    Ok(value)
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

    #[test]
    fn deluge_success_results_are_checked_for_mutations() {
        assert!(require_true(Value::Bool(true), "core.remove_torrent").is_ok());
        assert!(require_true(Value::Bool(false), "core.remove_torrent").is_err());
        assert!(require_not_false(Value::Null, "core.pause_torrent").is_ok());
        assert!(require_not_false(Value::Bool(true), "core.pause_torrent").is_ok());
        assert!(require_not_false(Value::Bool(false), "core.pause_torrent").is_err());
        assert!(require_torrent_id(json!("abc"), "core.add_torrent_magnet").is_ok());
        assert!(require_torrent_id(Value::Null, "core.add_torrent_file").is_err());
    }

    #[test]
    fn deluge_single_torrent_mutations_use_singular_rpc_arguments() {
        let hash = "deadbeef";
        assert_eq!(single_torrent_params(hash), json!([hash]));
        assert_eq!(torrent_ids_params(hash), json!([[hash]]));
    }
}
