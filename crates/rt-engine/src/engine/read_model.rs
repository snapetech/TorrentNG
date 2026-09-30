//! Bounded engine statistics, health, and diagnostic read projections.
//! Detached refreshes return immutable results to the actor for publication.

use super::*;

impl Engine {
    /// Serve the last complete snapshot immediately and refresh it outside the
    /// actor. Stats are observability data; waiting on SQLite, DHT, or a slow
    /// torrent task here would let a dashboard request delay control-plane
    /// commands for the whole engine.
    pub(super) fn request_engine_stats(&mut self) -> CmdResult<EngineStats> {
        if let Some(cache) = self.services.stats_cache.as_ref() {
            if cache.generated_at.elapsed() <= ENGINE_STATS_CACHE_TTL {
                return Ok(cache.stats.clone());
            }
        } else {
            let stats = self.fast_engine_stats();
            self.services.stats_cache = Some(subsystems::EngineStatsCache {
                generated_at: Instant::now(),
                stats,
                refresh_started_at: None,
            });
        }
        self.start_engine_stats_refresh();
        Ok(self
            .services
            .stats_cache
            .as_ref()
            .expect("fast stats initializes the cache")
            .stats
            .clone())
    }

    /// Build a bounded first-response snapshot without touching SQLite or
    /// querying any child actor. The detached collector replaces it shortly
    /// after with runtime counters and compatibility aggregates.
    pub(super) fn fast_engine_stats(&self) -> EngineStats {
        // This is the actor's immediate response path. A concurrent registry
        // mutation must not turn an observability request into an actor-wide
        // wait; the detached collector will obtain a complete view later.
        let registry_stats = self
            .registry
            .try_read()
            .map(|registry| registry.stats())
            .unwrap_or_default();
        let mut stats = engine_stats_from_registry(registry_stats);
        let tier_counts = self.runtime.tier_controller.tier_counts();
        apply_activity_tier_stats(
            &mut stats,
            tier_counts,
            registry_stats.torrents_total,
            self.runtime.tier_controller.dormant_heap_bytes() as u64,
        );
        let storage_jobs = self.services.storage_jobs.stats();
        apply_storage_job_stats(
            &mut stats,
            storage_jobs.inflight as u64,
            storage_jobs.queue_depth as u64,
            storage_jobs.capacity as u64,
            storage_jobs.worker_count as u64,
            self.services.storage_jobs.is_healthy() as u64,
        );
        finalize_engine_stats_resources(
            &mut stats,
            self.services.resources.snapshot(),
            current_storage_frame_stats(),
            self.config.memory.pressure_constrained_pct,
            self.config.memory.pressure_critical_pct,
        );
        stats
    }

