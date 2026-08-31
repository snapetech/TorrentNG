use std::sync::atomic::Ordering;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::{
    api::ws::Event,
    backend::TorrentBackend,
    cache::{AppEventRow, Db, TorrentRow},
    metrics::SharedMetrics,
    rtorrent::torrents::{RawTorrent, MULTICALL_RANGE_PAGE_SIZE},
    torrent_meta::session_tracker_url,
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
    backend: Arc<dyn TorrentBackend>,
    db: Arc<Db>,
    tx: broadcast::Sender<Event>,
    metrics: SharedMetrics,
    interval: Duration,
    event_retention: usize,
) {
    info!(
        component = backend.backend_type().as_str(),
        operation = "sync_loop",
        interval_ms = interval.as_millis() as u64,
        result = "started",
        "sync loop started"
    );
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;
    let range_supported = backend.has_bounded_sync().await;
    info!(
        component = backend.backend_type().as_str(),
        operation = "capability_probe",
        feature = "d.multicall.range",
        supported = range_supported,
        result = "ok",
        "backend capability probe complete"
    );
    let mut page_offset = 0i64;
    let mut full_cycle_seen = HashSet::new();
    let mut full_cycle_had_errors = false;
    let mut tracker_cache = HashMap::new();
    let mut sync_error_active = false;

    loop {
        ticker.tick().await;
        let result = if range_supported {
            tick_bounded(
                backend.as_ref(),
                &db,
                &tx,
                &mut page_offset,
                &mut full_cycle_seen,
                &mut full_cycle_had_errors,
                &mut tracker_cache,
            )
            .await
        } else {
            tick_full(backend.as_ref(), &db, &tx, &mut tracker_cache).await
        };
        match result {
            Ok(counts) => {
                if sync_error_active {
                    append_app_event(
                        &db,
                        "info",
                        "rtorrent_sync_recovered",
                        "backend sync recovered",
                        serde_json::json!({
                        "component": backend.backend_type().as_str(),
                            "operation": "sync",
                            "result": "ok",
                        }),
                        event_retention,
                    );
                    sync_error_active = false;
                }
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
                // `e.to_string()`/`%e` only print the outermost `.context()`
                // layer (e.g. "d.multicall.range main offset=200 limit=100")
                // and silently drop the actual XMLRPC fault underneath it,
                // which is the one detail that would explain *why* the
                // multicall failed. `error_chain` keeps the whole chain.
                let chain = error_chain(&e);
                warn!(
                    component = backend.backend_type().as_str(),
                    operation = "sync",
                    result = "error",
                    error = %chain,
                    "backend sync failed"
                );
                if !sync_error_active {
                    append_app_event(
                        &db,
                        "warn",
                        "rtorrent_sync_error",
                        "backend sync failed",
                        serde_json::json!({
                            "component": backend.backend_type().as_str(),
                            "operation": "sync",
                            "result": "error",
                            "error": chain,
                        }),
                        event_retention,
                    );
                    sync_error_active = true;
                }
            }
        }
    }
}

/// Renders every layer of an anyhow error chain, not just the outermost
/// `.context()` message. `anyhow::Error::to_string()` / `%e` only show the
/// top layer, which for these sync errors is always the same generic
/// "d.multicall.range <view> offset=<n> limit=<n>" wrapper - the actual
/// XMLRPC fault (the useful part for diagnosing *why* it failed) is one or
/// more levels deeper and was previously invisible in both the tracing log
/// and the persisted operator-log event.
pub(crate) fn error_chain(e: &anyhow::Error) -> String {
    e.chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(" -> caused by: ")
}

fn append_app_event(
    db: &Db,
    level: &str,
    kind: &str,
    message: &str,
    payload: serde_json::Value,
    retention: usize,
) {
    if let Err(e) = db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: chrono::Utc::now().timestamp(),
            level: level.to_owned(),
            kind: kind.to_owned(),
            message: message.to_owned(),
            payload: payload.to_string(),
        },
        retention,
    ) {
        warn!(
            component = "app_events",
            operation = "append",
            kind,
            result = "error",
            error = %e,
            "failed to append sync app event"
        );
    }
}

