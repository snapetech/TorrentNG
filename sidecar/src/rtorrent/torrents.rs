use anyhow::{Context, Result};

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

pub const MULTICALL_RANGE_PAGE_SIZE: i64 = 500;

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
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TransferRates {
    pub download: i64,
    pub upload: i64,
}

impl Client {
    pub async fn transfer_rates(&self) -> Result<TransferRates> {
        async fn first_available(client: &Client, methods: &[&str]) -> i64 {
            for method in methods {
                if let Ok(value) = client.call(method, &[]).await {
                    return value.as_i64().unwrap_or(0).max(0);
                }
            }
            0
        }

        let (download, upload) = tokio::join!(
            first_available(self, &["throttle.global_down.rate", "get_down_rate"]),
            first_available(self, &["throttle.global_up.rate", "get_up_rate"]),
        );

        Ok(TransferRates { download, upload })
    }

    pub async fn list_torrents(&self) -> Result<Vec<RawTorrent>> {
        let mut args: Vec<XmlValue> = vec!["".into(), "main".into()];
        args.extend(TORRENT_FIELDS.iter().map(|&f| XmlValue::from(f)));

        let result = self
            .call("d.multicall2", &args)
            .await
            .context("d.multicall2")?;

        parse_torrent_rows(result.into_array())
    }

    pub async fn has_multicall_range(&self) -> bool {
        if std::env::var("RTNG_RTORRENT_PATCHES")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .any(|patch| patch == "rtorrent-0.16.11-multicall-range")
        {
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
        let mut args: Vec<XmlValue> = vec![
            "".into(),
            view.to_owned().into(),
            offset.into(),
            limit.into(),
        ];
        args.extend(TORRENT_FIELDS.iter().map(|&f| XmlValue::from(f)));

        let result = self
            .call("d.multicall.range", &args)
            .await
            .with_context(|| format!("d.multicall.range {view} offset={offset} limit={limit}"))?;

        parse_torrent_rows(result.into_array())
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
        Ok(value
            .into_array()
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect())
    }

    pub async fn load_magnet(
        &self,
        magnet: &str,
        save_path: &str,
        category: &str,
        start: bool,
    ) -> Result<()> {
        let method = if start { "load.start" } else { "load.normal" };
        let dir_cmd = format!("d.directory.set={save_path}");
        let category_cmd = format!("d.custom1.set={category}");
        self.call(
            method,
            &[
                "".into(),
                magnet.into(),
                dir_cmd.as_str().into(),
                category_cmd.as_str().into(),
            ],
        )
        .await?;
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
        let data = reqwest::get(url)
            .await
            .with_context(|| format!("fetch torrent URL {url}"))?
            .error_for_status()
            .with_context(|| format!("fetch torrent URL {url}"))?
            .bytes()
            .await
            .with_context(|| format!("read torrent URL {url}"))?;
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
        let dir_cmd = format!("d.directory.set={save_path}");
        let category_cmd = format!("d.custom1.set={category}");
        // rTorrent's raw loader expects XMLRPC base64, not a plain string.
        let b64 = base64_encode(data);
        self.call(
            method,
            &[
                "".into(),
                XmlValue::Base64(b64),
                dir_cmd.as_str().into(),
                category_cmd.as_str().into(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn start(&self, hash: &str) -> Result<()> {
        self.call("d.start", &[hash.into()]).await?;
        Ok(())
    }

    pub async fn stop(&self, hash: &str) -> Result<()> {
        self.call("d.stop", &[hash.into()]).await?;
        Ok(())
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

    pub async fn reannounce(&self, hash: &str) -> Result<()> {
        self.call("d.tracker_announce", &[hash.into()]).await?;
        Ok(())
    }

    pub async fn set_category(&self, hash: &str, category: &str) -> Result<()> {
        self.call("d.custom1.set", &[hash.into(), category.into()])
            .await?;
        Ok(())
    }

    pub async fn set_location(&self, hash: &str, location: &str) -> Result<()> {
        self.call("d.directory.set", &[hash.into(), location.into()])
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
        self.call(
            "network.http.user_agent.set",
            &["".into(), user_agent.into()],
        )
        .await
        .context("set network.http.user_agent")?;
        Ok(())
    }

    /// Return the current user agent rTorrent is using.
    pub async fn get_user_agent(&self) -> Result<String> {
        let v = self
            .call("network.http.user_agent", &[])
            .await
            .context("get network.http.user_agent")?;
        Ok(v.as_str().unwrap_or("").to_owned())
    }
}

fn parse_torrent_rows(rows: Vec<XmlValue>) -> Result<Vec<RawTorrent>> {
    let mut torrents = Vec::with_capacity(rows.len());
    for row in rows {
        let fields = row.into_array();
        if fields.len() < TORRENT_FIELDS.len() {
            continue;
        }
        torrents.push(RawTorrent {
            hash: str_field(&fields, 0),
            name: str_field(&fields, 1),
            size_bytes: int_field(&fields, 2),
            bytes_done: int_field(&fields, 3),
            down_rate: int_field(&fields, 4),
            up_rate: int_field(&fields, 5),
            up_total: int_field(&fields, 6),
            down_total: int_field(&fields, 7),
            ratio: int_field(&fields, 8),
            is_active: bool_field(&fields, 9),
            is_open: bool_field(&fields, 10),
            complete: bool_field(&fields, 11),
            state: int_field(&fields, 12),
            priority: int_field(&fields, 13),
            category: str_field(&fields, 14),
            base_path: str_field(&fields, 15),
            directory: str_field(&fields, 16),
            creation_date: int_field(&fields, 17),
            timestamp_finished: int_field(&fields, 18),
            tracker_focus: int_field(&fields, 19),
            peers_connected: int_field(&fields, 20),
            peers_complete: int_field(&fields, 21),
            message: str_field(&fields, 22),
            tracker_url: String::new(),
        });
    }
    Ok(torrents)
}

fn str_field(fields: &[XmlValue], i: usize) -> String {
    fields
        .get(i)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned()
}
fn int_field(fields: &[XmlValue], i: usize) -> i64 {
    fields.get(i).and_then(|v| v.as_i64()).unwrap_or(0)
}
fn bool_field(fields: &[XmlValue], i: usize) -> bool {
    fields.get(i).and_then(|v| v.as_bool()).unwrap_or(false)
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
