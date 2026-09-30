//! Startup restore, interrupted-job recovery, and durable projection repair.
//! Recovery completes before the actor accepts ordinary torrent mutations.

use super::*;

impl Engine {
    pub(super) async fn load_persisted_torrents(&mut self) -> anyhow::Result<()> {
        let (mut rows, metadata_v2_hashes) = self
            .run_db("load_persisted_torrents", |db| {
                let rows = rt_db::list_all(db).map_err(|error| error.to_string())?;
                let metadata_v2_hashes = rt_db::list_torrent_metadata_v2_hashes(db)
                    .map_err(|error| error.to_string())?;
                Ok((rows, metadata_v2_hashes))
            })
            .await
            .map_err(anyhow::Error::msg)?;
        self.reconcile_registry_projection(&rows).await?;
        self.reconcile_persisted_projections(&mut rows).await?;
        // Resolve storage authority once for the entire restore. The old
        // path rebuilt/canonicalized the configured roots for every row,
        // turning a large restore into an avoidable O(torrents * roots)
        // filesystem/SQLite loop.
        let storage_authority = self
            .configured_storage_authority_async()
            .await
            .map_err(anyhow::Error::msg)?;
        self.repair_missing_torrent_tracker_rows_async(&rows)
            .await?;
        let tracker_deadlines = self
            .run_db("load_persisted_tracker_deadlines", |db| {
                let deadlines = rt_db::list_torrent_tracker_deadlines(db)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .collect::<HashMap<_, _>>();
                Ok(deadlines)
            })
            .await
            .map_err(anyhow::Error::msg)?;
        let restore_now = Instant::now();
        let restore_now_unix = unix_now_i64();
        // The rows stay alive for the entire restore. Borrow their save-path
        // strings as cache keys instead of cloning up to one 16 KiB path per
        // distinct torrent, which otherwise duplicates a large startup
        // projection before any task is spawned.
        let mut authorized_save_paths = HashMap::<&str, Result<(), String>>::new();
        let mut dormant_restored = 0_u64;

        for row in &rows {
            let authorization = authorized_save_paths
                .entry(row.save_path.as_str())
                .or_insert_with(|| {
                    storage_authority
                        .authorize_path(Path::new(&row.save_path))
                        .map_err(|error| error.to_string())
                });
            if let Err(error) = authorization {
                warn!(
                    component = "storage",
                    operation = "restore_torrent",
                    torrent = %row.info_hash,
                    save_path = %row.save_path,
                    result = "rejected",
                    error = %error,
                    "skipping persisted torrent outside configured storage roots"
                );
                continue;
            }
            let state = state_from_str(&row.state);
            let mut start_task = state != TorrentState::Error
                && (!self.config.runtime.torrent_tiers_enabled
                    || should_start_task_on_restore(state));
            if self.is_metadata_placeholder_row(row) {
                let entry = entry_from_row(row);
                let mut reg = self.registry.write().await;
                if let Err(e) = reg.add(entry) {
                    warn!(
                        component = "engine",
                        operation = "restore_metadata_pending_registry",
                        torrent = %row.info_hash,
                        result = "error",
                        error = %e,
                        "failed to restore metadata-pending registry entry"
                    );
                }
                drop(reg);
                self.runtime.tier_controller.apply_input(
                    row.info_hash.clone(),
                    TierInput {
                        state,
                        connected_peers: 0,
                        outstanding_requests: 0,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: start_task.then_some(Instant::now()),
                        now: Instant::now(),
                    },
                );
                let mut restored = false;
                if let Ok(info_hash) = metadata_info_hash_with_v2(
                    &row.info_hash,
                    metadata_v2_hashes.get(&row.info_hash).copied(),
                ) {
                    if start_task {
                        match prepare_metadata_task_memory(
                            &self.services.resources,
                            row.trackers.clone(),
                            self.config.network.max_peers,
                        ) {
                            Ok((trackers, task_memory)) => {
                                let _tx = self.spawn_metadata_task(
                                    info_hash,
                                    row.info_hash.clone(),
                                    trackers,
                                    matches!(
                                        state,
                                        TorrentState::Paused
                                            | TorrentState::Stopped
                                            | TorrentState::Queued
                                    ),
                                    state,
                                    task_memory,
                                );
                                // A paused metadata-pending torrent must not
                                // start DHT discovery during restore. If the
                                // torrent is resumed, the normal resume path
                                // registers it after the state transition.
                                // Metadata may still reveal a private torrent
                                // later; completion also removes the
                                // provisional registration in that case.
                                if should_register_dht_on_restore(state) {
                                    self.register_dht_torrent(
                                        info_hash.wire_hash(),
                                        &row.info_hash,
                                    )
                                    .await;
                                }
                            }
                            Err(error) => {
                                warn!(
                                    component = "memory",
                                    operation = "restore_metadata_task",
                                    torrent = %row.info_hash,
                                    result = "deferred",
                                    error = %error,
                                    "restoring metadata-pending torrent without a runtime task because memory is unavailable"
                                );
                            }
                        }
                    }
                    self.append_session_event(
                        Some(&row.info_hash),
                        EVENT_TORRENT_RESTORED,
                        Some("metadata-pending torrent restored"),
                        serde_json::json!({
                            "state": row.state,
                            "metadata_pending": true,
                            "v2_only": row.info_hash.len() == 64,
                        }),
                    );
                    restored = true;
                }
                if !restored {
                    self.append_session_event(
                        Some(&row.info_hash),
                        EVENT_TORRENT_RESTORED,
                        Some("metadata-pending torrent restored"),
                        serde_json::json!({
                            "state": row.state,
                            "metadata_pending": true,
                            "v2_only": row.info_hash.len() == 64,
                        }),
                    );
                }
                continue;
            }
            // A live task allocates its piece index and availability state
            // before it can service commands. Reserve that state while the
            // row is still only a durable projection; if the process-wide
            // class is full, restore the torrent dormant and let a later
            // promotion retry after memory is released.
            let mut piece_index_memory_lease = None;
            if start_task && matches!(row.info_hash.len(), 40 | 64) {
                match usize::try_from(row.piece_count) {
                    Ok(piece_count) => {
                        match reserve_piece_index_memory_for_hash(
                            &self.services.resources,
                            row.info_hash.len(),
                            piece_count,
                        ) {
                            Ok(lease) => piece_index_memory_lease = Some(lease),
                            Err(error) => {
                                warn!(
                                    component = "memory",
                                    operation = "restore_torrent_piece_index",
                                    torrent = %row.info_hash,
                                    result = "deferred",
                                    error = %error,
                                    "restoring torrent dormant because piece-index memory is unavailable"
                                );
                                start_task = false;
                            }
                        }
                    }
                    Err(error) => {
                        warn!(
                            component = "engine",
                            operation = "restore_torrent_piece_index",
                            torrent = %row.info_hash,
                            result = "deferred",
                            error = %error,
                            "restoring torrent dormant because persisted piece count is invalid"
                        );
                        start_task = false;
                    }
                }
            }
            // A dormant row only needs its durable projection. Do not read or
            // parse the metainfo blob until a lifecycle command, peer, or
            // tracker deadline promotes it. This is the key restart property
            // for the 100k target: cold rows remain O(1) registry state and
            // do not allocate a task, scheduler, or parsed metainfo tree.
            if !start_task {
                let entry = dormant_entry_from_row(row);
                {
                    let mut reg = self.registry.write().await;
                    if let Err(e) = reg.add_dormant(entry) {
                        warn!(
                            component = "engine",
                            operation = "restore_dormant_registry",
                            torrent = %row.info_hash,
                            result = "error",
                            error = %e,
                            "failed to restore dormant registry entry"
                        );
                        continue;
                    }
                }
                self.runtime.tier_controller.apply_input(
                    row.info_hash.clone(),
                    TierInput {
                        state,
                        connected_peers: 0,
                        outstanding_requests: 0,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: None,
                        now: restore_now,
                    },
                );
                if state == TorrentState::Seeding {
                    if let Some(deadline) =
                        tracker_deadlines.get(&row.info_hash).and_then(|deadline| {
                            unix_deadline_to_instant(*deadline, restore_now_unix, restore_now)
                        })
                    {
                        self.runtime
                            .tier_controller
                            .schedule_tracker_check(row.info_hash.clone(), deadline);
                    }
                }
                let tracker_deadline = tracker_deadlines.get(&row.info_hash).and_then(|deadline| {
                    unix_deadline_to_instant(*deadline, restore_now_unix, restore_now)
                });
                self.runtime.tier_controller.set_dormant_snapshot(
                    row.info_hash.clone(),
                    dormant_snapshot_from_row(row, state, tracker_deadline),
                );
                dormant_restored = dormant_restored.saturating_add(1);
                continue;
            }
            let blob_path = torrent_blob_path(&self.config, &row.info_hash);
            let mut parse_memory_lease = match reserve_torrent_parse_memory_from_blob(
                &self.config,
                &self.services.resources,
                &row.info_hash,
                "restore",
            ) {
                Ok(lease) => lease,
                Err(error) => {
                    warn!(
                        component = "memory",
                        operation = "restore_torrent_parse",
                        torrent = %row.info_hash,
                        result = "deferred",
                        error = %error,
                        "restoring torrent in error state because parser memory is unavailable"
                    );
                    self.restore_persisted_error_projection(
                        row,
                        "runtime",
                        &blob_path,
                        format!("torrent parser memory admission was unavailable: {error}"),
                        false,
                    )
                    .await?;
                    continue;
                }
            };
            let raw = match rt_storage::read_file_no_follow_limited(&blob_path, MAX_TORRENT_BYTES) {
                Ok(raw) => raw,
                Err(e) => {
                    warn!(
                        component = "engine",
                        operation = "load_persisted_torrent_metadata",
                        torrent = %row.info_hash,
                        result = "error",
                        error = %e,
                        "persisted torrent metadata missing"
                    );
                    self.restore_persisted_error_projection(
                        row,
                        "torrent_blob",
                        &blob_path,
                        format!("failed to read persisted torrent metadata: {e}"),
                        false,
                    )
                    .await?;
                    continue;
                }
            };
            let meta = match parse_torrent_with_memory_lease(&raw, &mut parse_memory_lease) {
                Ok(meta) => meta,
                Err(e) => {
                    warn!(
                        component = "engine",
                        operation = "parse_persisted_torrent",
                        torrent = %row.info_hash,
                        result = "error",
                        error = %e,
                        "failed to parse persisted torrent"
                    );
                    self.restore_persisted_error_projection(
                        row,
                        "torrent_blob",
                        &blob_path,
                        format!("persisted torrent metadata is invalid: {e}"),
                        true,
                    )
                    .await?;
                    continue;
                }
            };
            // parse_torrent stores its own bounded raw copy in the parsed
            // metadata. Do not keep the file-read buffer alive while the
            // restore path validates projections and constructs a live task.
            drop(raw);
            let actual_piece_count = match &meta {
                TorrentMeta::V1(meta) => Some(meta.pieces.len()),
                TorrentMeta::Hybrid(meta, _) => Some(meta.pieces.len()),
                TorrentMeta::V2(meta) => usize::try_from(meta.piece_count()).ok(),
            };
            if let Some(actual_piece_count) = actual_piece_count {
                let actual_piece_count = i64::try_from(actual_piece_count).unwrap_or(i64::MAX);
                if row.piece_count != actual_piece_count {
                    self.restore_persisted_error_projection(
                        row,
                        "torrent_blob",
                        &blob_path,
                        format!(
                            "persisted piece count {actual_piece_count} does not match row {}",
                            row.piece_count
                        ),
                        false,
                    )
                    .await?;
                    continue;
                }
            }
            self.repair_torrent_file_projection(&row.info_hash, &meta)
                .await?;
            let info_hash_hex = meta_info_hash_hex(&meta);
            if info_hash_hex != row.info_hash {
                warn!(
                    component = "engine",
                    operation = "load_persisted_torrent",
                    row_hash = %row.info_hash,
                    meta_hash = %info_hash_hex,
                    result = "error",
                    "persisted torrent hash mismatch"
                );
                self.restore_persisted_error_projection(
                    row,
                    "torrent_blob",
                    &blob_path,
                    format!(
                        "persisted torrent hash {info_hash_hex} does not match row {}",
                        row.info_hash
                    ),
                    true,
                )
                .await?;
                continue;
            }
            let torrent_metadata_memory_lease = if start_task {
                match reserve_torrent_metadata_memory_for_meta(&self.services.resources, &meta) {
                    Ok(lease) => lease,
                    Err(error) => {
                        warn!(
                            component = "memory",
                            operation = "restore_torrent_metadata",
                            torrent = %row.info_hash,
                            result = "deferred",
                            error = %error,
                            "restoring torrent in error state because persistent metadata memory is unavailable"
                        );
                        self.restore_persisted_error_projection(
                            row,
                            "runtime",
                            &blob_path,
                            format!(
                                "persistent torrent metadata memory admission was unavailable: {error}"
                            ),
                            false,
                        )
                        .await?;
                        continue;
                    }
                }
            } else {
                None
            };
            let entry = entry_from_row(row);
            {
                let mut reg = self.registry.write().await;
                if let Err(e) = reg.add(entry) {
                    warn!(
                        component = "engine",
                        operation = "restore_registry_entry",
                        torrent = %row.info_hash,
                        result = "error",
                        error = %e,
                        "failed to restore registry entry"
                    );
                    continue;
                }
            }

            self.runtime.tier_controller.apply_input(
                row.info_hash.clone(),
                TierInput {
                    state,
                    connected_peers: 0,
                    outstanding_requests: 0,
                    inbound_peer: false,
                    tracker_due: false,
                    last_active: start_task.then_some(Instant::now()),
                    now: Instant::now(),
                },
            );
            let is_private = meta.is_private();
            let v2_only = matches!(meta, TorrentMeta::V2(_));
            let dht_info_hash = meta_dht_info_hash(&meta);
            if start_task {
                let Some(piece_index_memory_lease) = piece_index_memory_lease else {
                    self.restore_persisted_error_projection(
                        row,
                        "runtime",
                        &blob_path,
                        "piece-index memory admission was unavailable".to_owned(),
                        false,
                    )
                    .await?;
                    continue;
                };
                let Some(torrent_metadata_memory_lease) = torrent_metadata_memory_lease else {
                    self.restore_persisted_error_projection(
                        row,
                        "runtime",
                        &blob_path,
                        "persistent torrent metadata memory admission was lost".to_owned(),
                        false,
                    )
                    .await?;
                    continue;
                };
                let _tx = self
                    .spawn_torrent_task_for_meta(
                        row.info_hash.clone(),
                        meta,
                        PathBuf::from(&row.save_path),
                        matches!(
                            state,
                            TorrentState::Paused | TorrentState::Stopped | TorrentState::Queued
                        ),
                        state,
                        piece_index_memory_lease,
                        torrent_metadata_memory_lease,
                    )
                    .await;
                if !is_private && should_register_dht_on_restore(state) {
                    self.register_dht_torrent(dht_info_hash, &row.info_hash)
                        .await;
                }
            }
            self.append_session_event(
                Some(&row.info_hash),
                EVENT_TORRENT_RESTORED,
                Some("torrent restored from database"),
                serde_json::json!({
                    "state": row.state,
                    "private": is_private,
                    "v2_only": v2_only,
                }),
            );
            info!(
                component = "engine",
                operation = "restore_torrent",
                torrent = %row.info_hash,
                state = %row.state,
                task_started = start_task,
                result = "ok",
                "restored persisted torrent"
            );
        }
        if dormant_restored > 0 {
            // One aggregate event keeps a 100k restart from generating a
            // matching 100k-row event-log write burst. Per-torrent detail is
            // already available in the registry and durable torrent rows.
            self.append_session_event(
                None,
                EVENT_TORRENT_RESTORED,
                Some("dormant torrents restored from database"),
                serde_json::json!({
                    "count": dormant_restored,
                    "task_started": false,
                    "dormant": true,
                }),
            );
        }
        Ok(())
    }

