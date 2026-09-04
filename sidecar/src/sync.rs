use anyhow::{bail, Context};
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

#[derive(Debug, Clone, Default)]
struct SyncCounts {
    seeding: i64,
    downloading: i64,
    stopped: i64,
    errored: i64,
    peers: i64,
}

#[derive(Debug, Default)]
struct BoundedSyncState {
    page_offset: i64,
    snapshot: Option<u64>,
    full_cycle_seen: HashSet<String>,
    full_cycle_had_errors: bool,
    /// Backends with bounded range reads but no snapshot token (qBittorrent
    /// and unpatched rTorrent) are eventually consistent. Require a hash to
    /// be absent from two clean full cycles before deleting it from the
    /// compatibility cache; a transient short page must not become data loss.
    missing_confirmations: HashMap<String, u8>,
    counts: SyncCounts,
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
        feature = "bounded_torrent_sync",
        supported = range_supported,
        result = "ok",
        "backend capability probe complete"
    );
    let mut bounded = BoundedSyncState::default();
    let mut sync_error_active = false;

    loop {
        ticker.tick().await;
        let result = if range_supported {
            tick_bounded(backend.as_ref(), &db, &tx, &mut bounded).await
        } else {
            tick_full(backend.as_ref(), &db, &tx).await
        };
        match result {
            Ok(counts) => {
                if sync_error_active {
                    append_app_event_async(
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
                    )
                    .await;
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
                // layer (e.g. "bounded_torrent_sync main offset=200 limit=100")
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
                    append_app_event_async(
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
                    )
                    .await;
                    sync_error_active = true;
                }
            }
        }
    }
}

/// Renders every layer of an anyhow error chain, not just the outermost
/// `.context()` message. `anyhow::Error::to_string()` / `%e` only show the
/// top layer, which for these sync errors is always the same generic
/// "bounded_torrent_sync <view> offset=<n> limit=<n>" wrapper - the actual
/// backend fault (the useful part for diagnosing *why* it failed) is one or
/// more levels deeper and was previously invisible in both the tracing log
/// and the persisted operator-log event.
pub(crate) fn error_chain(e: &anyhow::Error) -> String {
    e.chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(" -> caused by: ")
}

