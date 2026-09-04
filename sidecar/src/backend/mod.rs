use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::{redirect::Policy, Url};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use crate::rtorrent::files::RawFile;
use crate::rtorrent::torrents::{LiveSummary, RawTorrent};
use crate::rtorrent::trackers::RawTracker;
use crate::rtorrent::TransferRates;

const MAX_REMOTE_TORRENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_BACKEND_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const REMOTE_TORRENT_TIMEOUT: Duration = Duration::from_secs(30);
const WEBHOOK_RESPONSE_BYTES: usize = 64 * 1024;
const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// Download a user-supplied torrent URL without turning the sidecar into an
/// unrestricted SSRF or unbounded-body proxy. A single validated DNS result
/// is pinned into the client and redirects are rejected rather than followed
/// into a second, unvalidated address.
pub(crate) async fn download_remote_torrent(url: &str) -> Result<Vec<u8>> {
    let (client, parsed) =
        bounded_remote_client(url, false, REMOTE_TORRENT_TIMEOUT, "torrent URL").await?;
    let mut response = client
        .get(parsed)
        .send()
        .await
        .context("download torrent URL")?
        .error_for_status()
        .context("download torrent URL")?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_REMOTE_TORRENT_BYTES as u64)
    {
        bail!("torrent URL response exceeds {MAX_REMOTE_TORRENT_BYTES} byte limit");
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .map(|length| length.min(MAX_REMOTE_TORRENT_BYTES as u64) as usize)
            .unwrap_or_default(),
    );
    while let Some(chunk) = response.chunk().await.context("read torrent URL body")? {
        if body.len().saturating_add(chunk.len()) > MAX_REMOTE_TORRENT_BYTES {
            bail!("torrent URL response exceeds {MAX_REMOTE_TORRENT_BYTES} byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// POST a workflow webhook through the same address-pinned, no-redirect
/// egress boundary as remote torrent downloads. Private destinations are only
/// available when an operator explicitly enables them in configuration.
pub(crate) async fn post_remote_json(
    url: &str,
    payload: &serde_json::Value,
    allow_private: bool,
) -> Result<()> {
    let (client, parsed) =
        bounded_remote_client(url, allow_private, WEBHOOK_TIMEOUT, "workflow webhook URL").await?;
    let response = client
        .post(parsed)
        .json(payload)
        .send()
        .await
        .context("send workflow webhook")?;
    response_bytes_bounded(response, WEBHOOK_RESPONSE_BYTES, "workflow webhook").await?;
    Ok(())
}

async fn bounded_remote_client(
    url: &str,
    allow_private: bool,
    timeout: Duration,
    context: &str,
) -> Result<(reqwest::Client, Url)> {
    let parsed = Url::parse(url).with_context(|| format!("parse {context}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("{context} must use http or https");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("{context} credentials are not allowed");
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("{context} has no host"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("{context} has no usable port"))?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("resolve {context} host"))?
        .collect::<Vec<_>>();
    let address = addresses
        .into_iter()
        .find(|address| {
            if allow_private {
                is_valid_unicast(address.ip())
            } else {
                is_public_unicast(address.ip())
            }
        })
        .ok_or_else(|| {
            if allow_private {
                anyhow::anyhow!("{context} resolves only to unusable addresses")
            } else {
                anyhow::anyhow!(
                    "{context} resolves only to private or local addresses; set allow_private_webhooks to opt in"
                )
            }
        })?;

    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(timeout)
        .resolve(host, address)
        .build()
        .with_context(|| format!("create bounded {context} client"))?;
    Ok((client, parsed))
}

fn is_valid_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !ip.is_unspecified() && !ip.is_broadcast() && !ip.is_multicast(),
        IpAddr::V6(ip) => !ip.is_unspecified() && !ip.is_multicast(),
    }
}

fn is_public_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_private()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_multicast()
                && !(ip.octets()[0] == 0)
                && !(ip.octets()[0] == 100 && (ip.octets()[1] & 0b1100_0000) == 0b0100_0000)
                && !(ip.octets()[0] == 192 && ip.octets()[1] == 0)
                && !(ip.octets()[0] == 198 && (ip.octets()[1] == 18 || ip.octets()[1] == 19))
                && !(ip.octets()[0] == 198 && ip.octets()[1] == 51 && ip.octets()[2] == 100)
                && !(ip.octets()[0] == 203 && ip.octets()[1] == 0 && ip.octets()[2] == 113)
                && ip.octets()[0] < 240
        }
        IpAddr::V6(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_unique_local()
                && !ip.is_unicast_link_local()
                && !(ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8)
                && ip.to_ipv4_mapped().is_none()
        }
    }
}

