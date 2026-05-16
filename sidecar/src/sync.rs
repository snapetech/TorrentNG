use std::sync::atomic::Ordering;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::{
    api::ws::Event,
    cache::{Db, TorrentRow},
    metrics::SharedMetrics,
    rtorrent::Client,
};

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

    loop {
        ticker.tick().await;
        match tick(&rt, &db, &tx).await {
            Ok((seeding, downloading, stopped, errored, peers)) => {
                metrics.sync_cycles_total.fetch_add(1, Ordering::Relaxed);
                metrics.torrents_seeding.store(seeding, Ordering::Relaxed);
                metrics
                    .torrents_downloading
                    .store(downloading, Ordering::Relaxed);
                metrics.torrents_stopped.store(stopped, Ordering::Relaxed);
                metrics.torrents_errored.store(errored, Ordering::Relaxed);
                metrics.peers_connected.store(peers, Ordering::Relaxed);
            }
            Err(e) => {
                metrics.sync_errors_total.fetch_add(1, Ordering::Relaxed);
                warn!("sync error: {e:?}");
            }
        }
    }
}

async fn tick(
    rt: &Client,
    db: &Db,
    tx: &broadcast::Sender<Event>,
) -> anyhow::Result<(i64, i64, i64, i64, i64)> {
    let torrents = rt.list_torrents().await?;
    let now = chrono::Utc::now().timestamp();

    let seen: HashSet<String> = torrents.iter().map(|t| t.hash.clone()).collect();

    let mut seeding = 0i64;
    let mut downloading = 0i64;
    let mut stopped = 0i64;
    let mut errored = 0i64;
    let mut peers = 0i64;

    for t in &torrents {
        // Update per-state counters
        if !t.message.is_empty() && t.state == 3 {
            errored += 1;
        } else if !t.is_active {
            stopped += 1;
        } else if t.complete {
            seeding += 1;
        } else {
            downloading += 1;
        }
        peers += t.peers_connected;

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
            tags: String::new(), // unused: read from torrent_tags at query time
            updated_at: now,
        };
        if let Err(e) = db.upsert(&row) {
            warn!("upsert {}: {e}", t.hash);
        }
        let _ = tx.send(Event::TorrentUpdated {
            hash: t.hash.clone(),
        });
    }

    // Remove torrents that disappeared from rTorrent
    let known = db.all_hashes()?;
    for hash in known.difference(&seen) {
        let _ = db.delete(hash);
        let _ = tx.send(Event::TorrentRemoved { hash: hash.clone() });
    }

    // Aggregate speed stats
    let up_speed: i64 = torrents.iter().map(|t| t.up_rate).sum();
    let dn_speed: i64 = torrents.iter().map(|t| t.down_rate).sum();
    let _ = tx.send(Event::Stats {
        upload_speed: up_speed,
        download_speed: dn_speed,
    });

    Ok((seeding, downloading, stopped, errored, peers))
}