async fn append_app_event_async(
    db: &Db,
    level: &str,
    kind: &str,
    message: &str,
    payload: serde_json::Value,
    retention: usize,
) {
    let event = AppEventRow {
        event_id: None,
        occurred_at: chrono::Utc::now().timestamp(),
        level: level.to_owned(),
        kind: kind.to_owned(),
        message: message.to_owned(),
        payload: payload.to_string(),
    };
    if let Err(e) = db
        .run_blocking("sync_app_event", move |db| {
            db.append_app_event(&event, retention)
        })
        .await
    {
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
) -> anyhow::Result<SyncCounts> {
    // The session-file fallback is only a cache for duplicate reads within a
    // single reconciliation pass. Keeping it across passes makes tracker
    // changes invisible and retains hashes for torrents that no longer
    // exist. Scope it to this pass so its lifetime matches the projection it
    // describes and its memory is bounded by the current response.
    let mut tracker_cache = HashMap::new();
    let torrents = backend.list_torrents().await?;
    let sync_tags = backend.capabilities().supports_tags;
    const MAX_LEGACY_FULL_SYNC_ENTRIES: usize = 10_000;
    if torrents.len() > MAX_LEGACY_FULL_SYNC_ENTRIES {
        bail!(
            "{} legacy full-list sync returned {} torrents; maximum is {}; use a backend with bounded range support",
            backend.backend_type().as_str(),
            torrents.len(),
            MAX_LEGACY_FULL_SYNC_ENTRIES
        );
    }
    let now = chrono::Utc::now().timestamp();

    // Info hashes are hexadecimal identifiers and all supported clients treat
    // them case-insensitively. Compare logical hashes, not the spelling a
    // backend happened to return this cycle; otherwise a casing-only refresh
    // looks like a removal and can delete a live cache row.
    let seen: HashSet<String> = torrents.iter().map(|t| logical_hash(&t.hash)).collect();

    let mut counts = SyncCounts::default();

    let mut sync_error = None;
    for t in &torrents {
        if let Err(error) =
            upsert_torrent(db, tx, t, now, &mut counts, &mut tracker_cache, sync_tags).await
        {
            warn!(
                component = backend.backend_type().as_str(),
                operation = "upsert_torrent",
                torrent = %t.hash,
                result = "error",
                error = %error,
                "backend sync could not persist torrent projection"
            );
            sync_error.get_or_insert(error);
        }
    }

    if sync_error.is_none() {
        let known = db
            .run_blocking("sync_all_hashes", |db| db.all_hashes())
            .await?;
        for hash in known
            .iter()
            .filter(|hash| !seen.contains(&logical_hash(hash)))
        {
            match db
                .run_blocking("sync_delete_torrent", {
                    let hash = hash.clone();
                    move |db| db.delete(&hash)
                })
                .await
            {
                Ok(()) => {
                    let _ = tx.send(Event::TorrentRemoved { hash: hash.clone() });
                }
                Err(error) => {
                    warn!(
                        component = backend.backend_type().as_str(),
                        operation = "delete_torrent",
                        torrent = %hash,
                        result = "error",
                        error = %error,
                        "backend sync could not delete a stale torrent projection"
                    );
                    sync_error.get_or_insert(error);
                }
            }
        }
    }

    sync_error.map_or(Ok(counts), Err)
}

async fn tick_bounded(
    backend: &dyn TorrentBackend,
    db: &Db,
    tx: &broadcast::Sender<Event>,
    bounded: &mut BoundedSyncState,
) -> anyhow::Result<SyncCounts> {
    // Only the live-summary and paged reads in this tick can duplicate a
    // torrent. A per-tick cache avoids retaining every hash across a large
    // multi-page reconciliation cycle.
    let mut tracker_cache = HashMap::new();
    let now = chrono::Utc::now().timestamp();
    let sync_tags = backend.capabilities().supports_tags;
    let mut counts = SyncCounts::default();
    let mut touched = HashSet::new();
    let mut sync_error = None;

    match backend
        .live_summary("main", MULTICALL_RANGE_PAGE_SIZE)
        .await
    {
        Ok(summary) => {
            write_live_speeds(summary.rates.download, summary.rates.upload).await;
            for t in &summary.moving {
                // The live-summary view and the paged main view are separate
                // backend reads. Protect a torrent reported by the former
                // from end-of-list cleanup if the latter is temporarily
                // inconsistent or omits it during a concurrent mutation.
                let logical = logical_hash(&t.hash);
                bounded.full_cycle_seen.insert(logical.clone());
                if !touched.contains(&logical) {
                    match upsert_torrent(db, tx, t, now, &mut counts, &mut tracker_cache, sync_tags)
                        .await
                    {
                        Ok(()) => {
                            touched.insert(logical);
                        }
                        Err(error) => {
                            bounded.full_cycle_had_errors = true;
                            warn!(
                                component = backend.backend_type().as_str(),
                                operation = "upsert_torrent",
                                torrent = %t.hash,
                                result = "error",
                                error = %error,
                                "live summary torrent projection could not be persisted"
                            );
                            sync_error.get_or_insert(error);
                        }
                    }
                }
            }
        }
        Err(e) => {
            let chain = error_chain(&e);
            warn!(
                component = backend.backend_type().as_str(),
                operation = "live_summary_sync",
                result = "error",
                error = %chain,
                "live summary sync failed"
            );
            sync_error.get_or_insert_with(|| anyhow::anyhow!("live summary sync failed: {chain}"));
        }
    }

    let offset = bounded.page_offset;
    let fetched =
        fetch_range_resilient(backend, offset, MULTICALL_RANGE_PAGE_SIZE, bounded.snapshot).await;
    let page_len = fetched.torrents.len() as i64;
    bounded.full_cycle_had_errors |= fetched.had_errors;
    if fetched.had_errors {
        sync_error.get_or_insert_with(|| {
            anyhow::anyhow!("bounded torrent range returned an incomplete page at offset {offset}")
        });
    }
    if bounded.snapshot.is_none() {
        bounded.snapshot = fetched.snapshot;
    } else if fetched.snapshot != bounded.snapshot {
        bounded.full_cycle_had_errors = true;
        warn!(
            component = backend.backend_type().as_str(),
            operation = "bounded_sync_snapshot",
            result = "error",
            expected = ?bounded.snapshot,
            observed = ?fetched.snapshot,
            "backend changed or dropped the bounded-sync snapshot"
        );
        sync_error.get_or_insert_with(|| {
            anyhow::anyhow!("bounded torrent range changed snapshot at offset {offset}")
        });
    }

    for t in &fetched.torrents {
        let logical = logical_hash(&t.hash);
        bounded.full_cycle_seen.insert(logical.clone());
        if !touched.contains(&logical) {
            match upsert_torrent(db, tx, t, now, &mut counts, &mut tracker_cache, sync_tags).await {
                Ok(()) => {
                    touched.insert(logical);
                }
                Err(error) => {
                    bounded.full_cycle_had_errors = true;
                    warn!(
                        component = backend.backend_type().as_str(),
                        operation = "upsert_torrent",
                        torrent = %t.hash,
                        result = "error",
                        error = %error,
                        "paged torrent projection could not be persisted"
                    );
                    sync_error.get_or_insert(error);
                }
            }
        }
    }

    // A short page means "reached the end of the torrent list". Only perform
    // removed-torrent cleanup after a completely clean cycle: a failed page
    // may have omitted a live torrent, and deleting it from the cache here
    // would turn a transient XMLRPC fault into data loss. The error flag must
    // live for the whole cycle, not just this final short page.
    if page_len < MULTICALL_RANGE_PAGE_SIZE {
        if !bounded.full_cycle_had_errors {
            let known = db
                .run_blocking("sync_all_hashes", |db| db.all_hashes())
                .await?;
            let known_by_logical = known
                .iter()
                .map(|hash| (logical_hash(hash), hash))
                .collect::<HashMap<_, _>>();
            let stable_snapshot = bounded.snapshot.is_some();
            let missing = known_by_logical
                .iter()
                .filter(|(logical, _)| !bounded.full_cycle_seen.contains(*logical))
                .map(|(logical, hash)| (logical.clone(), (*hash).clone()))
                .collect::<Vec<_>>();
            bounded.missing_confirmations.retain(|logical, _| {
                !bounded.full_cycle_seen.contains(logical) && known_by_logical.contains_key(logical)
            });
            for (logical, hash) in missing {
                let confirmed = stable_snapshot
                    || bounded
                        .missing_confirmations
                        .insert(logical.clone(), 1)
                        .is_some();
                if confirmed {
                    bounded.missing_confirmations.remove(&logical);
                    match db
                        .run_blocking("sync_delete_torrent", {
                            let hash = hash.clone();
                            move |db| db.delete(&hash)
                        })
                        .await
                    {
                        Ok(()) => {
                            let _ = tx.send(Event::TorrentRemoved { hash });
                        }
                        Err(error) => {
                            bounded.full_cycle_had_errors = true;
                            warn!(
                                component = backend.backend_type().as_str(),
                                operation = "delete_torrent",
                                torrent = %hash,
                                result = "error",
                                error = %error,
                                "backend sync could not delete a stale torrent projection"
                            );
                            sync_error.get_or_insert(error);
                        }
                    }
                }
            }
        } else {
            bounded.missing_confirmations.clear();
            warn!(
                component = backend.backend_type().as_str(),
                operation = "bounded_sync_cleanup",
                result = "skipped",
                "skipping removed-torrent cleanup because an earlier page in this cycle had fetch errors"
            );
        }
        if !bounded.full_cycle_had_errors {
            let (errored, stopped, seeding, downloading, peers) = db
                .run_blocking("sync_counts", |db| db.sync_counts())
                .await
                .context("aggregate bounded sync counters")?;
            bounded.counts = SyncCounts {
                seeding,
                downloading,
                stopped,
                errored,
                peers,
            };
        }
        for hash in &bounded.full_cycle_seen {
            bounded.missing_confirmations.remove(hash);
        }
        let cycle_had_errors = bounded.full_cycle_had_errors;
        bounded.full_cycle_seen.clear();
        bounded.full_cycle_had_errors = false;
        bounded.page_offset = 0;
        bounded.snapshot = None;
        if cycle_had_errors {
            sync_error.get_or_insert_with(|| {
                anyhow::anyhow!("bounded torrent sync cycle had incomplete or failed pages")
            });
        }
    } else {
        bounded.page_offset += MULTICALL_RANGE_PAGE_SIZE;
    }

    sync_error.map_or(Ok(bounded.counts.clone()), Err)
}

struct ResilientFetch {
    torrents: Vec<RawTorrent>,
    snapshot: Option<u64>,
    /// True if any sub-range in this fetch failed (and was skipped) even
    /// after bisecting down to a single torrent. The caller must not treat
    /// a short result as "end of list" when this is set.
    had_errors: bool,
}

/// Fetches `[offset, offset+limit)` via the backend's bounded range API, and on failure
/// bisects the range and retries each half so a single torrent whose fields
/// can't be read (observed in production as intermittent
/// bounded-range faults at varying offsets) only costs that one
/// torrent's data for this tick, rather than the entire page silently going
/// stale until the next successful cycle.
fn fetch_range_resilient(
    backend: &dyn TorrentBackend,
    offset: i64,
    limit: i64,
    snapshot: Option<u64>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ResilientFetch> + Send + '_>> {
    Box::pin(async move {
        match backend
            .list_torrents_range_with_snapshot("main", offset, limit, snapshot)
            .await
        {
            Ok((torrents, observed_snapshot))
                if snapshot.is_none_or(|expected| observed_snapshot == Some(expected)) =>
            {
                ResilientFetch {
                    torrents,
                    snapshot: observed_snapshot,
                    had_errors: false,
                }
            }
            Ok((_, observed_snapshot)) => {
                warn!(
                    component = backend.backend_type().as_str(),
                    operation = "list_torrents_range_snapshot",
                    offset,
                    limit,
                    expected = ?snapshot,
                    observed = ?observed_snapshot,
                    "discarding a page returned for the wrong snapshot"
                );
                ResilientFetch {
                    torrents: Vec::new(),
                    snapshot: observed_snapshot,
                    had_errors: true,
                }
            }
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
                let mut left = fetch_range_resilient(backend, offset, left_limit, snapshot).await;
                let right_snapshot = left.snapshot.or(snapshot);
                let right = fetch_range_resilient(
                    backend,
                    offset + left_limit,
                    right_limit,
                    right_snapshot,
                )
                .await;
                left.torrents.extend(right.torrents);
                let snapshot_mismatch = left
                    .snapshot
                    .zip(right.snapshot)
                    .is_some_and(|(left, right)| left != right);
                ResilientFetch {
                    torrents: left.torrents,
                    snapshot: left.snapshot.or(right.snapshot),
                    had_errors: left.had_errors || right.had_errors || snapshot_mismatch,
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
                    snapshot,
                    had_errors: true,
                }
            }
        }
    })
}