pub(crate) async fn response_bytes_bounded(
    response: reqwest::Response,
    max_bytes: usize,
    context: &str,
) -> Result<Vec<u8>> {
    let mut response = response
        .error_for_status()
        .with_context(|| context.to_owned())?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        bail!("{context} response exceeds {max_bytes} byte limit");
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .map(|length| length.min(max_bytes as u64) as usize)
            .unwrap_or_default(),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("read {context} response body"))?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            bail!("{context} response exceeds {max_bytes} byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) async fn response_json_bounded<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    max_bytes: usize,
    context: &str,
) -> Result<T> {
    let body = response_bytes_bounded(response, max_bytes, context).await?;
    serde_json::from_slice(&body).with_context(|| format!("decode {context} response"))
}

/// qBittorrent reports several mutation failures as HTTP 200 with the literal
/// `Fails.` body. Keep all qBittorrent-shaped upstream calls on the same
/// fail-closed contract, including the native TorrentNG compatibility route.
pub(crate) fn validate_qbit_mutation_body(body: &[u8], context: &str) -> Result<()> {
    let text = std::str::from_utf8(body)
        .with_context(|| format!("{context} returned invalid UTF-8"))?
        .trim();
    if text.is_empty() || text == "Ok." {
        Ok(())
    } else {
        bail!("{context} returned unexpected mutation body {text:?}")
    }
}

pub(crate) const MAX_BACKEND_JSON_BYTES: usize = MAX_BACKEND_RESPONSE_BYTES;

/// Convert an externally supplied floating-point ratio into the fixed-point
/// representation used by the compatibility model. Malformed or negative
/// backend values are data errors, not valid negative ratios; map them to the
/// safe zero value instead of allowing a saturating Rust cast to hide them.
pub(crate) fn ratio_milli(value: Option<f64>) -> i64 {
    let Some(value) = value.filter(|value| value.is_finite() && *value >= 0.0) else {
        return 0;
    };
    let scaled = value * 1000.0;
    if scaled >= i64::MAX as f64 {
        i64::MAX
    } else {
        scaled as i64
    }
}

#[cfg(test)]
mod tests {
    use super::{is_public_unicast, is_valid_unicast, ratio_milli};
    use std::net::IpAddr;

    #[test]
    fn remote_address_policy_rejects_local_and_reserved_targets() {
        for value in [
            "0.0.0.0",
            "10.0.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "192.168.1.1",
            "192.0.2.1",
            "192.0.0.1",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            let ip: IpAddr = value.parse().unwrap();
            assert!(!is_public_unicast(ip), "{value} must not be public");
        }
    }

    #[test]
    fn explicit_private_webhook_policy_allows_unicast_only() {
        assert!(is_valid_unicast("127.0.0.1".parse().unwrap()));
        assert!(is_valid_unicast("192.168.1.1".parse().unwrap()));
        assert!(!is_valid_unicast("0.0.0.0".parse().unwrap()));
        assert!(!is_valid_unicast("224.0.0.1".parse().unwrap()));
    }

