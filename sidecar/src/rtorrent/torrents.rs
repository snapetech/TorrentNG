use anyhow::{anyhow, bail, Context, Result};
use std::path::{Component, Path};

use super::client::{Client, XmlValue};

/// Fields queried per torrent via d.multicall2 / d.multicall.range.
const TORRENT_FIELDS: &[&str] = &[
    "d.hash=",
    "d.name=",
    "d.size_bytes=",
    "d.bytes_done=",
    "d.down.rate=",
    "d.up.rate=",
    "d.up.total=",
    "d.down.total=",
    "d.ratio=",
    "d.is_active=",
    "d.is_open=",
    "d.complete=",
    "d.state=",
    "d.priority=",
    "d.custom1=", // category
    "d.base_path=",
    "d.directory=",
    "d.creation_date=",
    "d.timestamp.finished=",
    "d.tracker_focus=",
    "d.peers_connected=",
    "d.peers_complete=",
    "d.message=",
];

pub const MULTICALL_RANGE_PAGE_SIZE: i64 = 100;

const RTORRENT_MULTICALL_RANGE_PATCH: &str = "rtorrent-0.16.11-multicall-range";
const LEGACY_RTORRENT_NONZERO_RATE_PATCH: &str = "rtorrent-0.16.11-multicall-nonzero-rate";
const LEGACY_RTORRENT_LIVE_SUMMARY_PATCH: &str = "rtorrent-0.16.11-tng-live-summary";
const RTORRENT_DEFAULT_SAVE_PATH: &str = "/downloads/temp";

#[derive(Debug, Clone)]
pub struct RawTorrent {
    pub hash: String,
    pub name: String,
    pub size_bytes: i64,
    pub bytes_done: i64,
    pub down_rate: i64,
    pub up_rate: i64,
    pub up_total: i64,
    pub down_total: i64,
    pub ratio: i64,
    pub is_active: bool,
    pub is_open: bool,
    pub complete: bool,
    pub state: i64,
    pub priority: i64,
    pub category: String,
    pub base_path: String,
    pub directory: String,
    pub creation_date: i64,
    pub timestamp_finished: i64,
    pub tracker_focus: i64,
    pub peers_connected: i64,
    pub peers_complete: i64,
    pub message: String,
    pub tracker_url: String,
    pub tags: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TransferRates {
    pub download: i64,
    pub upload: i64,
}

#[derive(Debug, Clone, Default)]
pub struct LiveSummary {
    pub rates: TransferRates,
    pub moving: Vec<RawTorrent>,
}

impl Client {
    pub async fn transfer_rates(&self) -> Result<TransferRates> {
        async fn first_available(
            client: &Client,
            methods: &[&str],
            direction: &str,
        ) -> Result<i64> {
            let mut errors = Vec::new();
            for method in methods {
                match client.call_sync(method, &[]).await {
                    Ok(value) => {
                        return value.as_i64().filter(|value| *value >= 0).ok_or_else(|| {
                            anyhow!("rTorrent {method} returned invalid {direction} rate")
                        });
                    }
                    Err(error) => errors.push(format!("{method}: {error}")),
                }
            }
            bail!(
                "rTorrent did not provide a usable {direction} rate: {}",
                errors.join("; ")
            )
        }

        let (download, upload) = tokio::join!(
            first_available(
                self,
                &["throttle.global_down.rate", "get_down_rate"],
                "download"
            ),
            first_available(self, &["throttle.global_up.rate", "get_up_rate"], "upload"),
        );

        Ok(TransferRates {
            download: download?,
            upload: upload?,
        })
    }

    pub async fn list_torrents(&self) -> Result<Vec<RawTorrent>> {
        let mut args: Vec<XmlValue> = vec!["".into(), "main".into()];
        args.extend(TORRENT_FIELDS.iter().map(|&f| XmlValue::from(f)));

        let result = self
            .call_sync("d.multicall2", &args)
            .await
            .context("d.multicall2")?;

        parse_torrent_rows(result.try_into_array()?)
    }

    pub async fn has_multicall_range(&self) -> bool {
        if rtorrent_patch_manifest_enables_bounded_live(
            &std::env::var("TNG_RTORRENT_PATCHES").unwrap_or_default(),
        ) {
            return true;
        }

        self.list_methods()
            .await
            .map(|methods| methods.iter().any(|method| method == "d.multicall.range"))
            .unwrap_or(false)
    }