    pub(super) fn recover_interrupted_jobs_in_db(
        db: &mut Connection,
        now: i64,
        retention: usize,
    ) -> Result<(), rt_db::DbError> {
        let jobs = rt_db::list_active_jobs(db)?;
        for mut job in jobs {
            let previous_state = job.state.clone();
            let recovered_state = match previous_state.as_str() {
                JOB_STATE_RUNNING if job.kind == JOB_KIND_STORAGE_PLAN => Some(JOB_STATE_QUEUED),
                // A storage cancellation is different from an interrupted
                // run: the worker may have left staged filesystem state that
                // must be rolled back before the job becomes terminal. Keep
                // the intent durable so recovery can reattach a pre-cancelled
                // worker instead of resuming the plan.
                JOB_STATE_CANCELLING if job.kind == JOB_KIND_STORAGE_PLAN => {
                    Some(JOB_STATE_CANCELLING)
                }
                JOB_STATE_RUNNING | JOB_STATE_CANCELLING => Some(JOB_STATE_PAUSED),
                _ => None,
            };
            let Some(recovered_state) = recovered_state else {
                continue;
            };
            job.state = recovered_state.to_owned();
            job.updated_at = now;
            let event = rt_db::JobEventRow {
                event_id: None,
                job_id: job.job_id.clone(),
                occurred_at: now,
                kind: "job_recovered".to_owned(),
                message: Some("job recovered after engine restart".to_owned()),
                payload: serde_json::json!({
                "state": recovered_state,
                "previous_state": previous_state,
                })
                .to_string(),
            };
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if job.kind == JOB_KIND_RECHECK && recovered_state == JOB_STATE_PAUSED {
                for info_hash in &job.affected_torrents {
                    let Some(state) = rt_db::torrent_state(&tx, info_hash)? else {
                        continue;
                    };
                    if state != TorrentState::Checking.as_str() {
                        continue;
                    }
                    if !rt_db::update_state_only_in_tx(
                        &tx,
                        info_hash,
                        TorrentState::Paused.as_str(),
                    )? {
                        continue;
                    }
                    rt_db::append_session_event_in_tx(
                        &tx,
                        &rt_db::SessionEventRow {
                            event_id: None,
                            occurred_at: now,
                            info_hash: Some(info_hash.clone()),
                            kind: EVENT_RECHECK_RECOVERED_PAUSED.to_owned(),
                            message: Some(
                                "torrent paused after interrupted recheck was recovered".to_owned(),
                            ),
                            payload: serde_json::json!({
                                "job_id": job.job_id,
                                "from": TorrentState::Checking.as_str(),
                                "to": TorrentState::Paused.as_str(),
                            })
                            .to_string(),
                        },
                    )?;
                }
                rt_db::prune_session_events_in_tx(&tx, retention)?;
            }
            rt_db::upsert_job_in_tx(&tx, &job)?;
            rt_db::append_job_event_in_tx(&tx, &event)?;
            tx.commit()?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn recover_interrupted_jobs(&self) -> anyhow::Result<()> {
        let now = unix_now_i64();
        let mut db = self.db.lock().expect("database mutex poisoned");
        Self::recover_interrupted_jobs_in_db(&mut db, now, self.config.logging.event_retention)?;
        Ok(())
    }

    pub(super) async fn recover_interrupted_jobs_async(&self) -> Result<(), String> {
        let now = unix_now_i64();
        let retention = self.config.logging.event_retention;
        self.run_db("recover_interrupted_jobs", move |db| {
            Self::recover_interrupted_jobs_in_db(db, now, retention)
                .map_err(|error| error.to_string())
        })
        .await
    }

    /// A manual-recovery storage failure is terminal for the worker job, but
    /// it is not safe to treat the affected torrent row as an ordinary
    /// paused/stopped projection on the next process start. Mark those rows
    /// before load_persisted_torrents can decide whether to spawn tasks.
    pub(super) async fn restore_manual_storage_recovery_torrents_async(
        &self,
    ) -> Result<(), String> {
        let retention = self.config.logging.event_retention;
        let restored = self
            .run_db("restore_storage_manual_recovery", move |db| {
                let jobs = rt_db::list_failed_jobs_with_error_prefix(
                    db,
                    JOB_KIND_STORAGE_PLAN,
                    STORAGE_MANUAL_RECOVERY_PREFIX,
                )
                .map_err(|error| error.to_string())?;
                let mut target_jobs = HashMap::<String, Vec<String>>::new();
                for job in jobs {
                    let job_id = job.job_id.clone();
                    let mut targets = job.affected_torrents;
                    if targets.is_empty() {
                        let events = rt_db::list_job_events(db, &job_id, 64)
                            .map_err(|error| error.to_string())?;
                        if let Some(info_hash) = events
                            .iter()
                            .find_map(|event| decode_storage_plan_context(&event.payload))
                            .and_then(|context| {
                                context
                                    .get("info_hash")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_owned)
                            })
                        {
                            targets.push(info_hash);
                        }
                    }
                    for info_hash in targets {
                        target_jobs
                            .entry(info_hash)
                            .or_default()
                            .push(job_id.clone());
                    }
                }

                let mut restorations = Vec::new();
                for (info_hash, job_ids) in target_jobs {
                    let Some(state) =
                        rt_db::torrent_state(db, &info_hash).map_err(|error| error.to_string())?
                    else {
                        // The job may refer to a projection that was already
                        // removed by a separately completed operation.
                        continue;
                    };
                    if state != TorrentState::Error.as_str() {
                        restorations.push((info_hash, job_ids));
                    }
                }
                if restorations.is_empty() {
                    return Ok(0_usize);
                }

                let now = unix_now_i64();
                let tx = db.transaction().map_err(|error| error.to_string())?;
                let mut restored = 0;
                for (info_hash, job_ids) in &restorations {
                    if !rt_db::update_state_only_in_tx(&tx, info_hash, TorrentState::Error.as_str())
                        .map_err(|error| error.to_string())?
                    {
                        continue;
                    }
                    restored += 1;
                    let event = rt_db::SessionEventRow {
                        event_id: None,
                        occurred_at: now,
                        info_hash: Some(info_hash.clone()),
                        kind: "storage_manual_recovery_restored".to_owned(),
                        message: Some(
                            "torrent restored in error state after storage failure".to_owned(),
                        ),
                        payload: serde_json::json!({
                            "state": TorrentState::Error.as_str(),
                            "job_ids": job_ids,
                        })
                        .to_string(),
                    };
                    rt_db::append_session_event_in_tx(&tx, &event)
                        .map_err(|error| error.to_string())?;
                }
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())?;
                Ok(restored)
            })
            .await?;
        if restored > 0 {
            warn!(
                component = "storage_jobs",
                operation = "restore_manual_recovery",
                torrents = restored,
                result = "isolated",
                "affected torrents were restored in error state after an ambiguous storage failure"
            );
        }
        Ok(())
    }

