use anyhow::{anyhow, bail, Result};
use serde::Serialize;

use super::client::{Client, XmlValue};

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
                ],
            )
            .await?;

        parse_tracker_rows(result.try_into_array()?)
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

fn parse_tracker_rows(rows: Vec<XmlValue>) -> Result<Vec<RawTracker>> {
    let mut out = Vec::with_capacity(rows.len());
    for (idx, row) in rows.into_iter().enumerate() {
        let f = row.try_into_array()?;
        if f.len() < 15 {
            bail!("rTorrent tracker row {idx} returned {} fields", f.len());
        }
        out.push(RawTracker {
            url: required_url(&f, 0)?,
            id: required_i64(&f, 1, "t.id")?,
            group: required_i64(&f, 2, "t.group")?,
            group_index: idx as i64,
            is_enabled: required_bool(&f, 3, "t.is_enabled")?,
            is_open: required_bool(&f, 4, "t.is_open")?,
            is_extra_tracker: required_bool(&f, 5, "t.is_extra_tracker")?,
            activity_time_last: required_i64(&f, 6, "t.activity_time_last")?,
            activity_time_next: required_i64(&f, 7, "t.activity_time_next")?,
            min_interval: required_i64(&f, 8, "t.min_interval")?,
            normal_interval: required_i64(&f, 9, "t.normal_interval")?,
            failed_counter: required_i64(&f, 10, "t.failed_counter")?,
            success_counter: required_i64(&f, 11, "t.success_counter")?,
            scrape_incomplete: required_i64(&f, 12, "t.scrape_incomplete")?,
            scrape_complete: required_i64(&f, 13, "t.scrape_complete")?,
            scrape_downloaded: required_i64(&f, 14, "t.scrape_downloaded")?,
            message: String::new(),
        });
    }
    Ok(out)
}

fn required_url(fields: &[XmlValue], index: usize) -> Result<String> {
    fields
        .get(index)
        .and_then(XmlValue::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("rTorrent response omitted valid t.url"))
}

fn required_i64(fields: &[XmlValue], index: usize, name: &str) -> Result<i64> {
    fields
        .get(index)
        .and_then(XmlValue::as_i64)
        .ok_or_else(|| anyhow!("rTorrent response omitted valid {name}"))
}

fn required_bool(fields: &[XmlValue], index: usize, name: &str) -> Result<bool> {
    fields
        .get(index)
        .and_then(XmlValue::as_bool)
        .ok_or_else(|| anyhow!("rTorrent response omitted valid {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tracker_rows_accepts_requested_field_count() {
        let rows = vec![XmlValue::Array(vec![
            "udp://tracker.example/announce".into(),
            7_i64.into(),
            1_i64.into(),
            true.into(),
            false.into(),
            true.into(),
            10_i64.into(),
            20_i64.into(),
            30_i64.into(),
            40_i64.into(),
            3_i64.into(),
            4_i64.into(),
            5_i64.into(),
            6_i64.into(),
            7_i64.into(),
        ])];

        let parsed = parse_tracker_rows(rows).unwrap();

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].url, "udp://tracker.example/announce");
        assert_eq!(parsed[0].id, 7);
        assert_eq!(parsed[0].group, 1);
        assert_eq!(parsed[0].group_index, 0);
        assert!(parsed[0].is_enabled);
        assert!(!parsed[0].is_open);
        assert!(parsed[0].is_extra_tracker);
        assert_eq!(parsed[0].scrape_complete, 6);
        assert_eq!(parsed[0].scrape_downloaded, 7);
        assert_eq!(parsed[0].message, "");
    }

    #[test]
    fn parse_tracker_rows_rejects_short_rows() {
        let parsed = parse_tracker_rows(vec![XmlValue::Array(vec![
            "udp://tracker.example/announce".into(),
        ])]);

        assert!(parsed.is_err());
    }
}