    pub async fn list_torrents_range(
        &self,
        view: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<RawTorrent>> {
        let args = bounded_range_args(view, offset, limit);

        let result = self
            .call_sync("d.multicall.range", &args)
            .await
            .with_context(|| format!("d.multicall.range {view} offset={offset} limit={limit}"))?;

        parse_torrent_rows(result.try_into_array()?)
    }

    pub async fn list_torrents_nonzero_rate(
        &self,
        view: &str,
        limit: i64,
    ) -> Result<Vec<RawTorrent>> {
        let args = nonzero_rate_args(view, limit);

        let result = self
            .call_sync("d.multicall.nonzero_rate", &args)
            .await
            .with_context(|| format!("d.multicall.nonzero_rate {view} limit={limit}"))?;

        parse_torrent_rows(result.try_into_array()?)
    }

    pub async fn live_summary(&self, view: &str, limit: i64) -> Result<LiveSummary> {
        let args = live_summary_args(view, limit);

        let result = self
            .call_sync("tng.live_summary", &args)
            .await
            .with_context(|| format!("tng.live_summary {view} limit={limit}"))?;
        let mut fields = result.try_into_array()?;
        if fields.len() < 3 {
            bail!("tng.live_summary returned fewer than three fields");
        }
        let rows = fields
            .pop()
            .ok_or_else(|| anyhow!("tng.live_summary omitted torrent rows"))?
            .try_into_array()?;
        let upload = fields
            .get(1)
            .and_then(XmlValue::as_i64)
            .ok_or_else(|| anyhow!("tng.live_summary returned invalid upload rate"))?
            .max(0);
        let download = fields
            .first()
            .and_then(XmlValue::as_i64)
            .ok_or_else(|| anyhow!("tng.live_summary returned invalid download rate"))?
            .max(0);

        Ok(LiveSummary {
            rates: TransferRates { download, upload },
            moving: parse_torrent_rows(rows)?,
        })
    }

    pub async fn list_torrents_paged(&self, view: &str) -> Result<Vec<RawTorrent>> {
        if !self.has_multicall_range().await {
            return self.list_torrents().await;
        }

        let mut offset = 0i64;
        let mut torrents = Vec::new();
        loop {
            let mut page = self
                .list_torrents_range(view, offset, MULTICALL_RANGE_PAGE_SIZE)
                .await?;
            let page_len = page.len() as i64;
            torrents.append(&mut page);
            if page_len < MULTICALL_RANGE_PAGE_SIZE {
                break;
            }
            offset += page_len;
        }
        Ok(torrents)
    }

    pub async fn list_torrents_fast(&self) -> Result<Vec<RawTorrent>> {
        if self.has_multicall_range().await {
            return self
                .list_torrents_range("active", 0, MULTICALL_RANGE_PAGE_SIZE)
                .await;
        }
        self.list_torrents().await
    }

    pub async fn list_methods(&self) -> Result<Vec<String>> {
        let value = match self.call("method.list_keys", &["".into()]).await {
            Ok(v) => Ok(v),
            Err(_) => self.call("system.listMethods", &[]).await,
        }
        .context("list rTorrent XMLRPC methods")?;
        value
            .try_into_array()?
            .into_iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("rTorrent method list contained a non-string entry"))
            })
            .collect()
    }

    pub async fn load_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        let method = if start { "load.start" } else { "load.normal" };
        let save_path = normalize_rtorrent_save_path(save_path);
        let dir_cmd = format!("d.directory.set={save_path}");
        let category_cmd = format!("d.custom1.set={category}");
        let identity_cmd = self
            .tracker_peer_id()
            .map(|peer_id| format!("d.local_id.set={peer_id}"));
        let mut args: Vec<XmlValue> = vec![
            "".into(),
            magnet.into(),
            dir_cmd.as_str().into(),
            category_cmd.as_str().into(),
        ];
        if let Some(identity_cmd) = identity_cmd.as_deref() {
            args.push(identity_cmd.into());
        }
        // rTorrent executes load commands after inserting the download and
        // before load.start applies the requested started state. Assigning
        // local_id here prevents a newly-added torrent from receiving a
        // second, per-torrent identity.
        self.call_xmlrpc(method, &args).await?;
        Ok(())
    }

    pub async fn load_url(
        &self,
        url: &str,
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        if url.starts_with("magnet:") {
            return self.load_magnet(url, save_path, category, start).await;
        }
        let data = crate::backend::download_remote_torrent(url).await?;
        self.load_torrent(&data, save_path, category, start).await
    }

    pub async fn load_torrent(
        &self,
        data: &[u8],
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        let method = if start { "load.raw_start" } else { "load.raw" };
        let save_path = normalize_rtorrent_save_path(save_path);
        let dir_cmd = format!("d.directory.set={save_path}");
        let category_cmd = format!("d.custom1.set={category}");
        let identity_cmd = self
            .tracker_peer_id()
            .map(|peer_id| format!("d.local_id.set={peer_id}"));
        // rTorrent's raw loader expects XMLRPC base64, not a plain string.
        let b64 = base64_encode(data);
        let mut args: Vec<XmlValue> = vec![
            "".into(),
            XmlValue::Base64(b64),
            dir_cmd.as_str().into(),
            category_cmd.as_str().into(),
        ];
        if let Some(identity_cmd) = identity_cmd.as_deref() {
            args.push(identity_cmd.into());
        }
        self.call_xmlrpc(method, &args).await?;
        Ok(())
    }

    pub async fn start(&self, hash: &str) -> Result<()> {
        if let Some(peer_id) = self.tracker_peer_id() {
            self.call_xmlrpc("d.local_id.set", &[hash.into(), peer_id.into()])
                .await
                .context("set rTorrent download local_id before start")?;
        }
        self.call_xmlrpc("d.open", &[hash.into()]).await?;
        self.call_xmlrpc("d.resume", &[hash.into()]).await?;
        self.call_xmlrpc("d.try_start", &[hash.into()]).await?;
        self.call_xmlrpc("d.start", &[hash.into()]).await?;
        // d.resume starts a stopped download and rTorrent emits its normal
        // start announce as part of that transition. A manual announce here
        // was a second tracker request for every explicit start. The
        // dedicated reannounce endpoint below remains the only path that
        // should force an additional announce.
        Ok(())
    }

    pub async fn stop(&self, hash: &str) -> Result<()> {
        self.call_xmlrpc("d.stop", &[hash.into()]).await?;
        Ok(())
    }

    /// Same as calling stop() once per hash, but via one system.multicall
    /// round trip instead of one connection per hash -- seconds instead of
    /// minutes for a few thousand torrents. Returns one result per input
    /// hash, in the same order.
    pub async fn stop_many(&self, hashes: &[String]) -> Result<Vec<(String, Result<()>)>> {
        let results = self.call_multicall("d.stop", hashes).await?;
        Ok(hashes
            .iter()
            .cloned()
            .zip(results)
            .map(|(hash, res)| (hash, res.map(|_| ()).map_err(|e| anyhow!(e))))
            .collect())
    }

    pub async fn remove(&self, hash: &str, delete_data: bool) -> Result<()> {
        if delete_data {
            self.call("d.custom5.set", &[hash.into(), "1".into()])
                .await?;
        }
        self.call("d.erase", &[hash.into()]).await?;
        Ok(())
    }

    pub async fn recheck(&self, hash: &str) -> Result<()> {
        self.call("d.check_hash", &[hash.into()]).await?;
        Ok(())
    }

    /// Bulk equivalent of recheck() -- see stop_many().
    pub async fn recheck_many(&self, hashes: &[String]) -> Result<Vec<(String, Result<()>)>> {
        let results = self.call_multicall("d.check_hash", hashes).await?;
        Ok(hashes
            .iter()
            .cloned()
            .zip(results)
            .map(|(hash, res)| (hash, res.map(|_| ()).map_err(|e| anyhow!(e))))
            .collect())
    }

    pub async fn reannounce(&self, hash: &str) -> Result<()> {
        self.announce_trackers(hash).await?;
        Ok(())
    }

    async fn announce_trackers(&self, hash: &str) -> Result<()> {
        match self.call_xmlrpc("d.tracker_announce", &[hash.into()]).await {
            Ok(_) => Ok(()),
            Err(err) => self
                .call_xmlrpc("d.tracker_announce.force", &[hash.into()])
                .await
                .with_context(|| format!("d.tracker_announce failed first: {err}"))
                .map(|_| ()),
        }
    }

    pub async fn set_category(&self, hash: &str, category: &str) -> Result<()> {
        self.call("d.custom1.set", &[hash.into(), category.into()])
            .await?;
        Ok(())
    }

    pub async fn set_location(&self, hash: &str, location: &str) -> Result<()> {
        let location = normalize_rtorrent_save_path(location);
        self.call_xmlrpc("d.directory.set", &[hash.into(), location.into()])
            .await?;
        Ok(())
    }

    pub async fn rename_torrent(&self, hash: &str, name: &str) -> Result<()> {
        self.call("d.name.set", &[hash.into(), name.into()]).await?;
        Ok(())
    }

    pub async fn set_share_limits(
        &self,
        hash: &str,
        ratio_limit_milli: i64,
        seeding_time_limit: i64,
    ) -> Result<()> {
        self.call("d.ratio.set", &[hash.into(), ratio_limit_milli.into()])
            .await?;
        self.call(
            "d.custom2.set",
            &[hash.into(), seeding_time_limit.to_string().into()],
        )
        .await?;
        Ok(())
    }

    pub async fn toggle_sequential_download(&self, hash: &str) -> Result<()> {
        self.call("d.down.sequential.toggle", &[hash.into()])
            .await?;
        Ok(())
    }

    /// Push user_agent to rTorrent's HTTP user agent setting.
    /// Called on startup and on config change via API.
    pub async fn set_user_agent(&self, user_agent: &str) -> Result<()> {
        self.call_xmlrpc(
            "network.http.user_agent.set",
            &["".into(), user_agent.into()],
        )
        .await
        .context("set network.http.user_agent")?;
        Ok(())
    }

    /// Push a tracker-facing peer ID to every loaded rTorrent download.
    ///
    /// rTorrent stores the peer ID as each download's local_id, so existing
    /// session torrents need an explicit rewrite after package identity changes.
    pub async fn set_all_peer_ids(&self, peer_id: &str) -> Result<()> {
        if peer_id.len() != 20 || !peer_id.is_ascii() {
            anyhow::bail!("peer_id must be exactly 20 ASCII bytes");
        }

        let set_cmd = format!("d.local_id.set={peer_id}");
        self.call_xmlrpc(
            "d.multicall2",
            &[
                "".into(),
                "main".into(),
                set_cmd.as_str().into(),
                "d.save_full_session=".into(),
            ],
        )
        .await
        .context("set rTorrent download local_id values")?;
        Ok(())
    }

    /// Release the rTorrent startup gate after all loaded downloads have the
    /// resolved identity. The patched rTorrent profile keeps session torrents
    /// from resuming while this flag is zero, so no tracker announce can race
    /// the identity rewrite.
    pub async fn release_identity_gate(&self) -> Result<()> {
        self.call_xmlrpc("tng.identity_ready.set", &[1.into()])
            .await
            .context("release rTorrent tracker identity gate")?;
        self.call_xmlrpc(
            "d.multicall2",
            &[
                "".into(),
                "started".into(),
                "scheduler.simple.update=".into(),
            ],
        )
        .await
        .context("resume rTorrent downloads after identity gate")?;
        Ok(())
    }

    /// Return the current user agent rTorrent is using.
    pub async fn get_user_agent(&self) -> Result<String> {
        let v = self
            .call("network.http.user_agent", &[])
            .await
            .context("get network.http.user_agent")?;
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("rTorrent user-agent response was not a string"))
    }
}