async fn session_tracker_url_async(
    hash: &str,
    tracker_cache: &mut HashMap<String, Option<String>>,
) -> String {
    let normalized = hash.trim().to_ascii_uppercase();
    if let Some(cached) = tracker_cache.get(&normalized) {
        return cached.clone().unwrap_or_default();
    }
    let hash = hash.to_owned();
    let tracker = tokio::task::spawn_blocking(move || {
        let mut cache = HashMap::new();
        session_tracker_url(&hash, &mut cache)
    })
    .await
    .unwrap_or_default();
    tracker_cache.insert(normalized, (!tracker.is_empty()).then(|| tracker.clone()));
    tracker
}

async fn upsert_torrent(
    db: &Db,
    tx: &broadcast::Sender<Event>,
    t: &RawTorrent,
    now: i64,
    counts: &mut SyncCounts,
    tracker_cache: &mut HashMap<String, Option<String>>,
    sync_tags: bool,
) -> anyhow::Result<()> {
    let tracker_url = if t.tracker_url.is_empty() {
        session_tracker_url_async(&t.hash, tracker_cache).await
    } else {
        t.tracker_url.clone()
    };
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
        tracker_url,
        tags: t.tags.clone(),
        updated_at: now,
    };
    let changed = db
        .run_blocking("sync_upsert_torrent", move |db| {
            db.upsert_with_tags(&row, sync_tags)
        })
        .await
        .with_context(|| format!("persist torrent cache row {}", t.hash))?;
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
    if changed {
        let _ = tx.send(Event::TorrentUpdated {
            hash: t.hash.clone(),
        });
    }
    Ok(())
}

fn logical_hash(hash: &str) -> String {
    hash.to_ascii_lowercase()
}

async fn write_live_speeds(download: i64, upload: i64) {
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
    let target = std::path::Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("live-speeds.json")
        .to_owned();
    let cleanup_path = tmp_path.clone();
    let result = match tokio::task::spawn_blocking(move || {
        let result =
            std::fs::write(&tmp_path, body).and_then(|_| std::fs::rename(&tmp_path, &path));
        if result.is_err() {
            let _ = std::fs::remove_file(cleanup_path);
        }
        result
    })
    .await
    {
        Ok(result) => result.map_err(|e| e.to_string()),
        Err(e) => Err(format!("blocking live speed writer failed: {e}")),
    };
    if let Err(e) = result {
        warn!(
            component = "stats",
            operation = "write_live_speeds",
            target,
            result = "error",
            error = %e,
            "live speed cache write failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_app_event_persists_sync_failure_shape() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();

        append_app_event_async(
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
        )
        .await;

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

    #[test]
    fn logical_hash_collapses_hex_case_for_sync_identity() {
        assert_eq!(logical_hash("ABCdef0123"), "abcdef0123");
        assert_eq!(logical_hash("abcdef0123"), logical_hash("ABCDEF0123"));
    }
}