async fn tick_full(
    backend: &dyn TorrentBackend,
    db: &Db,
    tx: &broadcast::Sender<Event>,
    tracker_cache: &mut HashMap<String, Option<String>>,
) -> anyhow::Result<SyncCounts> {
    let torrents = backend.list_torrents().await?;
    let now = chrono::Utc::now().timestamp();

    let seen: HashSet<String> = torrents.iter().map(|t| t.hash.clone()).collect();

    let mut counts = SyncCounts::default();

    for t in &torrents {
        upsert_torrent(db, tx, t, now, &mut counts, tracker_cache);
    }

    let known = db.all_hashes()?;
    for hash in known.difference(&seen) {
        let _ = db.delete(hash);
        let _ = tx.send(Event::TorrentRemoved { hash: hash.clone() });
    }

    Ok(counts)
}

async fn tick_bounded(
    backend: &dyn TorrentBackend,
    db: &Db,
    tx: &broadcast::Sender<Event>,
    page_offset: &mut i64,
    full_cycle_seen: &mut HashSet<String>,
    full_cycle_had_errors: &mut bool,
    tracker_cache: &mut HashMap<String, Option<String>>,
) -> anyhow::Result<SyncCounts> {
    let now = chrono::Utc::now().timestamp();
    let mut counts = SyncCounts::default();
    let mut touched = HashSet::new();

    match backend
        .live_summary("main", MULTICALL_RANGE_PAGE_SIZE)
        .await
    {
        Ok(summary) => {
            write_live_speeds(summary.rates.download, summary.rates.upload);
            for t in &summary.moving {
                if touched.insert(t.hash.clone()) {
                    upsert_torrent(db, tx, t, now, &mut counts, tracker_cache);
                }
            }
        }
        Err(e) => warn!(
            component = backend.backend_type().as_str(),
            operation = "live_summary_sync",
            result = "error",
            error = %e,
            "live summary sync failed"
        ),
    }

    let fetched = fetch_range_resilient(backend, *page_offset, MULTICALL_RANGE_PAGE_SIZE).await;
    let page_len = fetched.torrents.len() as i64;
    *full_cycle_had_errors |= fetched.had_errors;

    for t in &fetched.torrents {
        full_cycle_seen.insert(t.hash.clone());
        if touched.insert(t.hash.clone()) {
            upsert_torrent(db, tx, t, now, &mut counts, tracker_cache);
        }
    }

    // A short page means "reached the end of the torrent list". Only perform
    // removed-torrent cleanup after a completely clean cycle: a failed page
    // may have omitted a live torrent, and deleting it from the cache here
    // would turn a transient XMLRPC fault into data loss. The error flag must
    // live for the whole cycle, not just this final short page.
    if page_len < MULTICALL_RANGE_PAGE_SIZE {
        if !*full_cycle_had_errors {
            let known = db.all_hashes()?;
            for hash in known.difference(full_cycle_seen) {
                let _ = db.delete(hash);
                let _ = tx.send(Event::TorrentRemoved { hash: hash.clone() });
            }
        } else {
            warn!(
                component = backend.backend_type().as_str(),
                operation = "bounded_sync_cleanup",
                result = "skipped",
                "skipping removed-torrent cleanup because an earlier page in this cycle had fetch errors"
            );
        }
        full_cycle_seen.clear();
        *full_cycle_had_errors = false;
        *page_offset = 0;
    } else {
        *page_offset += MULTICALL_RANGE_PAGE_SIZE;
    }

    Ok(counts)
}

struct ResilientFetch {
    torrents: Vec<RawTorrent>,
    /// True if any sub-range in this fetch failed (and was skipped) even
    /// after bisecting down to a single torrent. The caller must not treat
    /// a short result as "end of list" when this is set.
    had_errors: bool,
}