fn bounded_range_args(view: &str, offset: i64, limit: i64) -> Vec<XmlValue> {
    let mut args: Vec<XmlValue> = vec![
        "".into(),
        view.to_owned().into(),
        offset.into(),
        limit.into(),
    ];
    args.extend(TORRENT_FIELDS.iter().map(|&f| XmlValue::from(f)));
    args
}

fn nonzero_rate_args(view: &str, limit: i64) -> Vec<XmlValue> {
    let mut args: Vec<XmlValue> = vec!["".into(), view.to_owned().into(), limit.into()];
    args.extend(TORRENT_FIELDS.iter().map(|&f| XmlValue::from(f)));
    args
}

fn live_summary_args(view: &str, limit: i64) -> Vec<XmlValue> {
    let mut args: Vec<XmlValue> = vec!["".into(), view.to_owned().into(), limit.into()];
    args.extend(TORRENT_FIELDS.iter().map(|&f| XmlValue::from(f)));
    args
}

fn rtorrent_patch_manifest_enables_bounded_live(patches: &str) -> bool {
    patches.split(',').map(str::trim).any(|patch| {
        patch == RTORRENT_MULTICALL_RANGE_PATCH
            || patch == LEGACY_RTORRENT_NONZERO_RATE_PATCH
            || patch == LEGACY_RTORRENT_LIVE_SUMMARY_PATCH
    })
}