    /// Reconstruct storage plans from their durable queue/checkpoint event and
    /// hand them back to the bounded worker supervisor after restart. The
    /// worker owns the filesystem transaction; the actor only restores
    /// quiesce/resume and save-path finalization callbacks.
    pub(super) async fn resume_recovered_storage_jobs(&mut self) -> anyhow::Result<()> {
        let jobs = self
            .run_db("list_recoverable_storage_jobs", |db| {
                rt_db::list_active_jobs(db).map_err(|error| error.to_string())
            })
            .await
            .map_err(anyhow::Error::msg)?;
        for job in jobs.into_iter().filter(|job| {
            job.kind == JOB_KIND_STORAGE_PLAN
                && matches!(
                    job.state.as_str(),
                    JOB_STATE_QUEUED
                        | JOB_STATE_PAUSED
                        | JOB_STATE_CANCELLING
                        | STORAGE_JOB_STATE_COMMIT_PENDING
                )
        }) {
            let job_id_for_db = job.job_id.clone();
            let (events, first_event) = self
                .run_db("load_storage_job_recovery_events", move |db| {
                    Ok::<_, String>((
                        rt_db::list_job_events(db, &job_id_for_db, 64)
                            .map_err(|error| error.to_string())?,
                        rt_db::first_job_event(db, &job_id_for_db)
                            .map_err(|error| error.to_string())?,
                    ))
                })
                .await
                .map_err(anyhow::Error::msg)?;
            let first_decoded = first_event
                .as_ref()
                .and_then(|event| decode_storage_plan_event(&event.payload));
            let latest_decoded = events
                .iter()
                .find_map(|event| decode_storage_plan_event(&event.payload));
            let Some((operation, event_plan, event_completed_steps, event_context)) =
                latest_decoded.or_else(|| first_decoded.clone())
            else {
                let recovery_targets = first_event
                    .as_ref()
                    .and_then(|event| decode_storage_plan_context(&event.payload))
                    .or_else(|| {
                        events
                            .iter()
                            .find_map(|event| decode_storage_plan_context(&event.payload))
                    })
                    .and_then(|context| {
                        context
                            .get("info_hash")
                            .and_then(serde_json::Value::as_str)
                            .map(|info_hash| vec![info_hash.to_owned()])
                    })
                    .unwrap_or_else(|| job.affected_torrents.clone());
                self.isolate_storage_recovery_failure(
                    &job.job_id,
                    &recovery_targets,
                    "storage plan payload is missing or invalid".to_owned(),
                    "storage plan recovery failed",
                )
                .await;
                continue;
            };
            let Some(plan) = event_plan.or_else(|| {
                first_decoded
                    .as_ref()
                    .and_then(|(_, plan, _, _)| plan.clone())
            }) else {
                let recovery_targets = first_event
                    .as_ref()
                    .and_then(|event| decode_storage_plan_context(&event.payload))
                    .or_else(|| {
                        events
                            .iter()
                            .find_map(|event| decode_storage_plan_context(&event.payload))
                    })
                    .and_then(|context| {
                        context
                            .get("info_hash")
                            .and_then(serde_json::Value::as_str)
                            .map(|info_hash| vec![info_hash.to_owned()])
                    })
                    .unwrap_or_else(|| job.affected_torrents.clone());
                self.isolate_storage_recovery_failure(
                    &job.job_id,
                    &recovery_targets,
                    "storage plan payload is missing or invalid".to_owned(),
                    "storage plan recovery failed",
                )
                .await;
                continue;
            };
            // Checkpoint events intentionally omit the original move context.
            // Read that context from the oldest queued event instead of
            // assuming the newest of the last 64 events contains it; a large
            // plan may have hundreds or thousands of checkpoint records.
            let context = first_event
                .as_ref()
                .and_then(|event| decode_storage_plan_context(&event.payload))
                .or_else(|| {
                    events
                        .iter()
                        .find_map(|event| decode_storage_plan_context(&event.payload))
                })
                .unwrap_or(event_context);
            let recovery_targets = if is_storage_payload_delete_operation(&operation, &context) {
                context
                    .get("info_hash")
                    .and_then(serde_json::Value::as_str)
                    .map(|info_hash| vec![info_hash.to_owned()])
                    .unwrap_or_else(|| job.affected_torrents.clone())
            } else {
                job.affected_torrents.clone()
            };
            if let Err(error) = validate_storage_plan(&plan) {
                self.isolate_storage_recovery_failure(
                    &job.job_id,
                    &recovery_targets,
                    error,
                    "storage plan recovery found an oversized or invalid plan",
                )
                .await;
                continue;
            }
            let durable_quiesced = match recovered_storage_quiesced(&context, &recovery_targets) {
                Ok(quiesced) => quiesced,
                Err(error) => {
                    self.isolate_storage_recovery_failure(
                        &job.job_id,
                        &recovery_targets,
                        error,
                        "storage plan recovery found invalid quiesce context",
                    )
                    .await;
                    continue;
                }
            };
            // `job.checkpoint` stores a count for the legacy job projection,
            // not the step indexes themselves. Prefer the serialized event:
            // completed steps may be a sparse subset (for example `[2]`),
            // and turning that into `0..1` would silently skip the wrong
            // filesystem operation after restart. The count is only a
            // compatibility fallback for a crash between the job-row update
            // and its checkpoint-event insert.
            let checkpoint_steps =
                match recovered_storage_plan_steps(&plan, job.checkpoint, event_completed_steps) {
                    Ok(steps) => steps,
                    Err(error) => {
                        self.isolate_storage_recovery_failure(
                            &job.job_id,
                            &recovery_targets,
                            error,
                            "storage plan recovery found an invalid checkpoint",
                        )
                        .await;
                        continue;
                    }
                };
            // Progress events intentionally carry only the latest completed
            // step to keep large plans from writing a quadratic checkpoint
            // history. The durable row still stores the completed count. If
            // that count covers the whole plan, restore the full prefix before
            // filesystem reconciliation; otherwise the latest-step event is
            // an intentionally sparse checkpoint and must remain sparse.
            let checkpoint_steps = if usize::try_from(job.done).ok() == Some(plan.steps.len())
                && usize::try_from(job.checkpoint).ok() == Some(plan.steps.len())
            {
                (0..plan.steps.len()).collect()
            } else {
                checkpoint_steps
            };
            let roots = match self.configured_storage_roots_for_execution_async().await {
                Ok(roots) => roots,
                Err(error) => {
                    self.isolate_storage_recovery_failure(
                        &job.job_id,
                        &recovery_targets,
                        error,
                        "storage plan recovery could not resolve roots",
                    )
                    .await;
                    continue;
                }
            };
            let checkpoint_steps = if job.state == STORAGE_JOB_STATE_COMMIT_PENDING {
                // A commit-pending move is finalized by the actor immediately
                // below, so it must prove that every filesystem step is live
                // before publishing the new registry path. Ordinary queued
                // and paused jobs are reconciled by the detached worker after
                // this startup hand-off; they must not make the actor walk
                // filesystem state while recovering a queue.
                match rt_storage::reconcile_storage_plan_under_roots(
                    &plan,
                    &roots,
                    &checkpoint_steps,
                ) {
                    Ok(steps) => steps,
                    Err(error) => {
                        self.isolate_storage_recovery_failure(
                            &job.job_id,
                            &recovery_targets,
                            format!("storage plan filesystem reconciliation failed: {error}"),
                            "storage plan recovery found ambiguous filesystem state",
                        )
                        .await;
                        continue;
                    }
                }
            } else {
                checkpoint_steps
            };

            let move_context = if operation == "move" {
                if job.affected_torrents.len() != 1 {
                    self.isolate_storage_recovery_failure(
                        &job.job_id,
                        &recovery_targets,
                        "storage move job must have exactly one affected torrent context"
                            .to_owned(),
                        "storage move recovery failed",
                    )
                    .await;
                    continue;
                }
                let Some(info_hash) = job.affected_torrents.first().cloned() else {
                    self.isolate_storage_recovery_failure(
                        &job.job_id,
                        &recovery_targets,
                        "storage move job has no affected torrent context".to_owned(),
                        "storage move recovery failed",
                    )
                    .await;
                    continue;
                };
                let (old_save_path, save_path, name) =
                    match Self::storage_move_context_for_plan(&plan, &context) {
                        Ok(context) => context,
                        Err(error) => {
                            self.isolate_storage_recovery_failure(
                                &job.job_id,
                                std::slice::from_ref(&info_hash),
                                error,
                                "storage move recovery failed",
                            )
                            .await;
                            continue;
                        }
                    };
                if job.state == STORAGE_JOB_STATE_COMMIT_PENDING {
                    let current_save_path = {
                        let registry = self.registry.read().await;
                        registry
                            .get(&info_hash)
                            .map(|entry| PathBuf::from(&entry.save_path))
                    };
                    let Some(current_save_path) = current_save_path else {
                        self.isolate_storage_recovery_failure(
                            &job.job_id,
                            std::slice::from_ref(&info_hash),
                            format!("torrent {info_hash} is missing during storage move recovery"),
                            "storage move recovery failed",
                        )
                        .await;
                        continue;
                    };
                    if current_save_path != old_save_path && current_save_path != save_path {
                        self.isolate_storage_recovery_failure(
                            &job.job_id,
                            std::slice::from_ref(&info_hash),
                            format!(
                                "storage move recovery registry path {} is neither the plan source {} nor destination {}",
                                current_save_path.display(),
                                old_save_path.display(),
                                save_path.display()
                            ),
                            "storage move recovery found an inconsistent save path",
                        )
                        .await;
                        continue;
                    }
                } else if let Err(error) = self
                    .storage_plan_move_context(&job.affected_torrents, &plan)
                    .await
                {
                    self.isolate_storage_recovery_failure(
                        &job.job_id,
                        std::slice::from_ref(&info_hash),
                        error,
                        "storage move recovery found an inconsistent source path",
                    )
                    .await;
                    continue;
                }
                Some((info_hash, name, old_save_path, save_path))
            } else {
                None
            };
            let delete_info_hash = match recovered_storage_delete_target(&operation, &context) {
                Ok(info_hash) => info_hash,
                Err(error) => {
                    self.isolate_storage_recovery_failure(
                        &job.job_id,
                        &recovery_targets,
                        error,
                        "storage delete recovery failed",
                    )
                    .await;
                    continue;
                }
            };
            // Older delete jobs did not populate affected_torrents, so use
            // the durable context as the authoritative recovery target.
            // The restored torrent must be quiesced before a delete worker is
            // allowed to touch its payload.
            let quiesce_targets = delete_info_hash
                .as_ref()
                .map(|info_hash| vec![info_hash.clone()])
                .unwrap_or_else(|| job.affected_torrents.clone());
            if let Some(info_hash) = delete_info_hash.as_ref() {
                self.runtime
                    .pending_torrent_deletes
                    .insert(info_hash.clone());
            }
            let quiesced = match self
                .quiesce_torrents_for_storage_plan(&quiesce_targets)
                .await
            {
                Ok(live_quiesced) => {
                    let mut quiesced = durable_quiesced;
                    for (info_hash, was_paused) in live_quiesced {
                        quiesced.retain(|(target, _)| target != &info_hash);
                        quiesced.push((info_hash, was_paused));
                    }
                    quiesced
                }
                Err(error) => {
                    if let Some(info_hash) = delete_info_hash.as_ref() {
                        self.runtime.pending_torrent_deletes.remove(info_hash);
                    }
                    let reason =
                        format!("storage plan recovery could not quiesce torrent(s): {error}");
                    self.isolate_storage_recovery_failure(
                        &job.job_id,
                        &quiesce_targets,
                        reason,
                        "storage plan recovery could not quiesce torrent(s)",
                    )
                    .await;
                    continue;
                }
            };
            let quiesced_handles = self.torrent_handles_for_quiesced(&quiesced).await;

            if job.state == STORAGE_JOB_STATE_COMMIT_PENDING {
                if checkpoint_steps.len() != plan.steps.len() {
                    let reason = format!(
                        "storage job claimed commit pending but filesystem reconciliation found {}/{} steps",
                        checkpoint_steps.len(),
                        plan.steps.len()
                    );
                    self.isolate_storage_recovery_failure(
                        &job.job_id,
                        &quiesce_targets,
                        reason,
                        "storage plan commit-pending recovery found incomplete filesystem state",
                    )
                    .await;
                    if let Some(info_hash) = delete_info_hash.as_ref() {
                        self.runtime.pending_torrent_deletes.remove(info_hash);
                    }
                    continue;
                }
                let completed_offset = completed_byte_offset(&plan, &checkpoint_steps);
                if let Some(info_hash) = delete_info_hash {
                    let quiesced_handle = quiesced_handles
                        .iter()
                        .find(|(hash, _)| hash == &info_hash)
                        .map(|(_, handle)| *handle);
                    if let Err(error) = self
                        .finish_storage_delete(StorageDeleteCompletion {
                            job_id: job.job_id.clone(),
                            info_hash,
                            succeeded: true,
                            terminal_state: STORAGE_JOB_STATE_COMMIT_PENDING.to_owned(),
                            error: None,
                            completed_steps: checkpoint_steps,
                            completed_byte_offset: Some(completed_offset),
                            requires_manual_recovery: false,
                            quiesced,
                            quiesced_handle,
                            retry_attempt: 0,
                        })
                        .await
                    {
                        warn!(
                            component = "storage_jobs",
                            operation = "recover_commit_pending_delete",
                            job_id = %job.job_id,
                            result = "error",
                            error = %error,
                            "storage delete commit remains pending after restart"
                        );
                    }
                } else if let Some((info_hash, name, old_save_path, save_path)) = move_context {
                    let quiesced_for_move = quiesced
                        .iter()
                        .find(|(hash, _)| hash == &info_hash)
                        .map(|(_, paused)| *paused);
                    let torrent_handle = quiesced_handles
                        .iter()
                        .find(|(hash, _)| hash == &info_hash)
                        .map(|(_, handle)| *handle);
                    if let Err(error) = self
                        .finish_storage_move(
                            &job.job_id,
                            &info_hash,
                            name,
                            old_save_path,
                            save_path,
                            quiesced_for_move,
                            torrent_handle,
                            true,
                            STORAGE_JOB_STATE_COMMIT_PENDING.to_owned(),
                            None,
                            checkpoint_steps,
                            Some(completed_offset),
                            false,
                            0,
                        )
                        .await
                    {
                        warn!(
                            component = "storage_jobs",
                            operation = "recover_commit_pending_move",
                            job_id = %job.job_id,
                            result = "error",
                            error = %error,
                            "storage move commit remains pending after restart"
                        );
                    }
                } else {
                    self.resume_torrents_after_storage_plan(quiesced, quiesced_handles)
                        .await;
                    if let Err(error) = self
                        .complete_storage_plan_job_async(
                            &job.job_id,
                            &checkpoint_steps,
                            Some(completed_offset),
                        )
                        .await
                    {
                        warn!(
                            component = "storage_jobs",
                            operation = "recover_commit_pending_plan",
                            job_id = %job.job_id,
                            result = "error",
                            error = %error,
                            "storage plan commit remains pending after restart"
                        );
                    }
                }
                continue;
            }

            let (completion, completion_rx) = oneshot::channel();
            let submit_result = {
                #[cfg(not(test))]
                {
                    if job.state == JOB_STATE_CANCELLING {
                        self.services.storage_jobs.submit_cancelled_managed(
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    } else if job.state == JOB_STATE_PAUSED {
                        self.services.storage_jobs.submit_paused_managed(
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    } else {
                        self.services.storage_jobs.submit_managed(
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    }
                }
                #[cfg(test)]
                {
                    if job.state == JOB_STATE_CANCELLING {
                        self.services.storage_jobs.submit_cancelled(
                            Arc::clone(&self.db),
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    } else if job.state == JOB_STATE_PAUSED {
                        self.services.storage_jobs.submit_paused(
                            Arc::clone(&self.db),
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    } else {
                        self.services.storage_jobs.submit(
                            Arc::clone(&self.db),
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    }
                }
            };
            if let Err(error) = submit_result {
                if let Some(info_hash) = delete_info_hash.as_ref() {
                    self.runtime.pending_torrent_deletes.remove(info_hash);
                }
                self.isolate_storage_recovery_failure(
                    &job.job_id,
                    &quiesce_targets,
                    error,
                    "storage plan recovery could not be queued",
                )
                .await;
                continue;
            }

            let cmd_tx = self.cmd_tx.clone();
            let job_id = job.job_id.clone();
            let affected_torrents = quiesced;
            let affected_torrent_handles = quiesced_handles;
            tokio::spawn(async move {
                let completion = completion_rx.await.unwrap_or_else(|_| {
                    StorageJobCompletion::failed_with_manual_recovery(
                        "storage worker completion channel closed",
                        Vec::new(),
                    )
                });
                if let Some(info_hash) = delete_info_hash {
                    let quiesced_handle = affected_torrent_handles
                        .iter()
                        .find(|(hash, _)| hash == &info_hash)
                        .map(|(_, handle)| *handle);
                    // The delete completion is the only actor-side release of
                    // the removal guard and the quiesce. Its producer is
                    // bounded by the storage-job dispatcher, so retain this
                    // stateful handoff until the actor accepts it or closes.
                    send_engine_command_until_actor_stops(
                        cmd_tx.clone(),
                        EngineCmd::StorageDeleteFinished {
                            job_id,
                            info_hash,
                            succeeded: completion.succeeded,
                            terminal_state: completion.state,
                            error: completion.error,
                            completed_steps: completion.completed_steps,
                            completed_byte_offset: completion.completed_byte_offset,
                            requires_manual_recovery: completion.requires_manual_recovery,
                            quiesced: affected_torrents,
                            quiesced_handle,
                            retry_attempt: 0,
                        },
                        "recovered_storage_delete_completion",
                    )
                    .await;
                } else if let Some((info_hash, name, old_save_path, save_path)) = move_context {
                    let quiesced = affected_torrents
                        .iter()
                        .find(|(hash, _)| hash == &info_hash)
                        .map(|(_, paused)| *paused);
                    let torrent_handle = affected_torrent_handles
                        .iter()
                        .find(|(hash, _)| hash == &info_hash)
                        .map(|(_, handle)| *handle);
                    send_engine_command_until_actor_stops(
                        cmd_tx.clone(),
                        EngineCmd::StorageMoveFinished {
                            job_id,
                            info_hash,
                            name,
                            old_save_path,
                            save_path,
                            quiesced,
                            torrent_handle,
                            succeeded: completion.succeeded,
                            terminal_state: completion.state,
                            error: completion.error,
                            completed_steps: completion.completed_steps,
                            completed_byte_offset: completion.completed_byte_offset,
                            requires_manual_recovery: completion.requires_manual_recovery,
                            retry_attempt: 0,
                        },
                        "recovered_storage_move_completion",
                    )
                    .await;
                } else {
                    send_engine_command_until_actor_stops(
                        cmd_tx,
                        EngineCmd::StoragePlanFinished {
                            job_id,
                            affected_torrents,
                            affected_torrent_handles,
                            manual_recovery_torrents: job.affected_torrents.clone(),
                            succeeded: completion.succeeded,
                            terminal_state: completion.state,
                            error: completion.error,
                            completed_steps: completion.completed_steps,
                            completed_byte_offset: completion.completed_byte_offset,
                            requires_manual_recovery: completion.requires_manual_recovery,
                            resume_torrents: true,
                            retry_attempt: 0,
                        },
                        "recovered_storage_plan_completion",
                    )
                    .await;
                }
            });
        }
        Ok(())
    }

    /// A recheck job is persisted before its torrent command is dispatched.
    /// If the process stops in that window, the job remains queued while the
    /// torrent row and task are restored normally. Redispatch those queued
    /// jobs after the torrent projection has been loaded; explicitly paused
    /// jobs are left alone because their queued state is an intentional
    /// control decision, not an interrupted dispatch.
    pub(super) async fn resume_recovered_recheck_jobs(&mut self) -> anyhow::Result<()> {
        let jobs = self
            .run_db("list_recoverable_recheck_jobs", |db| {
                rt_db::list_active_jobs(db).map_err(|error| error.to_string())
            })
            .await
            .map_err(anyhow::Error::msg)?;
        for job in jobs
            .into_iter()
            .filter(|job| job.kind == JOB_KIND_RECHECK && job.state == JOB_STATE_QUEUED)
        {
            if let Err(error) = self
                .control_recheck_job(&job.job_id, JOB_STATE_RUNNING)
                .await
            {
                warn!(
                    component = "engine",
                    operation = "recover_recheck_job",
                    job_id = %job.job_id,
                    result = "deferred",
                    error = %error,
                    "queued recheck could not be redispatched during startup"
                );
            }
        }
        Ok(())
    }

    pub(super) async fn persist_entry_with_event(
        &self,
        entry: &TorrentEntry,
        meta: &TorrentMeta,
        event: Option<&rt_db::SessionEventRow>,
    ) -> anyhow::Result<()> {
        let row = row_from_entry(entry, meta);
        let files = meta_file_rows(&entry.info_hash, meta);
        let tracker_rows = tracker_rows_from_urls(
            &entry.info_hash,
            &row.trackers,
            row.uploaded,
            row.downloaded,
            row.amount_left,
        );
        let info_hash = entry.info_hash.clone();
        let event = event.cloned();
        let retention = self.config.logging.event_retention;
        self.run_db("persist_torrent_projection", move |db| {
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
            rt_db::replace_torrent_files_in_tx(&tx, &info_hash, &files)
                .map_err(|error| error.to_string())?;
            rt_db::replace_torrent_trackers_in_tx(&tx, &info_hash, &tracker_rows)
                .map_err(|error| error.to_string())?;
            rt_db::delete_torrent_metadata_v2_hash_in_tx(&tx, &info_hash)
                .map_err(|error| error.to_string())?;
            if let Some(event) = event.as_ref() {
                rt_db::append_session_event_in_tx(&tx, event).map_err(|error| error.to_string())?;
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
            }
            tx.commit().map_err(|error| error.to_string())
        })
        .await
        .map_err(anyhow::Error::msg)
    }

    #[cfg(test)]
    pub(super) fn save_torrent_blob(&self, info_hash: &str, raw: &[u8]) -> anyhow::Result<()> {
        save_torrent_blob_from_config(&self.config, info_hash, raw)
    }

    #[cfg(test)]
    pub(super) fn load_torrent_blob(&self, info_hash: &str) -> anyhow::Result<Vec<u8>> {
        load_torrent_blob_from_config(&self.config, info_hash)
    }

    pub(super) fn publish_torrent_blob(
        &self,
        info_hash: &str,
        staged_blob: &Path,
    ) -> CmdResult<()> {
        let destination = torrent_blob_path(&self.config, info_hash);
        if let Err(error) = rt_storage::rename_no_follow(staged_blob, &destination) {
            self.remove_magnet_blob_path_best_effort(staged_blob, "torrent_add_publish_failed");
            return Err(format!(
                "publishing torrent metadata blob {} failed: {error}",
                staged_blob.display()
            ));
        }
        if let Some(parent) = destination.parent() {
            if let Err(error) = rt_storage::sync_dir_no_follow(parent) {
                return Err(format!(
                    "publishing torrent metadata blob {} was not durably committed: {error}",
                    destination.display()
                ));
            }
        }
        Ok(())
    }

    pub(super) fn remove_magnet_blob_best_effort(&self, info_hash: &str, operation: &str) {
        self.remove_magnet_blob_path_best_effort(
            &torrent_blob_path(&self.config, info_hash),
            operation,
        );
    }

    pub(super) fn remove_magnet_blob_candidate_best_effort(
        &self,
        info_hash: &str,
        staged_blob: Option<&Path>,
        operation: &str,
    ) {
        if let Some(staged_blob) = staged_blob {
            self.remove_magnet_blob_path_best_effort(staged_blob, operation);
        } else {
            self.remove_magnet_blob_best_effort(info_hash, operation);
        }
    }

    pub(super) fn remove_magnet_blob_path_best_effort(&self, path: &Path, operation: &str) {
        remove_staged_blob_path_best_effort(path, operation);
    }

    pub(super) async fn delete_persisted_torrent(
        &self,
        info_hash: &str,
        event: Option<&rt_db::SessionEventRow>,
    ) -> anyhow::Result<()> {
        // Remove filesystem projections first. If a mount or fastresume file
        // is unavailable, retain the DB row so the operator can retry rather
        // than leaving an invisible metadata/payload split. DB deletion is
        // last; a DB failure is likewise recoverable by retrying cleanup.
        let config = Arc::clone(&self.config);
        let info_hash_owned = info_hash.to_owned();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            match rt_storage::remove_file_no_follow(&torrent_blob_path(&config, &info_hash_owned)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            FastresumeStore::new(fastresume_dir(&config)).delete(&info_hash_owned)?;
            Ok(())
        })
        .await
        .map_err(|error| {
            anyhow::Error::msg(crate::task_join_error_summary(
                "torrent projection cleanup worker",
                &error,
            ))
        })??;
        let info_hash = info_hash.to_owned();
        let event = event.cloned();
        let retention = self.config.logging.event_retention;
        self.run_db("delete_torrent_projection", move |db| {
            let tx = db.transaction().map_err(|error| error.to_string())?;
            let _ = rt_db::delete_in_tx(&tx, &info_hash).map_err(|error| error.to_string())?;
            if let Some(event) = event.as_ref() {
                rt_db::append_session_event_in_tx(&tx, event).map_err(|error| error.to_string())?;
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
            }
            tx.commit().map_err(|error| error.to_string())
        })
        .await
        .map_err(anyhow::Error::msg)
    }

    #[cfg(test)]
    pub(super) fn load_torrent_metadata(
        &self,
        info_hash: &str,
    ) -> anyhow::Result<EngineTorrentMetadata> {
        let db = DbExecutor::direct(Arc::clone(&self.db));
        load_torrent_metadata_from_sources(&self.config, &db, &self.services.resources, info_hash)
    }

    pub(super) fn is_pure_v2_torrent(&self, info_hash: &str) -> bool {
        // v2-only rows use the 32-byte SHA-256 info hash representation. A
        // hybrid torrent still has its v1 SHA-1 identity and therefore stays
        // on the regular torrent-task path. The worker performs the
        // authoritative metadata-kind check before touching payload files.
        info_hash.len() == 64
    }

    pub(super) async fn start_pure_v2_recheck(
        &self,
        info_hash: &str,
        job_id: Option<String>,
    ) -> CmdResult<()> {
        self.ensure_torrent_storage_idle(info_hash).await?;
        let Some(recheck_guard) = try_acquire_pure_v2_recheck_task() else {
            return Err("pure v2 recheck capacity exhausted; retry later".to_owned());
        };
        let (save_root, previous_state, restore_state) = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            let previous_state = entry.state;
            let restore_state = matches!(entry.state, TorrentState::Paused | TorrentState::Stopped)
                .then_some(entry.state);
            (
                PathBuf::from(&entry.save_path),
                previous_state,
                restore_state,
            )
        };
        let authority = self.configured_storage_authority_async().await?;
        authority
            .authorize_path(&save_root)
            .map_err(|error| error.to_string())?;
        let event = self.session_event_row(
            Some(info_hash),
            EVENT_RECHECK_REQUESTED,
            Some("torrent recheck requested"),
            serde_json::json!({ "job_id": job_id }),
        );
        self.set_registry_state_with_event(info_hash, TorrentState::Checking, None, Some(event))
            .await?;
        let run_token = PURE_V2_RECHECK_RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        if let Some(job_id) = &job_id {
            if let Err(error) = self
                .update_job_state_async_with_run_token(
                    job_id,
                    JOB_STATE_RUNNING,
                    None,
                    Some("pure v2 recheck dispatched to storage worker"),
                    Some(run_token),
                )
                .await
            {
                let rollback_event = self.session_event_row(
                    Some(info_hash),
                    "check_dispatch_failed",
                    Some("pure v2 recheck dispatch failed"),
                    serde_json::json!({ "job_id": job_id, "error": error.clone() }),
                );
                if let Err(rollback_error) = self
                    .set_registry_state_with_event(
                        info_hash,
                        previous_state,
                        None,
                        Some(rollback_event),
                    )
                    .await
                {
                    warn!(
                        component = "storage",
                        operation = "rollback_pure_v2_recheck_dispatch",
                        torrent = %info_hash,
                        result = "error",
                        error = %rollback_error,
                        "failed to restore pure-v2 torrent state after recheck admission failure"
                    );
                }
                self.update_job_state_best_effort(
                    job_id,
                    JOB_STATE_FAILED,
                    Some(error.clone()),
                    Some("pure v2 recheck dispatch failed"),
                )
                .await;
                return Err(format!("failed to mark pure v2 recheck running: {error}"));
            }
        }

        let config = Arc::clone(&self.config);
        let resources = self.services.resources.clone();
        let cmd_tx = self.cmd_tx.clone();
        let info_hash = info_hash.to_owned();
        tokio::spawn(async move {
            let _recheck_guard = recheck_guard;
            let result =
                execute_pure_v2_recheck(config, resources, authority, save_root, info_hash.clone())
                    .await;
            let command = match result {
                Ok(result) => EngineCmd::PureV2RecheckFinished {
                    info_hash,
                    job_id,
                    run_token: Some(run_token),
                    restore_state,
                    total_length: result.total_length,
                    total_files: result.total_files,
                    done: result.done,
                    invalid_files: result.invalid_files,
                    amount_left: result.amount_left,
                    error: None,
                },
                Err(error) => EngineCmd::PureV2RecheckFinished {
                    info_hash,
                    job_id,
                    run_token: Some(run_token),
                    restore_state,
                    total_length: 0,
                    total_files: 0,
                    done: 0,
                    invalid_files: Vec::new(),
                    amount_left: 0,
                    error: Some(error),
                },
            };
            // A lost completion leaves the taskless torrent in Checking and
            // its durable recheck job active. Pure-v2 verifier admission is
            // capped, so retain the finalization message until the actor
            // accepts it or its channel closes.
            send_engine_command_until_actor_stops(cmd_tx, command, "complete_pure_v2_recheck")
                .await;
        });
        Ok(())
    }

    pub(super) async fn finish_pure_v2_recheck(
        &self,
        completion: PureV2RecheckCompletion,
    ) -> CmdResult<()> {
        let PureV2RecheckCompletion {
            info_hash,
            job_id,
            run_token,
            restore_state,
            total_length,
            total_files,
            done,
            invalid_files,
            amount_left,
            error,
        } = completion;
        if let Some(job_id) = job_id.as_deref() {
            let current_job = self.active_torrent_job(&info_hash).await?;
            if !matches!(
                current_job,
                Some((current_job_id, kind))
                    if current_job_id == job_id && kind == JOB_KIND_RECHECK
            ) {
                warn!(
                    component = "storage",
                    operation = "finish_pure_v2_recheck",
                    torrent = %info_hash,
                    job_id = %job_id,
                    result = "stale",
                    "discarding pure-v2 recheck completion for a non-current job"
                );
                return Ok(());
            }
            if let Some(run_token) = run_token {
                let current_run_token = self.current_pure_v2_recheck_run_token(job_id).await?;
                if current_run_token != Some(run_token) {
                    warn!(
                        component = "storage",
                        operation = "finish_pure_v2_recheck",
                        torrent = %info_hash,
                        job_id = %job_id,
                        result = "stale",
                        completion_run_token = run_token,
                        current_run_token = ?current_run_token,
                        "discarding pure-v2 recheck completion for a prior execution attempt"
                    );
                    return Ok(());
                }
            }
        }
        if self.runtime.pending_torrent_deletes.contains(&info_hash) {
            if let Some(job_id) = &job_id {
                if let Err(error) = self
                    .update_job_state_async(
                        job_id,
                        JOB_STATE_CANCELLED,
                        Some("torrent removal superseded the recheck".to_owned()),
                        Some("pure v2 recheck discarded during torrent removal"),
                    )
                    .await
                {
                    self.fail_pure_v2_recheck_finalization(&info_hash, Some(job_id), &error)
                        .await;
                    return Err(error);
                }
            }
            return Ok(());
        }
        if let Some(error) = error {
            if let Some(job_id) = &job_id {
                let job_id_for_db = job_id.clone();
                let state = self
                    .run_db("load_pure_v2_recheck_error_state", move |db| {
                        rt_db::get_job(db, &job_id_for_db)
                            .map(|job| job.state)
                            .map_err(|error| error.to_string())
                    })
                    .await;
                let state = match state {
                    Ok(state) => state,
                    Err(load_error) => {
                        self.fail_pure_v2_recheck_finalization(
                            &info_hash,
                            Some(job_id),
                            &load_error,
                        )
                        .await;
                        return Err(load_error);
                    }
                };
                if matches!(
                    state.as_str(),
                    JOB_STATE_PAUSED | JOB_STATE_CANCELLED | JOB_STATE_FAILED | JOB_STATE_COMPLETED
                ) {
                    let event = self.session_event_row(
                        Some(&info_hash),
                        "check_discarded",
                        Some("pure v2 file-root recheck error was discarded"),
                        serde_json::json!({ "job_id": job_id, "state": state }),
                    );
                    if let Err(error) = self
                        .set_registry_state_with_event(
                            &info_hash,
                            TorrentState::Paused,
                            None,
                            Some(event),
                        )
                        .await
                    {
                        self.fail_pure_v2_recheck_finalization(&info_hash, Some(job_id), &error)
                            .await;
                        return Err(error);
                    }
                    return Ok(());
                }
                if let Err(update_error) = self
                    .update_job_state_async(
                        job_id,
                        JOB_STATE_FAILED,
                        Some(error.clone()),
                        Some("pure v2 recheck failed"),
                    )
                    .await
                {
                    self.fail_pure_v2_recheck_finalization(&info_hash, Some(job_id), &update_error)
                        .await;
                    return Err(update_error);
                }
            }
            let event = self.session_event_row(
                Some(&info_hash),
                "check_failed",
                Some("pure v2 file-root recheck failed"),
                serde_json::json!({ "error": error }),
            );
            if let Err(state_error) = self
                .set_registry_state_with_event(&info_hash, TorrentState::Error, None, Some(event))
                .await
            {
                self.fail_pure_v2_recheck_finalization(&info_hash, job_id.as_deref(), &state_error)
                    .await;
                return Err(state_error);
            }
            return Ok(());
        }

        // A pause/cancel can arrive while the worker is reading the payload.
        // The worker is deliberately not force-killed mid-read; instead its
        // completion is ignored so it cannot resurrect a user-paused or
        // cancelled job. A later resume starts a fresh verification pass.
        if let Some(job_id) = &job_id {
            let job_id_for_db = job_id.clone();
            let state = self
                .run_db("load_pure_v2_recheck_state", move |db| {
                    rt_db::get_job(db, &job_id_for_db)
                        .map(|job| job.state)
                        .map_err(|error| error.to_string())
                })
                .await;
            let state = match state {
                Ok(state) => state,
                Err(load_error) => {
                    self.fail_pure_v2_recheck_finalization(&info_hash, Some(job_id), &load_error)
                        .await;
                    return Err(load_error);
                }
            };
            if matches!(
                state.as_str(),
                JOB_STATE_PAUSED | JOB_STATE_CANCELLED | JOB_STATE_FAILED
            ) {
                let event = self.session_event_row(
                    Some(&info_hash),
                    "check_discarded",
                    Some("pure v2 file-root recheck was discarded"),
                    serde_json::json!({ "job_id": job_id }),
                );
                if let Err(error) = self
                    .set_registry_state_with_event(
                        &info_hash,
                        TorrentState::Paused,
                        None,
                        Some(event),
                    )
                    .await
                {
                    self.fail_pure_v2_recheck_finalization(&info_hash, Some(job_id), &error)
                        .await;
                    return Err(error);
                }
                return Ok(());
            }
            if let Err(error) = self
                .persist_pure_v2_recheck_job_async(job_id, done, total_files, &invalid_files)
                .await
            {
                self.fail_pure_v2_recheck_finalization(&info_hash, Some(job_id), &error)
                    .await;
                return Err(error);
            }
        }

        let (invalid_files_projection, invalid_files_truncated) =
            bounded_session_event_indices(&invalid_files);
        let event = self.session_event_row(
            Some(&info_hash),
            "check_completed",
            Some("pure v2 file-root recheck completed"),
            serde_json::json!({
                "total_length": total_length,
                "invalid_file_count": invalid_files.len(),
                "invalid_files_truncated": invalid_files_truncated,
                "invalid_files": invalid_files_projection,
            }),
        );
        let state_result = if let Some(restore_state) = restore_state {
            self.set_registry_state_with_verified_progress(
                &info_hash,
                restore_state,
                total_length,
                amount_left,
                Some(event),
            )
            .await
        } else if invalid_files.is_empty() {
            self.set_registry_state_with_event(
                &info_hash,
                TorrentState::Seeding,
                Some(total_length),
                Some(event),
            )
            .await
        } else {
            self.set_registry_state_with_verified_progress(
                &info_hash,
                TorrentState::Paused,
                total_length,
                amount_left,
                Some(event),
            )
            .await
        };
        if let Err(error) = state_result {
            self.fail_pure_v2_recheck_finalization(&info_hash, job_id.as_deref(), &error)
                .await;
            return Err(error);
        }
        Ok(())
    }

    /// Return the execution token from the most recent pure-v2 recheck start
    /// event. The durable job ID remains stable across pause/resume, so the
    /// event token is the generation fence for detached filesystem workers.
    pub(super) async fn current_pure_v2_recheck_run_token(
        &self,
        job_id: &str,
    ) -> Result<Option<u64>, String> {
        let job_id_for_db = job_id.to_owned();
        let events = self
            .run_db("load_pure_v2_recheck_start_events", move |db| {
                rt_db::list_job_events(db, &job_id_for_db, 64).map_err(|error| error.to_string())
            })
            .await?;
        let Some(event) = events
            .into_iter()
            .find(|event| event.kind == "check_started")
        else {
            return Ok(None);
        };
        let payload: serde_json::Value = serde_json::from_str(&event.payload)
            .map_err(|error| format!("pure v2 recheck start event has invalid payload: {error}"))?;
        let run_token = payload
            .get("run_token")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "pure v2 recheck start event has no run token".to_owned())?;
        Ok(Some(run_token))
    }

    /// A pure-v2 worker has no live torrent actor to report a terminal error
    /// after its completion command is consumed. If finalization fails after
    /// the current-job fence, fail closed instead of leaving a taskless row in
    /// Checking with a non-terminal job. The in-memory fallback keeps the API
    /// truthful even when the database worker is the component that failed;
    /// restart recovery can reconcile the durable row once storage returns.
    pub(super) async fn fail_pure_v2_recheck_finalization(
        &self,
        info_hash: &str,
        job_id: Option<&str>,
        reason: &str,
    ) {
        if let Some(job_id) = job_id {
            self.update_job_state_best_effort(
                job_id,
                JOB_STATE_FAILED,
                Some(reason.to_owned()),
                Some("pure v2 recheck finalization failed"),
            )
            .await;
        }
        let event = self.session_event_row(
            Some(info_hash),
            "check_failed",
            Some("pure v2 recheck finalization failed"),
            serde_json::json!({ "error": reason }),
        );
        if let Err(error) = self
            .set_registry_state_with_event(info_hash, TorrentState::Error, None, Some(event))
            .await
        {
            warn!(
                component = "storage",
                operation = "fail_pure_v2_recheck_finalization",
                torrent = %info_hash,
                result = "db_error",
                error = %error,
                "failed to persist pure-v2 recheck failure state; forcing in-memory error"
            );
            let mut registry = self.registry.write().await;
            let was_dormant = registry.is_dormant(info_hash);
            if let Some(mut entry) = registry.get_mut(info_hash) {
                entry.set_error(reason.to_owned());
            }
            if was_dormant {
                let _ = registry.demote(info_hash);
            }
        }
    }

    pub(super) async fn set_registry_state(
        &self,
        info_hash: &str,
        state: TorrentState,
        completed_length: Option<u64>,
    ) -> CmdResult<()> {
        self.set_registry_state_with_event(info_hash, state, completed_length, None)
            .await
    }

    pub(super) async fn set_registry_state_with_event(
        &self,
        info_hash: &str,
        state: TorrentState,
        completed_length: Option<u64>,
        event: Option<rt_db::SessionEventRow>,
    ) -> CmdResult<()> {
        self.set_registry_state_with_event_and_progress(
            info_hash,
            state,
            completed_length,
            None,
            event,
        )
        .await
    }

    /// Persist a state transition together with the payload progress observed
    /// by an off-task verifier. Unlike `completed_length`, this does not mark
    /// the torrent complete or set `completed_at`; it only replaces the
    /// authoritative total and remaining lengths from the verification pass.
    pub(super) async fn set_registry_state_with_verified_progress(
        &self,
        info_hash: &str,
        state: TorrentState,
        total_length: u64,
        amount_left: u64,
        event: Option<rt_db::SessionEventRow>,
    ) -> CmdResult<()> {
        self.set_registry_state_with_event_and_progress(
            info_hash,
            state,
            None,
            Some((total_length, amount_left)),
            event,
        )
        .await
    }

    pub(super) async fn set_registry_state_with_event_and_progress(
        &self,
        info_hash: &str,
        state: TorrentState,
        completed_length: Option<u64>,
        verified_progress: Option<(u64, u64)>,
        event: Option<rt_db::SessionEventRow>,
    ) -> CmdResult<()> {
        self.ensure_torrent_not_deleting(info_hash)?;
        let (previous, was_dormant, completed_at_for_db, total_length_for_db, amount_left_for_db) = {
            let mut reg = self.registry.write().await;
            let was_dormant = reg.is_dormant(info_hash);
            let mut entry = reg
                .get_mut(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            let previous = entry.clone();
            entry.transition(state).map_err(|error| error.to_string())?;
            if let Some(total) = completed_length {
                entry.total_length = total;
                entry.amount_left = 0;
                if entry.completed_at.is_none() {
                    entry.completed_at = Some(unix_now_i64() as u64);
                }
            }
            if let Some((total_length, amount_left)) = verified_progress {
                entry.total_length = total_length;
                entry.amount_left = amount_left.min(total_length);
            }
            (
                previous,
                was_dormant,
                entry.completed_at.map(db_i64),
                db_i64(entry.total_length),
                db_i64(entry.amount_left),
            )
        };
        let retention = self.config.logging.event_retention;
        let info_hash_for_db = info_hash.to_owned();
        let state_for_db = state.as_str().to_owned();
        let persistence = self
            .run_db("persist_torrent_state", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                let updated = rt_db::update_state_in_tx(
                    &tx,
                    &info_hash_for_db,
                    &state_for_db,
                    completed_at_for_db,
                    total_length_for_db,
                    amount_left_for_db,
                )
                .map_err(|error| error.to_string())?;
                if !updated {
                    return Err(format!(
                        "torrent {info_hash_for_db} is missing from the database"
                    ));
                }
                if let Some(event) = event.as_ref() {
                    rt_db::append_session_event_in_tx(&tx, event)
                        .map_err(|error| error.to_string())?;
                    rt_db::prune_session_events_in_tx(&tx, retention)
                        .map_err(|error| error.to_string())?;
                }
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            self.restore_registry_lifecycle(info_hash, previous, was_dormant)
                .await;
            return Err(error);
        }
        Ok(())
    }
}