    #[test]
    fn ratio_conversion_rejects_garbage_and_saturates_huge_values() {
        assert_eq!(ratio_milli(Some(1.25)), 1_250);
        assert_eq!(ratio_milli(None), 0);
        assert_eq!(ratio_milli(Some(-1.0)), 0);
        assert_eq!(ratio_milli(Some(f64::NAN)), 0);
        assert_eq!(ratio_milli(Some(f64::MAX)), i64::MAX);
    }
}

pub mod deluge;
pub mod qbittorrent;
pub mod rtorrent;
pub mod torrentng;
pub mod transmission;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendType {
    Rtorrent,
    Qbittorrent,
    Transmission,
    Deluge,
    Torrentng,
}

impl BackendType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rtorrent => "rtorrent",
            Self::Qbittorrent => "qbittorrent",
            Self::Transmission => "transmission",
            Self::Deluge => "deluge",
            Self::Torrentng => "torrentng",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendStatus {
    Connected,
    Unreachable,
}

impl BackendStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Unreachable => "unreachable",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BackendHealth {
    #[serde(rename = "type")]
    pub backend_type: &'static str,
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackendCapabilities {
    pub supports_tags: bool,
    pub supports_categories: bool,
    pub supports_file_priority: bool,
    pub supports_tracker_edit: bool,
    pub supports_recheck: bool,
    pub supports_torrent_export: bool,
    pub supports_webseed_reads: bool,
    pub supports_piece_state_reads: bool,
    pub supports_piece_hash_reads: bool,
    pub supports_peer_snapshots: bool,
    pub supports_peer_add: bool,
    pub supports_peer_ban: bool,
    pub supports_queue_order: bool,
    pub supports_per_torrent_limits: bool,
    pub supports_global_limits: bool,
    pub supports_share_limits: bool,
    pub supports_mode_flags: bool,
    pub supports_location_update: bool,
    pub supports_torrent_rename: bool,
    pub supports_file_rename: bool,
    pub supports_runtime_user_agent: bool,
    pub supports_config_overlay: bool,
    pub supports_restart: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BackendTransferLimits {
    pub download_limit: i64,
    pub upload_limit: i64,
    pub speed_limits_mode: bool,
}

#[async_trait]
pub trait TorrentBackend: Send + Sync {
    fn backend_type(&self) -> BackendType;
    fn capabilities(&self) -> BackendCapabilities;

    async fn health(&self) -> BackendStatus;
    async fn transfer_rates(&self) -> Result<TransferRates>;
    async fn list_torrents(&self) -> Result<Vec<RawTorrent>>;
    async fn add_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()>;
    async fn add_torrent(
        &self,
        data: &[u8],
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()>;
    async fn torrent_blob(&self, _hash: &str) -> Result<Vec<u8>> {
        bail!(
            "{} backend does not support torrent export",
            self.backend_type().as_str()
        )
    }
    async fn add_url(&self, url: &str, save_path: &str, category: &str, start: bool) -> Result<()> {
        if url.starts_with("magnet:") {
            return self.add_magnet(url, save_path, category, start).await;
        }
        let data = download_remote_torrent(url).await?;
        self.add_torrent(&data, save_path, category, start).await
    }
    async fn remove(&self, hash: &str, delete_data: bool) -> Result<()>;
    async fn start(&self, hash: &str) -> Result<()>;
    async fn stop(&self, hash: &str) -> Result<()>;
    async fn recheck(&self, hash: &str) -> Result<()>;
    async fn reannounce(&self, hash: &str) -> Result<()>;

    /// Bulk-optimized stop for backends that can process many hashes in one
    /// round trip (e.g. rTorrent's system.multicall). `None` means this
    /// backend has no such fast path -- the caller falls back to calling
    /// stop() once per hash. `Some` always covers every input hash, one
    /// result each, in the same order.
    async fn stop_many(&self, _hashes: &[String]) -> Option<Result<Vec<(String, Result<()>)>>> {
        None
    }

    /// Bulk-optimized recheck -- see stop_many().
    async fn recheck_many(&self, _hashes: &[String]) -> Option<Result<Vec<(String, Result<()>)>>> {
        None
    }
    async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>>;
    async fn add_tracker(&self, hash: &str, url: &str) -> Result<()>;
    async fn edit_tracker(&self, hash: &str, original_url: &str, new_url: &str) -> Result<()>;
    async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()>;
    async fn list_files(&self, hash: &str) -> Result<Vec<RawFile>>;
    async fn list_webseeds(&self, _hash: &str) -> Result<Vec<String>> {
        bail!(
            "{} backend does not support webseed reads",
            self.backend_type().as_str()
        )
    }

    async fn piece_states(&self, _hash: &str) -> Result<Vec<BackendPieceState>> {
        bail!(
            "{} backend does not support piece state reads",
            self.backend_type().as_str()
        )
    }

    async fn piece_hashes(&self, _hash: &str) -> Result<Vec<String>> {
        bail!(
            "{} backend does not support piece hash reads",
            self.backend_type().as_str()
        )
    }

    async fn list_peers(&self, _hash: &str) -> Result<Vec<BackendPeer>> {
        bail!(
            "{} backend does not support peer snapshot reads",
            self.backend_type().as_str()
        )
    }

    async fn set_file_priority(&self, hash: &str, file_index: usize, priority: i64) -> Result<()>;
    async fn set_category(&self, hash: &str, category: &str) -> Result<()>;
    async fn set_location(&self, _hash: &str, _location: &str) -> Result<()> {
        bail!(
            "{} backend does not support location updates",
            self.backend_type().as_str()
        )
    }
    async fn rename_torrent(&self, _hash: &str, _name: &str) -> Result<()> {
        bail!(
            "{} backend does not support torrent renames",
            self.backend_type().as_str()
        )
    }
    async fn rename_file(&self, _hash: &str, _file_index: usize, _name: &str) -> Result<()> {
        bail!(
            "{} backend does not support file renames",
            self.backend_type().as_str()
        )
    }
    async fn set_share_limits(
        &self,
        _hash: &str,
        _ratio_limit_milli: i64,
        _seeding_time_limit: i64,
    ) -> Result<()> {
        bail!(
            "{} backend does not support share limits",
            self.backend_type().as_str()
        )
    }
    async fn set_download_limit(&self, _hash: &str, _limit: Option<i64>) -> Result<()> {
        bail!(
            "{} backend does not support per-torrent download limits",
            self.backend_type().as_str()
        )
    }

    async fn set_upload_limit(&self, _hash: &str, _limit: Option<i64>) -> Result<()> {
        bail!(
            "{} backend does not support per-torrent upload limits",
            self.backend_type().as_str()
        )
    }

    async fn download_limits(&self, _hashes: &[String]) -> Result<BTreeMap<String, i64>> {
        bail!(
            "{} backend does not support per-torrent download limit reads",
            self.backend_type().as_str()
        )
    }

    async fn upload_limits(&self, _hashes: &[String]) -> Result<BTreeMap<String, i64>> {
        bail!(
            "{} backend does not support per-torrent upload limit reads",
            self.backend_type().as_str()
        )
    }

    async fn set_global_download_limit(&self, _limit: i64) -> Result<()> {
        bail!(
            "{} backend does not support global download limits",
            self.backend_type().as_str()
        )
    }

    async fn global_limits(&self) -> Result<BackendTransferLimits> {
        bail!(
            "{} backend does not support global transfer limit reads",
            self.backend_type().as_str()
        )
    }

    async fn set_global_upload_limit(&self, _limit: i64) -> Result<()> {
        bail!(
            "{} backend does not support global upload limits",
            self.backend_type().as_str()
        )
    }

    async fn toggle_global_speed_limits_mode(&self) -> Result<()> {
        bail!(
            "{} backend does not support global speed-limit mode toggles",
            self.backend_type().as_str()
        )
    }

    async fn toggle_sequential_download(&self, _hash: &str) -> Result<()> {
        bail!(
            "{} backend does not support sequential download toggles",
            self.backend_type().as_str()
        )
    }

    async fn toggle_first_last_piece_priority(&self, _hash: &str) -> Result<()> {
        bail!(
            "{} backend does not support first/last piece priority toggles",
            self.backend_type().as_str()
        )
    }

    async fn set_force_start(&self, _hash: &str, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support force-start updates",
            self.backend_type().as_str()
        )
    }

    async fn set_super_seeding(&self, _hash: &str, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support super-seeding updates",
            self.backend_type().as_str()
        )
    }

    async fn set_auto_tmm(&self, _hash: &str, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support automatic torrent management updates",
            self.backend_type().as_str()
        )
    }