fn parse_torrent_rows(rows: Vec<XmlValue>) -> Result<Vec<RawTorrent>> {
    let mut torrents = Vec::with_capacity(rows.len());
    for (row_index, row) in rows.into_iter().enumerate() {
        let fields = row
            .try_into_array()
            .with_context(|| format!("parse rTorrent torrent row {row_index}"))?;
        if fields.len() < TORRENT_FIELDS.len() {
            bail!(
                "rTorrent torrent row {row_index} returned {} fields, expected at least {}",
                fields.len(),
                TORRENT_FIELDS.len()
            );
        }
        torrents.push(RawTorrent {
            hash: required_string_field(&fields, 0, "d.hash")?,
            name: required_string_field(&fields, 1, "d.name")?,
            size_bytes: required_i64_field(&fields, 2, "d.size_bytes")?,
            bytes_done: required_i64_field(&fields, 3, "d.bytes_done")?,
            down_rate: required_i64_field(&fields, 4, "d.down.rate")?,
            up_rate: required_i64_field(&fields, 5, "d.up.rate")?,
            up_total: required_i64_field(&fields, 6, "d.up.total")?,
            down_total: required_i64_field(&fields, 7, "d.down.total")?,
            ratio: required_i64_field(&fields, 8, "d.ratio")?,
            is_active: required_bool_field(&fields, 9, "d.is_active")?,
            is_open: required_bool_field(&fields, 10, "d.is_open")?,
            complete: required_bool_field(&fields, 11, "d.complete")?,
            state: required_i64_field(&fields, 12, "d.state")?,
            priority: required_i64_field(&fields, 13, "d.priority")?,
            category: decode_legacy_category(required_string_field(&fields, 14, "d.custom1")?),
            base_path: required_string_field(&fields, 15, "d.base_path")?,
            directory: required_string_field(&fields, 16, "d.directory")?,
            creation_date: required_i64_field(&fields, 17, "d.creation_date")?,
            timestamp_finished: required_i64_field(&fields, 18, "d.timestamp.finished")?,
            tracker_focus: required_i64_field(&fields, 19, "d.tracker_focus")?,
            peers_connected: required_i64_field(&fields, 20, "d.peers_connected")?,
            peers_complete: required_i64_field(&fields, 21, "d.peers_complete")?,
            message: required_string_field(&fields, 22, "d.message")?,
            tracker_url: String::new(),
            tags: String::new(),
        });
    }
    Ok(torrents)
}