    pub(super) fn start_engine_stats_refresh(&mut self) {
        let refresh_is_live = self.services.stats_cache.as_ref().is_some_and(|cache| {
            cache
                .refresh_started_at
                .is_some_and(|started| started.elapsed() <= ENGINE_STATS_REFRESH_STALE_AFTER)
        });
        if refresh_is_live {
            return;
        }
        let Some(cache) = self.services.stats_cache.as_mut() else {
            return;
        };
        let started_at = Instant::now();
        cache.refresh_started_at = Some(started_at);

        let storage_jobs = self.services.storage_jobs.stats();
        let task_count = self.runtime.torrent_chans.len();
        let task_channels = if task_count <= MAX_ENGINE_STATS_TASKS {
            self.runtime
                .torrent_chans
                .iter()
                .map(|(info_hash, tx)| (info_hash.clone(), tx.clone()))
                .collect()
        } else {
            warn!(
                component = "engine",
                operation = "collect_runtime_stats",
                task_count,
                maximum = MAX_ENGINE_STATS_TASKS,
                "skipping detailed torrent runtime stats above bounded task cap"
            );
            Vec::new()
        };
        let input = EngineStatsRefreshInput {
            registry: Arc::clone(&self.registry),
            db: self.db_executor(),
            task_count: task_count as u64,
            task_channels,
            dht_tx: self.services.dht_tx.clone(),
            storage_jobs_inflight: storage_jobs.inflight as u64,
            storage_jobs_queue_depth: storage_jobs.queue_depth as u64,
            storage_jobs_capacity: storage_jobs.capacity as u64,
            storage_workers: storage_jobs.worker_count as u64,
            storage_workers_healthy: self.services.storage_jobs.is_healthy() as u64,
            resources: self.services.resources.clone(),
            storage_frame: current_storage_frame_stats(),
            tier_counts: self.runtime.tier_controller.tier_counts(),
            dormant_runtime_heap_bytes: self.runtime.tier_controller.dormant_heap_bytes() as u64,
            pressure_constrained_pct: self.config.memory.pressure_constrained_pct,
            pressure_critical_pct: self.config.memory.pressure_critical_pct,
        };
        let cmd_tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let command = match timeout(
                ENGINE_STATS_REFRESH_DEADLINE,
                collect_engine_stats_background(input),
            )
            .await
            {
                Ok(Ok(stats)) => EngineCmd::StatsRefreshComplete {
                    started_at,
                    stats: Box::new(stats),
                },
                Ok(Err(error)) => EngineCmd::StatsRefreshFailed { started_at, error },
                Err(_) => EngineCmd::StatsRefreshFailed {
                    started_at,
                    error: format!(
                        "engine stats refresh exceeded {} ms",
                        ENGINE_STATS_REFRESH_DEADLINE.as_millis()
                    ),
                },
            };
            let _ = timeout(ENGINE_COMMAND_SEND_TIMEOUT, cmd_tx.send(command)).await;
        });
    }

    /// Full synchronous collector retained for direct engine tests and for
    /// callers that exercise the internal actor in isolation. Public command
    /// handling uses `request_engine_stats`, which never awaits this path.
    #[cfg(test)]
    pub(super) async fn engine_stats(&mut self) -> CmdResult<EngineStats> {
        if let Some(cache) = self.services.stats_cache.as_ref() {
            if cache.generated_at.elapsed() <= ENGINE_STATS_CACHE_TTL {
                return Ok(cache.stats.clone());
            }
        }
        let mut stats = EngineStats::default();
        let (registry_stats, mut states) = {
            let reg = self.registry.read().await;
            let registry_stats = reg.stats();
            // Only promoted tasks need actor runtime queries below. Dormant
            // rows are represented by the registry aggregate and do not
            // force a 100k-entry traversal on every stats refresh.
            let states = self
                .runtime
                .torrent_chans
                .keys()
                .filter_map(|info_hash| {
                    reg.get(info_hash)
                        .map(|entry| (info_hash.clone(), entry.state))
                })
                .collect::<HashMap<_, _>>();
            (registry_stats, states)
        };
        stats.torrents_total = registry_stats.torrents_total;
        stats.torrents_seeding = registry_stats.torrents_seeding;
        stats.torrents_downloading = registry_stats.torrents_downloading;
        stats.torrents_paused = registry_stats
            .torrents_stopped
            .saturating_add(registry_stats.torrents_paused);
        stats.torrents_checking = registry_stats.torrents_checking;
        stats.torrents_queued = registry_stats.torrents_queued;
        stats.torrents_error = registry_stats.torrents_error;
        stats.torrents_metadata_pending = registry_stats.torrents_metadata_pending;
        stats.bytes_uploaded = registry_stats.bytes_uploaded;
        stats.bytes_downloaded = registry_stats.bytes_downloaded;
        stats.bytes_left = registry_stats.bytes_left;
        let now = Instant::now();
        // These aggregate queries are compatibility/read-model work, not
        // actor ordering. Never make the engine actor wait on the database
        // mutex or execute SQLite while it is responsible for command
        // dispatch. A busy writer causes a bounded stats miss instead of
        // stalling every torrent command behind the stats cache refresh.
        let db = Arc::clone(&self.db);
        let tracker_counts = match tokio::task::spawn_blocking(move || {
            let db = db
                .try_lock()
                .map_err(|_| "database busy while collecting engine stats".to_owned())?;
            let jobs = rt_db::count_active_jobs(&db).map_err(|e| e.to_string())?;
            let trackers = rt_db::torrent_tracker_status_counts(&db).map_err(|e| e.to_string())?;
            Ok::<_, String>((jobs, trackers))
        })
        .await
        {
            Ok(Ok((jobs, trackers))) => {
                stats.jobs_active = jobs;
                trackers
            }
            Ok(Err(error)) => {
                warn!(
                    component = "engine",
                    operation = "collect_database_stats",
                    result = "unavailable",
                    error = %error,
                    "engine database aggregate stats unavailable"
                );
                return Err(error);
            }
            Err(error) => {
                warn!(
                    component = "engine",
                    operation = "collect_database_stats",
                    result = "worker_failed",
                    error = %crate::task_join_error_summary("engine database stats worker", &error),
                    "engine database aggregate stats worker failed"
                );
                return Err(crate::task_join_error_summary(
                    "engine database stats worker",
                    &error,
                ));
            }
        };
        let storage_jobs = self.services.storage_jobs.stats();
        stats.storage_jobs_inflight = storage_jobs.inflight as u64;
        stats.storage_jobs_queue_depth = storage_jobs.queue_depth as u64;
        stats.storage_jobs_capacity = storage_jobs.capacity as u64;
        stats.storage_workers = storage_jobs.worker_count as u64;
        stats.storage_workers_healthy = self.services.storage_jobs.is_healthy() as u64;
        stats.trackers_total = tracker_counts.total;
        stats.trackers_working = tracker_counts.working;
        stats.trackers_warning = tracker_counts.warning;
        stats.trackers_error = tracker_counts.error;
        if let Some(dht_tx) = &self.services.dht_tx {
            let (reply, rx) = tokio::sync::oneshot::channel();
            if dht_tx.try_send(DhtCommand::GetStats { reply }).is_ok() {
                match timeout(Duration::from_millis(250), rx).await {
                    Ok(Ok(dht)) => {
                        stats.dht_routing_nodes = dht.routing_nodes;
                        stats.dht_announced_peer_sets = dht.announced_peer_sets;
                        stats.dht_announced_peers = dht.announced_peers;
                        stats.dht_tracked_torrents = dht.tracked_torrents;
                        stats.dht_tracked_torrents_cap = dht.tracked_torrents_cap;
                        stats.dht_tracked_torrents_rejected = dht.tracked_torrents_rejected;
                        stats.dht_outstanding_requests = dht.outstanding_requests;
                        stats.dht_queried_nodes = dht.queried_nodes;
                    }
                    Ok(Err(_)) => {}
                    Err(_) => warn!(
                        component = "engine",
                        operation = "collect_runtime_stats",
                        target = "dht",
                        duration_ms = 250_u64,
                        result = "timeout",
                        "timed out collecting DHT runtime stats"
                    ),
                }
            }
        }
        // Query task actors in bounded parallelism. The old sequential loop
        // made a single slow/dead torrent add 250 ms to every other torrent;
        // at 100k tasks that was an outage-sized stats request.
        let task_channels = self
            .runtime
            .torrent_chans
            .iter()
            .map(|(info_hash, tx)| (info_hash.clone(), tx.clone()))
            .collect::<Vec<_>>();
        let runtime_results = timeout(
            ENGINE_STATS_TASK_QUERY_DEADLINE,
            stream::iter(task_channels.into_iter().map(|(info_hash, tx)| async move {
                let (reply, rx) = tokio::sync::oneshot::channel();
                let send_result = timeout(
                    ENGINE_COMMAND_SEND_TIMEOUT,
                    tx.send(TorrentCmd::GetRuntimeStats { reply }),
                )
                .await;
                if !matches!(send_result, Ok(Ok(()))) {
                    return (info_hash, None);
                }
                match timeout(ENGINE_STATS_TASK_QUERY_DEADLINE, rx).await {
                    Ok(Ok(runtime)) => (info_hash, Some(runtime)),
                    Ok(Err(_)) => (info_hash, None),
                    Err(_) => {
                        warn!(
                            component = "engine",
                            operation = "collect_runtime_stats",
                            target = "torrent",
                            torrent = %info_hash,
                            duration_ms = ENGINE_STATS_TASK_QUERY_DEADLINE.as_millis(),
                            result = "timeout",
                            "timed out collecting torrent runtime stats"
                        );
                        (info_hash, None)
                    }
                }
            }))
            .buffer_unordered(64)
            .collect::<Vec<_>>(),
        )
        .await
        .unwrap_or_default();

        let mut seen_shared_storage_resources = HashMap::new();
        for (info_hash, runtime) in runtime_results {
            let Some(state) = states.remove(&info_hash) else {
                continue;
            };
            if let Some(runtime) = runtime {
                self.runtime.tier_controller.apply_input(
                    info_hash.clone(),
                    TierInput {
                        state,
                        connected_peers: runtime.connected_peers as usize,
                        outstanding_requests: runtime.outstanding_requests as usize,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: self.runtime.tier_last_active.get(&info_hash).copied(),
                        now,
                    },
                );
                stats.add_torrent_runtime_with_shared_storage_resources(
                    info_hash,
                    runtime,
                    &mut seen_shared_storage_resources,
                );
            } else if self.runtime.tier_controller.tier(&info_hash).is_none() {
                self.runtime.tier_controller.apply_input(
                    info_hash.clone(),
                    TierInput {
                        state,
                        connected_peers: 0,
                        outstanding_requests: 0,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: self.runtime.tier_last_active.get(&info_hash).copied(),
                        now,
                    },
                );
            }
        }
        for (info_hash, state) in states {
            if self.runtime.tier_controller.tier(&info_hash).is_none() {
                self.runtime.tier_controller.apply_input(
                    info_hash.clone(),
                    TierInput {
                        state,
                        connected_peers: 0,
                        outstanding_requests: 0,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: self.runtime.tier_last_active.get(&info_hash).copied(),
                        now,
                    },
                );
            }
        }
        // The channel map is the authoritative set of promoted torrent
        // actors. Runtime-stat replies are best-effort and must not make this
        // gauge lag behind a demotion or disappear when one actor is slow.
        stats.torrent_tasks_active = self.runtime.torrent_chans.len() as u64;
        let [dormant, warm, hot] = self.runtime.tier_controller.tier_counts();
        let tracked = dormant.saturating_add(warm).saturating_add(hot);
        stats.torrents_activity_dormant =
            dormant as u64 + registry_stats.torrents_total.saturating_sub(tracked as u64);
        stats.torrents_activity_warm = warm as u64;
        stats.torrents_activity_hot = hot as u64;
        stats.dormant_runtime_heap_bytes = self.runtime.tier_controller.dormant_heap_bytes() as u64;
        let mut resources = self.services.resources.snapshot();
        let storage_frame = MemoryClass::StorageFrame as usize;
        if let Some(storage) = current_storage_frame_stats() {
            resources.classes[storage_frame].cap_bytes = storage.cap_bytes;
            resources.classes[storage_frame].used_bytes = storage.in_use_bytes;
            resources.classes[storage_frame].denied_allocations = storage.denied_allocations;
        }
        let piece_assembly = MemoryClass::PieceAssembly as usize;
        resources.classes[piece_assembly].used_bytes = resources.classes[piece_assembly]
            .used_bytes
            .max(stats.piece_assembly_bytes);
        let peer_buffer = MemoryClass::PeerBuffer as usize;
        let governor_peer_buffer_bytes = resources.classes[peer_buffer].used_bytes;
        resources.classes[peer_buffer].used_bytes = governor_peer_buffer_bytes
            .saturating_add(stats.peer_rx_buffer_bytes)
            .saturating_add(stats.peer_tx_buffer_bytes)
            .saturating_add(stats.peer_command_queue_bytes);
        let tracker_peers = MemoryClass::TrackerPeers as usize;
        // Live tracker caches now own governor leases. Keep the larger of
        // that authoritative live total and the collected runtime estimate
        // so a slow stats reply cannot hide retained cache memory without
        // double-counting the same bytes.
        resources.classes[tracker_peers].used_bytes = resources.classes[tracker_peers]
            .used_bytes
            .max(stats.tracker_peer_cache_bytes);
        let dht_table = MemoryClass::DhtTable as usize;
        resources.classes[dht_table].used_bytes = stats
            .dht_routing_nodes
            .saturating_mul(64)
            .saturating_add(stats.dht_announced_peers.saturating_mul(32))
            .saturating_add(stats.dht_queried_nodes.saturating_mul(32))
            .saturating_add(stats.dht_outstanding_requests.saturating_mul(64));
        let queued_disk = MemoryClass::QueuedDisk as usize;
        resources.classes[queued_disk].used_bytes = stats.storage_queued_disk_bytes;
        resources.total_used_bytes = resources
            .classes
            .iter()
            .fold(0u64, |total, class| total.saturating_add(class.used_bytes));
        resources.pressure = memory_pressure_for(
            resources.total_used_bytes,
            resources.total_cap_bytes,
            self.config.memory.pressure_constrained_pct,
            self.config.memory.pressure_critical_pct,
        );
        stats.resources = Some(resources);
        self.services.stats_cache = Some(subsystems::EngineStatsCache {
            generated_at: Instant::now(),
            stats: stats.clone(),
            refresh_started_at: None,
        });
        Ok(stats)
    }

    pub(super) async fn engine_subsystem_health(&self) -> CmdResult<EngineSubsystemHealth> {
        #[cfg(not(test))]
        let db_worker_healthy = self.db_worker.is_healthy();
        #[cfg(test)]
        let db_worker_healthy = true;
        let dht_enabled = self.services.dht_tx.is_some();
        let dht_healthy = if let Some(dht_tx) = &self.services.dht_tx {
            if dht_tx.is_closed() {
                false
            } else {
                let (reply, response) = oneshot::channel();
                if dht_tx.try_send(DhtCommand::GetStats { reply }).is_err() {
                    false
                } else {
                    matches!(
                        timeout(Duration::from_millis(250), response).await,
                        Ok(Ok(_))
                    )
                }
            }
        } else {
            true
        };
        Ok(EngineSubsystemHealth {
            db_worker_healthy,
            storage_workers_healthy: self.services.storage_jobs.is_healthy(),
            dht_enabled,
            dht_healthy,
        })
    }

    pub(super) async fn diagnose_torrent_inner(
        &self,
        info_hash: &str,
    ) -> CmdResult<TorrentDiagnostic> {
        let (state, bytes_left) = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            (entry.state, entry.amount_left)
        };
        let info_hash_for_db = info_hash.to_owned();
        let (is_private, tracker_counts, active_jobs) = self
            .run_db("diagnose_torrent", move |db| {
                let is_private =
                    rt_db::torrent_is_private(db, &info_hash_for_db).map_err(|e| e.to_string())?;
                let tracker_counts =
                    rt_db::torrent_tracker_status_counts_for_torrent(db, &info_hash_for_db)
                        .map_err(|error| error.to_string())?;
                let active_jobs = rt_db::list_active_jobs(db)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|job| {
                        job.affected_torrents
                            .iter()
                            .any(|hash| hash == &info_hash_for_db)
                    })
                    .count();
                Ok::<_, String>((is_private, tracker_counts, active_jobs))
            })
            .await?;
        let tracker_errors = usize::try_from(tracker_counts.error).unwrap_or(usize::MAX);
        let tracker_warnings = usize::try_from(tracker_counts.warning).unwrap_or(usize::MAX);
        let mut reasons = Vec::new();
        let mut next_actions = Vec::new();
        if state == TorrentState::Seeding && bytes_left == 0 {
            reasons.push("torrent is already seeding".to_owned());
        } else {
            match state {
                TorrentState::Paused | TorrentState::Stopped => {
                    reasons.push("torrent is paused or stopped".to_owned());
                    next_actions.push("resume the torrent".to_owned());
                }
                TorrentState::Checking => {
                    reasons.push("torrent is currently checking pieces".to_owned());
                    next_actions.push("wait for the active recheck job to finish".to_owned());
                }
                TorrentState::MetadataPending => {
                    reasons.push("torrent is waiting for metadata".to_owned());
                    next_actions
                        .push("wait for metadata peers or add the .torrent file".to_owned());
                }
                TorrentState::Downloading | TorrentState::Queued => {
                    reasons.push(format!("{bytes_left} bytes are still missing"));
                }
                TorrentState::Error => {
                    reasons.push("torrent is in an error state".to_owned());
                    next_actions.push("inspect tracker and storage errors".to_owned());
                }
                TorrentState::Seeding => {}
            }
            if active_jobs > 0 {
                reasons.push(format!("{active_jobs} active job(s) affect this torrent"));
            }
            if tracker_errors > 0 {
                reasons.push(format!("{tracker_errors} tracker(s) are in error state"));
                next_actions.push("check tracker failure_reason values".to_owned());
            }
            if tracker_warnings > 0 {
                reasons.push(format!("{tracker_warnings} tracker(s) reported warnings"));
            }
            if is_private && tracker_counts.total == 0 {
                reasons.push("private torrent has no persisted trackers".to_owned());
                next_actions.push("add a private tracker before expecting peers".to_owned());
            }
        }
        if next_actions.is_empty() && state != TorrentState::Seeding {
            next_actions.push("inspect torrent files, trackers, and active jobs".to_owned());
        }
        Ok(TorrentDiagnostic {
            info_hash: info_hash.to_owned(),
            state: state.as_str().to_owned(),
            is_private,
            bytes_left,
            active_jobs,
            tracker_errors,
            tracker_warnings,
            reasons,
            next_actions,
        })
    }
}