    async fn set_auto_management(&self, _hash: &str, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support automatic management updates",
            self.backend_type().as_str()
        )
    }

    async fn add_peers(&self, _hash: &str, _peers: &[SocketAddr]) -> Result<()> {
        bail!(
            "{} backend does not support explicit peer adds",
            self.backend_type().as_str()
        )
    }

    async fn update_queue_order(&self, _hashes: &[String], _queue_move: QueueMove) -> Result<()> {
        bail!(
            "{} backend does not support queue order updates",
            self.backend_type().as_str()
        )
    }

    async fn ban_peers(&self, _peers: &[SocketAddr]) -> Result<()> {
        bail!(
            "{} backend does not support peer bans",
            self.backend_type().as_str()
        )
    }

    async fn add_tags(&self, _hash: &str, _tags: &[&str]) -> Result<()> {
        bail!(
            "{} backend does not support tag additions",
            self.backend_type().as_str()
        )
    }
    async fn remove_tags(&self, _hash: &str, _tags: &[&str]) -> Result<()> {
        bail!(
            "{} backend does not support tag removals",
            self.backend_type().as_str()
        )
    }
    async fn set_tags(&self, _hash: &str, _tags: &[&str]) -> Result<()> {
        bail!(
            "{} backend does not support tag replacement",
            self.backend_type().as_str()
        )
    }

    /// Whether `list_torrents_range` is a genuinely bounded upstream read.
    /// The default is false because the legacy fallback calls the full-list
    /// endpoint; callers must not advertise pagination by slicing that result
    /// locally.
    async fn has_bounded_sync(&self) -> bool {
        false
    }

    async fn list_torrents_range(
        &self,
        _view: &str,
        _offset: i64,
        _limit: i64,
    ) -> Result<Vec<RawTorrent>> {
        self.list_torrents().await
    }

    /// Fetch one page, optionally pinned to a backend-provided immutable
    /// snapshot. Implementations that return `None` are eventually consistent;
    /// the legacy full-list fallback is not a bounded page and must keep
    /// `has_bounded_sync()` false.
    async fn list_torrents_range_with_snapshot(
        &self,
        view: &str,
        offset: i64,
        limit: i64,
        _snapshot: Option<u64>,
    ) -> Result<(Vec<RawTorrent>, Option<u64>)> {
        Ok((self.list_torrents_range(view, offset, limit).await?, None))
    }

    async fn live_summary(&self, _view: &str, _limit: i64) -> Result<LiveSummary> {
        Ok(LiveSummary {
            rates: self.transfer_rates().await?,
            moving: Vec::new(),
        })
    }

    async fn feature_status(&self) -> (String, String) {
        ("unknown".to_owned(), "unknown".to_owned())
    }

    async fn set_dht(&self, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support runtime DHT toggles",
            self.backend_type().as_str()
        )
    }

    async fn set_pex(&self, _enabled: bool) -> Result<()> {
        bail!(
            "{} backend does not support runtime peer exchange toggles",
            self.backend_type().as_str()
        )
    }

    async fn get_user_agent(&self) -> Result<String> {
        bail!(
            "{} backend does not support runtime user-agent reads",
            self.backend_type().as_str()
        )
    }

    async fn set_user_agent(&self, _user_agent: &str) -> Result<()> {
        bail!(
            "{} backend does not support runtime user-agent updates",
            self.backend_type().as_str()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueMove {
    Up,
    Down,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendPieceState {
    Missing,
    Partial,
    Complete,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackendPeer {
    pub addr: SocketAddr,
    pub client: String,
    pub progress: f64,
    pub download_rate: i64,
    pub upload_rate: i64,
    pub downloaded: i64,
    pub uploaded: i64,
}

/// qBittorrent-compatible peer snapshots are used by both the native API
/// adapter and the qBittorrent adapter. Treat a malformed snapshot as a
/// backend error instead of silently projecting it as an empty peer list.
pub(crate) fn parse_qbit_peer_response(response: &serde_json::Value) -> Result<Vec<BackendPeer>> {
    let peers = response
        .get("peers")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("qBittorrent peer response has no peers object"))?;
    peers
        .iter()
        .map(|(key, peer)| parse_qbit_peer(key, peer))
        .collect()
}

fn parse_qbit_peer(key: &str, peer: &serde_json::Value) -> Result<BackendPeer> {
    let peer_object = peer
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("qBittorrent peer {key:?} is not an object"))?;
    let addr = if let Some(ip) = peer_object.get("ip") {
        let ip = ip
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("qBittorrent peer {key:?} has an invalid ip"))?
            .parse::<IpAddr>()
            .with_context(|| format!("parse qBittorrent peer {key:?} ip"))?;
        let port = peer_object
            .get("port")
            .and_then(serde_json::Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0)
            .ok_or_else(|| anyhow::anyhow!("qBittorrent peer {key:?} has an invalid port"))?;
        SocketAddr::new(ip, port)
    } else {
        key.parse::<SocketAddr>()
            .with_context(|| format!("parse qBittorrent peer address {key:?}"))?
    };
    let client = peer_object
        .get("client")
        .or_else(|| peer_object.get("peer_id_client"))
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("qBittorrent peer {key:?} has an invalid client"))
        })
        .transpose()?
        .unwrap_or_default();
    let progress = peer_object
        .get("progress")
        .or_else(|| peer_object.get("relevance"))
        .map(|value| {
            let progress = value.as_f64().ok_or_else(|| {
                anyhow::anyhow!("qBittorrent peer {key:?} has an invalid progress")
            })?;
            if !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
                bail!("qBittorrent peer {key:?} has an out-of-range progress")
            }
            Ok(progress)
        })
        .transpose()?
        .unwrap_or(0.0);
    Ok(BackendPeer {
        addr,
        client,
        progress,
        download_rate: qbit_peer_counter(peer_object, key, "dl_speed")?,
        upload_rate: qbit_peer_counter(peer_object, key, "up_speed")?,
        downloaded: qbit_peer_counter(peer_object, key, "downloaded")?,
        uploaded: qbit_peer_counter(peer_object, key, "uploaded")?,
    })
}

fn qbit_peer_counter(
    peer: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    field: &str,
) -> Result<i64> {
    match peer.get(field) {
        None => Ok(0),
        Some(value) => value
            .as_i64()
            .filter(|value| *value >= 0)
            .ok_or_else(|| anyhow::anyhow!("qBittorrent peer {key:?} has an invalid {field}")),
    }
}

pub(crate) fn map_qbit_piece_state(state: i64) -> Result<BackendPieceState> {
    match state {
        0 => Ok(BackendPieceState::Missing),
        1 => Ok(BackendPieceState::Partial),
        2 => Ok(BackendPieceState::Complete),
        other => bail!("qBittorrent pieceStates returned unknown state {other}"),
    }
}
