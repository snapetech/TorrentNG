use super::*;
use crate::model::QbServerState;
use rt_api_model::ChunkedVec;
use serde::ser::{SerializeMap, Serializer};
#[derive(Debug, Deserialize)]
pub struct MaindataQuery {
    pub rid: Option<i64>,
}

#[derive(Debug, Serialize)]
struct SyncMaindataResponse {
    rid: i64,
    full_update: bool,
    torrents: SyncTorrentMap,
    torrents_removed: Vec<String>,
    server_state: QbServerState,
}

#[derive(Debug)]
struct SyncTorrentMap {
    infos: Vec<QbTorrentInfo>,
}

impl Serialize for SyncTorrentMap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.infos.len()))?;
        for info in &self.infos {
            map.serialize_entry(&info.hash, info)?;
        }
        map.end()
    }
}

pub async fn handle_sync_maindata(
    State(state): State<AppState>,
    Query(q): Query<MaindataQuery>,
) -> impl IntoResponse {
    let (current_revision, requested_revision) = {
        let registry = state.registry.read().await;
        (
            registry.revision(),
            q.rid.filter(|rid| *rid > 0).map(|rid| rid as u64),
        )
    };
    let unchanged_empty_registry = q
        .rid
        .is_some_and(|rid| rid > 0 && rid == qbit_registry_rid(current_revision))
        && current_revision == 0;
    let empty_entries = Arc::new(ChunkedVec::from_vec(Vec::<rt_session::TorrentEntry>::new()));
    let (revision, full_update, entries, torrents_removed) = if requested_revision
        .is_some_and(|requested| requested == current_revision)
        || unchanged_empty_registry
    {
        (current_revision, false, empty_entries, Vec::new())
    } else {
        let delta = if let Some(requested_revision) =
            requested_revision.filter(|requested| *requested <= current_revision)
        {
            let registry = state.registry.read().await;
            registry.changes_since(requested_revision).map(|changes| {
                let mut changed = HashSet::new();
                let mut removed = HashSet::new();
                for change in changes {
                    if change.removed {
                        changed.remove(&change.info_hash);
                        removed.insert(change.info_hash);
                    } else {
                        removed.remove(&change.info_hash);
                        changed.insert(change.info_hash);
                    }
                }
                let entries = changed
                    .into_iter()
                    .filter_map(|hash| registry.get(&hash))
                    .collect::<Vec<_>>();
                let mut removed = removed
                    .into_iter()
                    .filter(|hash| registry.get(hash).is_none())
                    .collect::<Vec<_>>();
                removed.sort_unstable();
                (
                    registry.revision(),
                    Arc::new(ChunkedVec::from_vec(entries)),
                    removed,
                )
            })
        } else {
            None
        };
        if let Some((revision, entries, removed)) = delta {
            (revision, false, entries, removed)
        } else {
            // Full updates use the shared snapshot cache. This remains
            // O(N) when the registry changes, but repeated qBit polling
            // no longer clones every TorrentEntry independently of the
            // TorrentNG/SSE snapshot consumers.
            let snapshot = match state.torrent_snapshot(None).await {
                Ok(snapshot) => snapshot,
                Err(TorrentSnapshotError::Expired { revision }) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        format!("failed to build torrent snapshot at revision {revision}"),
                    )
                        .into_response();
                }
            };
            (snapshot.revision, true, snapshot.entries, Vec::new())
        }
    };
    let torrent_count = entries.len();
    let include_live = torrent_count <= QBIT_LIVE_PROJECTION_MAX_ENTRIES;
    let estimate = estimate_qbit_torrent_info_page_bytes(entries.iter(), include_live);
    let _lease = if state.engine.is_some() {
        match reserve_qbit_api_snapshot(&state, estimate).await {
            Ok(Some(lease)) => Some(lease),
            Ok(None) => return qbit_api_snapshot_budget_exhausted(),
            Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
        }
    } else {
        None
    };
    let active_rechecks = if entries.is_empty() {
        HashSet::new()
    } else {
        match active_recheck_hashes(&state).await {
            Ok(active_rechecks) => active_rechecks,
            Err(error) => return qbit_backend_unavailable(&error),
        }
    };
    let mut infos = Vec::with_capacity(entries.len());
    if include_live {
        let live_entries = entries.iter().cloned().collect();
        infos = match load_qbit_live_projections(&state, live_entries, active_rechecks).await {
            Ok(infos) => infos,
            Err(error) => return qbit_backend_unavailable(&error),
        };
    } else {
        for entry in entries.iter() {
            let info = match qbit_torrent_info(&state, entry, &active_rechecks, false).await {
                Ok(info) => info,
                Err(error) => return qbit_backend_unavailable(&error),
            };
            infos.push(info);
        }
    }
    state.api_metrics.record_estimated_response_bytes(estimate);
    let rid = qbit_registry_rid(revision);
    let (alltime_dl, alltime_ul, session_rates, connected_peers, queued_io_jobs) =
        if let Some(engine) = &state.engine {
            match engine.stats().await {
                Ok(stats) => (
                    qbit_i64(stats.bytes_downloaded),
                    qbit_i64(stats.bytes_uploaded),
                    QbitSwarmProjection {
                        download_rate: stats.download_rate,
                        upload_rate: stats.upload_rate,
                        ..Default::default()
                    },
                    qbit_i64(stats.connected_peers),
                    qbit_i64(stats.storage_jobs_queue_depth),
                ),
                Err(error) => return qbit_backend_unavailable(&error),
            }
        } else {
            let (alltime_dl, alltime_ul) = infos.iter().fold((0_i64, 0_i64), |(dl, ul), info| {
                (
                    dl.saturating_add(info.downloaded),
                    ul.saturating_add(info.uploaded),
                )
            });
            (
                alltime_dl,
                alltime_ul,
                qbit_session_rates_from_infos(&infos),
                qbit_i64(
                    infos
                        .iter()
                        .map(|info| info.num_leechs as u64 + info.num_seeds as u64)
                        .sum(),
                ),
                0,
            )
        };
    let global_ratio = if alltime_dl > 0 {
        alltime_ul as f64 / alltime_dl as f64
    } else {
        0.0
    };
    let limits = match global_limits_result(&state).await {
        Ok(limits) => limits,
        Err(error) => return qbit_backend_unavailable(&error),
    };
    let free_space_on_disk = if let Some(engine) = &state.engine {
        match engine.list_storage_roots().await {
            Ok(roots) => roots
                .into_iter()
                .filter(|root| root.ok)
                .map(|root| root.available_bytes)
                .max()
                .map(qbit_i64)
                .unwrap_or(0),
            Err(error) => return qbit_backend_unavailable(&error),
        }
    } else {
        0
    };
    let resp = SyncMaindataResponse {
        rid,
        full_update,
        torrents: SyncTorrentMap { infos },
        torrents_removed,
        server_state: QbServerState {
            dl_info_speed: session_rates.download_rate,
            dl_info_data: alltime_dl,
            up_info_speed: session_rates.upload_rate,
            up_info_data: alltime_ul,
            alltime_dl,
            alltime_ul,
            average_time_queue: 0,
            connection_status: "connected".into(),
            free_space_on_disk,
            global_ratio,
            queued_io_jobs,
            queueing: false,
            read_cache_hits: "0".into(),
            read_cache_overload: "0".into(),
            refresh_interval: 1500,
            total_buffers_size: 0,
            total_peer_connections: connected_peers,
            total_queued_size: 0,
            total_wasted_session: 0,
            dl_rate_limit: limits.download_limit,
            up_rate_limit: limits.upload_limit,
            use_alt_speed_limits: limits.speed_limits_mode,
            write_cache_overload: "0".into(),
        },
    };
    (StatusCode::OK, Json(resp)).into_response()
}