/// Classic ruTorrent stores the `label`/category value in `d.custom1` using
/// PHP's `rawurlencode()` so commas and other separator characters used
/// elsewhere in the field don't collide with real content. TorrentNG reads
/// `d.custom1` raw, so a library migrated from ruTorrent shows categories
/// like `linux%20iso` instead of `linux iso`. Decode defensively: only when
/// the value actually contains a `%XX` escape and decodes to valid UTF-8, so
/// a category that legitimately contains a literal `%` is left alone.
fn decode_legacy_category(raw: String) -> String {
    if !raw.contains('%') {
        return raw;
    }
    match urlencoding::decode(&raw) {
        Ok(decoded) if decoded != raw => decoded.into_owned(),
        _ => raw,
    }
}

fn required_string_field(fields: &[XmlValue], i: usize, name: &str) -> Result<String> {
    fields
        .get(i)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("rTorrent response omitted valid {name}"))
}

fn required_i64_field(fields: &[XmlValue], i: usize, name: &str) -> Result<i64> {
    fields
        .get(i)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("rTorrent response omitted valid {name}"))
}

fn required_bool_field(fields: &[XmlValue], i: usize, name: &str) -> Result<bool> {
    fields
        .get(i)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| anyhow!("rTorrent response omitted valid {name}"))
}

