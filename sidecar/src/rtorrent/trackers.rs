use anyhow::{Context, Result};
use serde::Serialize;

use super::client::{Client, XmlValue};
use crate::torrent_meta::session_tracker_urls;

#[derive(Debug, Clone, Serialize)]
pub struct RawTracker {
    pub url: String,
    pub id: i64,
    pub group: i64,
    pub group_index: i64,
    pub is_enabled: bool,
    pub is_open: bool,
    pub is_extra_tracker: bool,
    pub activity_time_last: i64,
    pub activity_time_next: i64,
    pub min_interval: i64,
    pub normal_interval: i64,
    pub failed_counter: i64,
    pub success_counter: i64,
    pub scrape_incomplete: i64,
    pub scrape_complete: i64,
    pub scrape_downloaded: i64,
    pub message: String,
}

impl Client {
    pub async fn list_trackers(&self, hash: &str) -> Result<Vec<RawTracker>> {
        let result = self
            .call(
                "t.multicall",
                &[
                    hash.into(),
                    "".into(),
                    "t.url=".into(),
                    "t.id=".into(),
                    "t.group=".into(),
                    "t.group_index=".into(),
                    "t.is_enabled=".into(),
                    "t.is_open=".into(),
                    "t.is_extra_tracker=".into(),
                    "t.activity_time_last=".into(),
                    "t.activity_time_next=".into(),
                    "t.min_interval=".into(),
                    "t.normal_interval=".into(),
                    "t.failed_counter=".into(),
                    "t.success_counter=".into(),
                    "t.scrape_incomplete=".into(),
                    "t.scrape_complete=".into(),
                    "t.scrape_downloaded=".into(),
                    "t.message=".into(),
                ],
            )
            .await
            .with_context(|| format!("t.multicall {hash}"))?;

        let rows = result.into_array();
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let f = row.into_array();
            if f.len() < 18 {
                continue;
            }
            out.push(RawTracker {
                url: sf(&f, 0),
                id: nf(&f, 1),
                group: nf(&f, 2),
                group_index: nf(&f, 3),
                is_enabled: bf(&f, 4),
                is_open: bf(&f, 5),
                is_extra_tracker: bf(&f, 6),
                activity_time_last: nf(&f, 7),
                activity_time_next: nf(&f, 8),
                min_interval: nf(&f, 9),
                normal_interval: nf(&f, 10),
                failed_counter: nf(&f, 11),
                success_counter: nf(&f, 12),
                scrape_incomplete: nf(&f, 13),
                scrape_complete: nf(&f, 14),
                scrape_downloaded: nf(&f, 15),
                message: sf(&f, 17),
            });
        }

        if out.is_empty() {
            out = session_tracker_urls(hash)
                .into_iter()
                .enumerate()
                .map(|(idx, url)| RawTracker {
                    url,
                    id: idx as i64,
                    group: 0,
                    group_index: idx as i64,
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
                .collect();
        }
        Ok(out)
    }

    pub async fn add_tracker(&self, hash: &str, url: &str) -> Result<()> {
        self.call("d.tracker.insert", &[hash.into(), 0_i64.into(), url.into()])
            .await?;
        Ok(())
    }

    pub async fn edit_tracker(&self, hash: &str, original_url: &str, new_url: &str) -> Result<()> {
        self.call(
            "t.url.set",
            &[hash.into(), original_url.into(), new_url.into()],
        )
        .await?;
        Ok(())
    }

    pub async fn remove_tracker(&self, hash: &str, url: &str) -> Result<()> {
        // rTorrent doesn't have a direct remove; we disable it
        self.call("t.disable", &[hash.into(), url.into()]).await?;
        Ok(())
    }
}

fn sf(f: &[XmlValue], i: usize) -> String {
    f.get(i).and_then(|v| v.as_str()).unwrap_or("").to_owned()
}

fn nf(f: &[XmlValue], i: usize) -> i64 {
    f.get(i).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn bf(f: &[XmlValue], i: usize) -> bool {
    f.get(i).and_then(|v| v.as_bool()).unwrap_or(false)
}
