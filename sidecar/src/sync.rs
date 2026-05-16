use std::sync::atomic::Ordering;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::{
    api::ws::Event,
    cache::{Db, TorrentRow},
    metrics::SharedMetrics,
    rtorrent::{torrents::MULTICALL_RANGE_PAGE_SIZE, Client},
};

#[derive(Debug, Default)]
struct SyncCounts {
    seeding: i64,
    downloading: i64,
    stopped: i64,
    errored: i64,
    peers: i64,
}

pub async fn run(
    rt: Arc<Client>,
    db: Arc<Db>,
    tx: broadcast::Sender<Event>,
    metrics: SharedMetrics,
    interval: Duration,
) {
    info!("sync loop started, interval={interval:?}");
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;
    let range_supported = rt.has_multicall_range().await;
    info!("rTorrent d.multicall.range supported={range_supported}");
    let mut page_offset = 0i64;
    let mut full_cycle_seen = HashSet::new();

    loop {
        ticker.tick().await;
        let result = if range_supported {
            tick_bounded(&rt, &db, &tx, &mut page_offset, &mut full_cycle_seen).await
        } else {
            tick_full(&rt, &db, &tx).await
        };
        match result {
            Ok(counts) => {
                metrics.sync_cycles_total.fetch_add(1, Ordering::Relaxed);
                metrics
                    .torrents_seeding
                    .store(counts.seeding, Ordering::Relaxed);
                metrics
                    .torrents_downloading
                    .store(counts.downloading, Ordering::Relaxed);
                metrics
                    .torrents_stopped
                    .store(counts.stopped, Ordering::Relaxed);
                metrics
                    .torrents_errored
                    .store(counts.errored, Ordering::Relaxed);
                metrics
                    .peers_connected
                    .store(counts.peers, Ordering::Relaxed);
            }
            Err(e) => {
                metrics.sync_errors_total.fetch_add(1, Ordering::Relaxed);
                warn!("sync error: {e:?}");
            }
        }
    }
}

async fn tick_full(
    rt: &Client,
    db: &Db,
    tx: &broadcast::Sender<Event>,
) -> anyhow::Result<SyncCounts> {
    let torrents = rt.list_torrents().await?;
    let now = chrono::Utc::now().timestamp();

    let seen: HashSet<String> = torrents.iter().map(|t| t.hash.clone()).collect();

    let mut counts = SyncCounts::default();

    for t in &torrents {
        upsert_torrent(db, tx, t, now, &mut counts);
    }

    let known = db.all_hashes()?;
    for hash in known.difference(&seen) {
        let _ = db.delete(hash);
        let _ = tx.send(Event::TorrentRemoved { hash: hash.clone() });
    }

    Ok(counts)
}

async fn tick_bounded(
    rt: &Client,
    db: &Db,
    tx: &broadcast::Sender<Event>,
    page_offset: &mut i64,
    full_cycle_seen: &mut HashSet<String>,
) -> anyhow::Result<SyncCounts> {
    let now = chrono::Utc::now().timestamp();
    let mut counts = SyncCounts::default();
    let mut touched = HashSet::new();

    match rt.live_summary("main", MULTICALL_RANGE_PAGE_SIZE).await {
        Ok(summary) => {
            write_live_speeds(summary.rates.download, summary.rates.upload);
            for t in &summary.moving {
                if touched.insert(t.hash.clone()) {
                    upsert_torrent(db, tx, t, now, &mut counts);
                }
            }
        }
        Err(e) => warn!("live summary sync failed: {e:?}"),
    }

    let page = rt
        .list_torrents_range("main", *page_offset, MULTICALL_RANGE_PAGE_SIZE)
        .await?;
    let page_len = page.len() as i64;

    for t in &page {
        full_cycle_seen.insert(t.hash.clone());
        if touched.insert(t.hash.clone()) {
            upsert_torrent(db, tx, t, now, &mut counts);
        }
    }

    if page_len < MULTICALL_RANGE_PAGE_SIZE {
        let known = db.all_hashes()?;
        for hash in known.difference(full_cycle_seen) {
            let _ = db.delete(hash);
            let _ = tx.send(Event::TorrentRemoved { hash: hash.clone() });
        }
        full_cycle_seen.clear();
        *page_offset = 0;
    } else {
        *page_offset += page_len;
    }

    Ok(counts)
}

fn upsert_torrent(
    db: &Db,
    tx: &broadcast::Sender<Event>,
    t: &crate::rtorrent::torrents::RawTorrent,
    now: i64,
    counts: &mut SyncCounts,
) {
    if !t.message.is_empty() && t.state == 3 {
        counts.errored += 1;
    } else if !t.is_active {
        counts.stopped += 1;
    } else if t.complete {
        counts.seeding += 1;
    } else {
        counts.downloading += 1;
    }
    counts.peers += t.peers_connected;

    let row = TorrentRow {
        hash: t.hash.clone(),
        name: t.name.clone(),
        size_bytes: t.size_bytes,
        bytes_done: t.bytes_done,
        down_rate: t.down_rate,
        up_rate: t.up_rate,
        up_total: t.up_total,
        down_total: t.down_total,
        ratio: t.ratio,
        is_active: t.is_active,
        is_open: t.is_open,
        complete: t.complete,
        state: t.state,
        priority: t.priority,
        category: t.category.clone(),
        base_path: t.base_path.clone(),
        directory: t.directory.clone(),
        creation_date: t.creation_date,
        timestamp_finished: t.timestamp_finished,
        tracker_focus: t.tracker_focus,
        peers_connected: t.peers_connected,
        peers_complete: t.peers_complete,
        message: t.message.clone(),
        tracker_url: t.tracker_url.clone(),
        tags: String::new(),
        updated_at: now,
    };
    if let Err(e) = db.upsert(&row) {
        warn!("upsert {}: {e}", t.hash);
    }
    let _ = tx.send(Event::TorrentUpdated {
        hash: t.hash.clone(),
    });
}

fn write_live_speeds(download: i64, upload: i64) {
    let Some(path) = std::env::var("RTNG_LIVE_SPEEDS_FILE")
        .ok()
        .filter(|path| !path.trim().is_empty())
    else {
        return;
    };
    let body = serde_json::json!({
        "download": download.max(0),
        "upload": upload.max(0),
        "updated_at": chrono::Utc::now().timestamp(),
    })
    .to_string();
    let tmp_path = format!("{path}.tmp");
    if let Err(e) = std::fs::write(&tmp_path, body).and_then(|_| std::fs::rename(&tmp_path, &path))
    {
        warn!("write live speeds {path}: {e}");
        let _ = std::fs::remove_file(tmp_path);
    }
}