fn normalize_rtorrent_save_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "." {
        return RTORRENT_DEFAULT_SAVE_PATH.to_owned();
    }
    if trimmed.starts_with('/') {
        return trimmed.trim_end_matches('/').to_owned();
    }

    let mut parts = Vec::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(part) => {
                if let Some(part) = part.to_str() {
                    if !part.is_empty() {
                        parts.push(part);
                    }
                }
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {}
        }
    }

    if parts.is_empty() {
        RTORRENT_DEFAULT_SAVE_PATH.to_owned()
    } else {
        format!("{}/{}", RTORRENT_DEFAULT_SAVE_PATH, parts.join("/"))
    }
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 {
            chunk[1] as usize
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as usize
        } else {
            0
        };
        out.push(CHARS[b0 >> 2] as char);
        out.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((b1 & 0xf) << 2) | (b2 >> 6)] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[b2 & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_range_args, decode_legacy_category, live_summary_args, nonzero_rate_args,
        normalize_rtorrent_save_path, rtorrent_patch_manifest_enables_bounded_live,
    };
    use crate::rtorrent::XmlValue;

    #[test]
    fn decode_legacy_category_decodes_rutorrent_style_encoding() {
        assert_eq!(
            decode_legacy_category("linux%20iso".to_owned()),
            "linux iso"
        );
        assert_eq!(decode_legacy_category("books".to_owned()), "books");
        // A literal, non-escape '%' is left alone rather than mangled.
        assert_eq!(decode_legacy_category("100% done".to_owned()), "100% done");
    }

    #[test]
    fn multicall_range_patch_enables_bounded_live_features() {
        assert!(rtorrent_patch_manifest_enables_bounded_live(
            "rtorrent-0.16.11-user-agent-command,rtorrent-0.16.11-multicall-range"
        ));
    }

    #[test]
    fn legacy_split_feature_patch_names_remain_accepted() {
        assert!(rtorrent_patch_manifest_enables_bounded_live(
            "rtorrent-0.16.11-multicall-nonzero-rate"
        ));
        assert!(rtorrent_patch_manifest_enables_bounded_live(
            "rtorrent-0.16.11-tng-live-summary"
        ));
    }

    #[test]
    fn unrelated_patch_manifest_does_not_enable_bounded_live_features() {
        assert!(!rtorrent_patch_manifest_enables_bounded_live(
            "rtorrent-0.16.11-user-agent-command"
        ));
    }

    #[test]
    fn bounded_multicall_args_keep_required_rtorrent_target() {
        let args = bounded_range_args("main", 25, 100);

        assert_eq!(str_arg(&args, 0), "");
        assert_eq!(str_arg(&args, 1), "main");
        assert_eq!(int_arg(&args, 2), 25);
        assert_eq!(int_arg(&args, 3), 100);
        assert_eq!(str_arg(&args, 4), "d.hash=");
    }

    #[test]
    fn live_summary_args_keep_required_rtorrent_target() {
        let args = live_summary_args("main", 100);

        assert_eq!(str_arg(&args, 0), "");
        assert_eq!(str_arg(&args, 1), "main");
        assert_eq!(int_arg(&args, 2), 100);
        assert_eq!(str_arg(&args, 3), "d.hash=");
    }

    #[test]
    fn nonzero_rate_args_keep_required_rtorrent_target() {
        let args = nonzero_rate_args("active", 50);

        assert_eq!(str_arg(&args, 0), "");
        assert_eq!(str_arg(&args, 1), "active");
        assert_eq!(int_arg(&args, 2), 50);
        assert_eq!(str_arg(&args, 3), "d.hash=");
    }

    #[test]
    fn rtorrent_peer_id_default_is_valid() {
        assert_eq!(crate::config::DEFAULT_PEER_ID.len(), 20);
        assert!(crate::config::DEFAULT_PEER_ID.is_ascii());
        assert!(crate::config::DEFAULT_PEER_ID.starts_with("-lt100B-"));
    }

    #[test]
    fn normalizes_relative_rtorrent_save_paths_under_writable_download_root() {
        assert_eq!(normalize_rtorrent_save_path(""), "/downloads/temp");
        assert_eq!(normalize_rtorrent_save_path("."), "/downloads/temp");
        assert_eq!(
            normalize_rtorrent_save_path("./Movie.Name.2026"),
            "/downloads/temp/Movie.Name.2026"
        );
        assert_eq!(
            normalize_rtorrent_save_path("Movie.Name.2026"),
            "/downloads/temp/Movie.Name.2026"
        );
        assert_eq!(
            normalize_rtorrent_save_path("../Movie.Name.2026"),
            "/downloads/temp/Movie.Name.2026"
        );
        assert_eq!(
            normalize_rtorrent_save_path("/downloads/download/movies"),
            "/downloads/download/movies"
        );
    }

    fn str_arg(args: &[XmlValue], index: usize) -> &str {
        args.get(index).and_then(XmlValue::as_str).unwrap()
    }

    fn int_arg(args: &[XmlValue], index: usize) -> i64 {
        args.get(index).and_then(XmlValue::as_i64).unwrap()
    }
}