/// Fetches `[offset, offset+limit)` via `d.multicall.range`, and on failure
/// bisects the range and retries each half so a single torrent whose fields
/// can't be read (observed in production as intermittent
/// `d.multicall.range` faults at varying offsets) only costs that one
/// torrent's data for this tick, rather than the entire page silently going
/// stale until the next successful cycle.
fn fetch_range_resilient(
    backend: &dyn TorrentBackend,
    offset: i64,
    limit: i64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ResilientFetch> + Send + '_>> {
    Box::pin(async move {
        match backend.list_torrents_range("main", offset, limit).await {
            Ok(torrents) => ResilientFetch {
                torrents,
                had_errors: false,
            },
            Err(e) if limit > 1 => {
                warn!(
                    component = backend.backend_type().as_str(),
                    operation = "list_torrents_range_bisect",
                    offset,
                    limit,
                    error = %error_chain(&e),
                    "range fetch failed, bisecting to isolate the faulting torrent(s)"
                );
                let left_limit = limit / 2;
                let right_limit = limit - left_limit;
                let mut left = fetch_range_resilient(backend, offset, left_limit).await;
                let right = fetch_range_resilient(backend, offset + left_limit, right_limit).await;
                left.torrents.extend(right.torrents);
                ResilientFetch {
                    torrents: left.torrents,
                    had_errors: left.had_errors || right.had_errors,
                }
            }
            Err(e) => {
                warn!(
                    component = backend.backend_type().as_str(),
                    operation = "list_torrents_range_skip",
                    offset,
                    error = %error_chain(&e),
                    "skipping one torrent whose fields could not be fetched this cycle"
                );
                ResilientFetch {
                    torrents: Vec::new(),
                    had_errors: true,
                }
            }
        }
    })
}

fn upsert_torrent(
    db: &Db,
    tx: &broadcast::Sender<Event>,
    t: &RawTorrent,
    now: i64,
    counts: &mut SyncCounts,
    tracker_cache: &mut HashMap<String, Option<String>>,
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
        tracker_url: if t.tracker_url.is_empty() {
            session_tracker_url(&t.hash, tracker_cache)
        } else {
            t.tracker_url.clone()
        },
        tags: t.tags.clone(),
        updated_at: now,
    };
    if let Err(e) = db.upsert(&row) {
        warn!(
            component = "cache",
            operation = "upsert_torrent",
            torrent = %t.hash,
            result = "error",
            error = %e,
            "torrent cache upsert failed"
        );
    }
    let _ = tx.send(Event::TorrentUpdated {
        hash: t.hash.clone(),
    });
}

fn write_live_speeds(download: i64, upload: i64) {
    let Some(path) = std::env::var("TNG_LIVE_SPEEDS_FILE")
        .or_else(|_| std::env::var("RTNG_LIVE_SPEEDS_FILE"))
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
        let target = std::path::Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("live-speeds.json");
        warn!(
            component = "stats",
            operation = "write_live_speeds",
            target,
            result = "error",
            error = %e,
            "live speed cache write failed"
        );
        let _ = std::fs::remove_file(tmp_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_app_event_persists_sync_failure_shape() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();

        append_app_event(
            &db,
            "warn",
            "rtorrent_sync_error",
            "backend sync failed",
            serde_json::json!({
                "component": "rtorrent",
                "operation": "sync",
                "result": "error",
                "error": "connection refused",
            }),
            10,
        );

        let events = db.list_app_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].level, "warn");
        assert_eq!(events[0].kind, "rtorrent_sync_error");
        assert_eq!(events[0].message, "backend sync failed");
        let payload: serde_json::Value = serde_json::from_str(&events[0].payload).unwrap();
        assert_eq!(payload["component"], "rtorrent");
        assert_eq!(payload["operation"], "sync");
        assert_eq!(payload["result"], "error");
    }
}
