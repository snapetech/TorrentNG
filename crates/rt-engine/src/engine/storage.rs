//! Actor-side move, delete, and storage-plan completion choreography.
//! Filesystem execution remains in the supervised storage worker.

use super::*;

impl Engine {
    /// Begin a mutable-field update without making the engine actor perform
    /// per-file filesystem admission. A save-path move needs `exists()` and
    /// device detection for each persisted file; those calls belong on the
    /// blocking storage-planning task. The actor receives a prepared plan
    /// later and remains available for health, lifecycle, and peer commands in
    /// the meantime.
    pub(super) async fn begin_update_torrent_fields(
        &mut self,
        info_hash: String,
        name: Option<String>,
        save_path: Option<PathBuf>,
        reply: oneshot::Sender<CmdResult<Option<String>>>,
    ) {
        if let Some(name) = name.as_deref() {
            if let Err(error) = validate_text_bytes(name, MAX_ENGINE_NAME_BYTES, "torrent name") {
                let _ = reply.send(Err(error));
                return;
            }
        }
        if let Err(error) = validate_save_path_value(save_path.as_deref()) {
            let _ = reply.send(Err(error));
            return;
        }
        if let Err(error) = self.ensure_torrent_jobs_idle(&info_hash).await {
            let _ = reply.send(Err(error));
            return;
        }
        let normalized_name = normalize_optional_text(name);
        let (torrent_handle, current_name, current_save_path) = {
            let reg = self.registry.read().await;
            let Some(entry) = reg.get(&info_hash) else {
                let _ = reply.send(Err(format!("torrent {info_hash} not found")));
                return;
            };
            (
                entry.handle,
                entry.name.clone(),
                PathBuf::from(&entry.save_path),
            )
        };

        let Some(target_save_path) = save_path else {
            let result = self
                .persist_torrent_fields_inner(&info_hash, normalized_name, None)
                .await;
            let _ = reply.send(result);
            return;
        };
        if target_save_path == current_save_path {
            let result = self
                .persist_torrent_fields_inner(&info_hash, normalized_name, Some(target_save_path))
                .await;
            let _ = reply.send(result);
            return;
        }

        // Root configuration is a small, bounded control-plane read. The
        // expensive per-file path probing and same-device decision are moved
        // below the actor boundary.
        let authority = match self.configured_storage_authority_async().await {
            Ok(authority) => authority,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let file_entries = match self.torrent_file_entries_async(&info_hash).await {
            Ok(file_entries) => file_entries,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let Some(storage_plan_task_guard) = try_acquire_engine_storage_plan_task() else {
            let _ = reply.send(Err(
                "storage move planning capacity exhausted; retry later".to_owned()
            ));
            return;
        };
        if !self.try_reserve_storage_move(&info_hash, torrent_handle) {
            drop(storage_plan_task_guard);
            let _ = reply.send(Err(
                "torrent already has a storage move being prepared; retry later".to_owned(),
            ));
            return;
        }
        // Freeze the live task before probing the payload. Planning before
        // this barrier can miss a file that is created between the probe and
        // the eventual storage-worker submission, leaving it in the old root.
        let quiesced = match self.quiesce_torrent_for_storage_move(&info_hash).await {
            Ok(quiesced) => quiesced,
            Err(error) => {
                self.release_storage_move_reservation(&info_hash);
                drop(storage_plan_task_guard);
                let _ = reply.send(Err(format!(
                    "torrent {info_hash} could not be quiesced for storage move: {error}"
                )));
                return;
            }
        };
        self.record_storage_move_quiescence(&info_hash, torrent_handle, quiesced);
        let planning_source = current_save_path.clone();
        let planning_destination = target_save_path.clone();
        let cmd_tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let _storage_plan_task_guard = storage_plan_task_guard;
            let plan_result = match tokio::task::spawn_blocking(move || {
                plan_torrent_payload_files_with_authority(
                    &authority,
                    &planning_source,
                    &planning_destination,
                    &file_entries,
                )
            })
            .await
            {
                Ok(result) => result,
                Err(error) => Err(crate::task_join_error_summary(
                    "storage move planning task",
                    &error,
                )),
            };
            // Planning owns a bounded admission slot and has already
            // quiesced the torrent. Do not drop this stateful handoff after
            // the ordinary completion deadline: that would strand the move
            // reservation and leave the task paused until restart.
            send_engine_command_until_actor_stops(
                cmd_tx,
                EngineCmd::PreparedTorrentFields {
                    info_hash,
                    torrent_handle,
                    quiesced,
                    name: normalized_name,
                    current_name,
                    current_save_path,
                    save_path: target_save_path,
                    plan: plan_result,
                    reply,
                },
                "storage_move_plan_completion",
            )
            .await;
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_prepared_torrent_fields(
        &mut self,
        info_hash: &str,
        torrent_handle: rt_session::TorrentHandle,
        quiesced: Option<bool>,
        normalized_name: Option<String>,
        current_name: &str,
        current_save_path: &Path,
        target_save_path: PathBuf,
        plan: CmdResult<Option<StoragePlan>>,
    ) -> CmdResult<Option<String>> {
        // This completion owns the in-memory planning reservation. Check for
        // competing durable work without rejecting our own reservation.
        if let Err(error) = self.ensure_torrent_durable_job_idle(info_hash).await {
            return self
                .fail_prepared_torrent_move(info_hash, quiesced, torrent_handle, error)
                .await;
        }
        let current = {
            let reg = self.registry.read().await;
            reg.get(info_hash).map(|entry| {
                (
                    entry.handle,
                    entry.name.clone(),
                    PathBuf::from(&entry.save_path),
                )
            })
        };
        let Some(current) = current else {
            return self
                .fail_prepared_torrent_move(
                    info_hash,
                    quiesced,
                    torrent_handle,
                    format!("torrent {info_hash} not found"),
                )
                .await;
        };
        if current.0 != torrent_handle
            || current.1 != current_name
            || current.2 != current_save_path
        {
            return self
                .fail_prepared_torrent_move(
                    info_hash,
                    quiesced,
                    torrent_handle,
                    format!(
                        "torrent {info_hash} changed while its storage move was being planned; retry"
                    ),
                )
                .await;
        }

        let plan = match plan {
            Ok(Some(plan)) => plan,
            Ok(None) => {
                // A torrent may have no payload files on disk yet. The absence of
                // filesystem steps does not make a save-path change metadata-only:
                // a live TorrentTask still caches its storage root. Route the
                // empty transaction through the normal commit/handoff path
                // so the task cannot keep writing to the old root later.
                StoragePlan {
                    dry_run: false,
                    can_apply: true,
                    issues: Vec::new(),
                    steps: Vec::new(),
                    rollback_steps: Vec::new(),
                }
            }
            Err(error) => {
                return self
                    .fail_prepared_torrent_move(info_hash, quiesced, torrent_handle, error)
                    .await;
            }
        };
        self.queue_torrent_move_after_plan(PreparedTorrentMove {
            info_hash: info_hash.to_owned(),
            normalized_name,
            current_save_path: current_save_path.to_path_buf(),
            target_save_path,
            plan,
            quiesced,
            torrent_handle,
        })
        .await
    }

    pub(super) async fn fail_prepared_torrent_move(
        &mut self,
        info_hash: &str,
        quiesced: Option<bool>,
        torrent_handle: TorrentHandle,
        error: String,
    ) -> CmdResult<Option<String>> {
        self.release_storage_move_reservation(info_hash);
        if let Err(resume_error) = self
            .resume_torrent_after_storage_move(info_hash, quiesced, None, Some(torrent_handle))
            .await
        {
            warn!(
                component = "storage_jobs",
                operation = "resume_after_move_preparation_failure",
                torrent = %info_hash,
                result = "error",
                error = %resume_error,
                "failed to restore torrent activity after storage-move preparation failed"
            );
        }
        Err(error)
    }

    pub(super) async fn queue_torrent_move_after_plan(
        &mut self,
        prepared: PreparedTorrentMove,
    ) -> CmdResult<Option<String>> {
        let PreparedTorrentMove {
            info_hash,
            normalized_name,
            current_save_path,
            target_save_path,
            plan,
            quiesced,
            torrent_handle,
        } = prepared;
        if let Err(error) = rt_storage::ensure_plan_can_apply(&plan) {
            return self
                .fail_prepared_torrent_move(
                    &info_hash,
                    quiesced,
                    torrent_handle,
                    format!("storage plan cannot apply: {error}"),
                )
                .await;
        }
        // The planner owns the reservation until submission succeeds or the
        // failure path releases it. Only reject competing durable jobs here.
        if let Err(error) = self.ensure_torrent_durable_job_idle(&info_hash).await {
            return self
                .fail_prepared_torrent_move(&info_hash, quiesced, torrent_handle, error)
                .await;
        }
        let (completion, completion_rx) = oneshot::channel();
        let durable_quiesced = quiesced
            .map(|was_paused| vec![(info_hash.clone(), was_paused)])
            .unwrap_or_default();
        let result = self
            .queue_storage_plan_job_with_context(
                "move",
                vec![info_hash.clone()],
                &plan,
                Vec::new(),
                {
                    let mut context = serde_json::json!({
                        "old_save_path": current_save_path.display().to_string(),
                        "save_path": target_save_path.display().to_string(),
                        "name": normalized_name.clone(),
                    });
                    context["quiesced"] = storage_quiesced_context(&durable_quiesced);
                    context
                },
                completion,
            )
            .await;
        if let Ok(job_id) = &result {
            self.release_storage_move_reservation(&info_hash);
            let cmd_tx = self.cmd_tx.clone();
            let job_id_for_task = job_id.clone();
            tokio::spawn(async move {
                let completion = completion_rx.await.unwrap_or_else(|_| {
                    StorageJobCompletion::failed_with_manual_recovery(
                        "storage worker completion channel closed",
                        Vec::new(),
                    )
                });
                // The filesystem commit handoff is durable state, not a
                // best-effort notification. Storage-job admission bounds the
                // number of these retained completions while the actor is
                // processing a long command.
                send_engine_command_until_actor_stops(
                    cmd_tx,
                    EngineCmd::StorageMoveFinished {
                        job_id: job_id_for_task,
                        info_hash,
                        name: normalized_name,
                        old_save_path: current_save_path,
                        save_path: target_save_path,
                        quiesced,
                        torrent_handle: Some(torrent_handle),
                        succeeded: completion.succeeded,
                        terminal_state: completion.state,
                        error: completion.error,
                        completed_steps: completion.completed_steps,
                        completed_byte_offset: completion.completed_byte_offset,
                        requires_manual_recovery: completion.requires_manual_recovery,
                        retry_attempt: 0,
                    },
                    "storage_move_completion",
                )
                .await;
            });
            return Ok(Some(job_id.clone()));
        }
        self.fail_prepared_torrent_move(
            &info_hash,
            quiesced,
            torrent_handle,
            result
                .err()
                .unwrap_or_else(|| "storage move job submission failed".to_owned()),
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn update_torrent_fields_inner(
        &self,
        info_hash: &str,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<Option<String>> {
        if let Some(name) = name.as_deref() {
            validate_text_bytes(name, MAX_ENGINE_NAME_BYTES, "torrent name")?;
        }
        validate_save_path_value(save_path.as_deref())?;
        self.ensure_torrent_jobs_idle(info_hash).await?;
        let normalized_name = normalize_optional_text(name);
        let current_save_path = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            PathBuf::from(&entry.save_path)
        };
        let target_save_path = save_path;
        if let Some(target) = target_save_path.as_deref() {
            self.authorize_storage_path_async(target).await?;
        }
        if let Some(target) = &target_save_path {
            if *target != current_save_path {
                if let Some(plan) = self.plan_torrent_payload_files(
                    &current_save_path,
                    target,
                    &self.torrent_file_entries(info_hash)?,
                )? {
                    // The actor only performs bounded orchestration here.
                    // Filesystem work and checkpoints run behind the storage
                    // worker boundary; completion comes back as a command so
                    // health/lifecycle requests remain serviceable.
                    let quiesced = self
                        .quiesce_torrent_for_storage_move(info_hash)
                        .await
                        .map_err(|error| {
                            format!(
                                "torrent {info_hash} could not be quiesced for storage move: {error}"
                            )
                        })?;
                    let torrent_handle = self.torrent_handle_for(info_hash).await;
                    let (completion, completion_rx) = oneshot::channel();
                    let durable_quiesced = quiesced
                        .map(|was_paused| vec![(info_hash.to_owned(), was_paused)])
                        .unwrap_or_default();
                    let result = self
                        .queue_storage_plan_job_with_context(
                            "move",
                            vec![info_hash.to_owned()],
                            &plan,
                            Vec::new(),
                            {
                                let mut context = serde_json::json!({
                                    "old_save_path": current_save_path.display().to_string(),
                                    "save_path": target.display().to_string(),
                                    "name": normalized_name.clone(),
                                });
                                context["quiesced"] = storage_quiesced_context(&durable_quiesced);
                                context
                            },
                            completion,
                        )
                        .await;
                    if let Ok(job_id) = &result {
                        let cmd_tx = self.cmd_tx.clone();
                        let job_id_for_task = job_id.clone();
                        let info_hash = info_hash.to_owned();
                        let name = normalized_name.clone();
                        let old_save_path = current_save_path.clone();
                        let save_path = target.clone();
                        tokio::spawn(async move {
                            let completion = completion_rx.await.unwrap_or_else(|_| {
                                StorageJobCompletion::failed_with_manual_recovery(
                                    "storage worker completion channel closed",
                                    Vec::new(),
                                )
                            });
                            send_engine_command_until_actor_stops(
                                cmd_tx,
                                EngineCmd::StorageMoveFinished {
                                    job_id: job_id_for_task,
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
                                "storage_move_completion",
                            )
                            .await;
                        });
                        return Ok(Some(job_id.clone()));
                    }
                    if let Err(resume_error) = self
                        .resume_torrent_after_storage_move(
                            info_hash,
                            quiesced,
                            None,
                            torrent_handle,
                        )
                        .await
                    {
                        warn!(
                            component = "storage_jobs",
                            operation = "resume_after_move_plan_failure",
                            torrent = %info_hash,
                            result = "error",
                            error = %resume_error,
                            "failed to restore torrent activity after storage-move planning failed"
                        );
                    }
                    return result.map(|_| None);
                }
            }
        }

        let mut reg = self.registry.write().await;
        let was_dormant = reg.is_dormant(info_hash);
        let mut entry = reg
            .get_mut(info_hash)
            .ok_or_else(|| format!("torrent {info_hash} not found"))?;
        let previous = entry.clone();

        if let Some(name) = normalized_name {
            entry.name = name;
        }
        if let Some(save_path) = target_save_path {
            entry.save_path = save_path.to_string_lossy().to_string();
        }

        let row = {
            let db = self.db.lock().expect("database mutex poisoned");
            let mut row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
            row.name = entry.name.clone();
            row.save_path = entry.save_path.clone();
            row
        };
        drop(entry);
        drop(reg);

        let fields_event = self.session_event_row(
            Some(info_hash),
            EVENT_FIELDS_UPDATED,
            Some("torrent fields updated"),
            serde_json::json!({
                "name": row.name,
                "save_path": row.save_path,
            }),
        );
        let persistence = (|| -> Result<(), String> {
            let mut db = self.db.lock().expect("database mutex poisoned");
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
            rt_db::append_session_event_in_tx(&tx, &fields_event)
                .map_err(|error| error.to_string())?;
            rt_db::prune_session_events_in_tx(&tx, self.config.logging.event_retention)
                .map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())
        })();
        if let Err(error) = persistence {
            self.restore_registry_entry(info_hash, previous, was_dormant)
                .await;
            return Err(error);
        }
        Ok(None)
    }

    pub(super) async fn persist_torrent_fields_inner(
        &self,
        info_hash: &str,
        normalized_name: Option<String>,
        target_save_path: Option<PathBuf>,
    ) -> CmdResult<Option<String>> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
        let (previous, was_dormant, name_for_db, save_path_for_db) = {
            let mut reg = self.registry.write().await;
            let was_dormant = reg.is_dormant(info_hash);
            let mut entry = reg
                .get_mut(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            let previous = entry.clone();

            if let Some(name) = normalized_name {
                entry.name = name;
            }
            if let Some(save_path) = target_save_path {
                entry.save_path = save_path.to_string_lossy().to_string();
            }

            (
                previous,
                was_dormant,
                entry.name.clone(),
                entry.save_path.clone(),
            )
        };

        let info_hash_for_db = info_hash.to_owned();

        let fields_event = self.session_event_row(
            Some(info_hash),
            EVENT_FIELDS_UPDATED,
            Some("torrent fields updated"),
            serde_json::json!({
                "name": name_for_db.clone(),
                "save_path": save_path_for_db.clone(),
            }),
        );
        let retention = self.config.logging.event_retention;
        let persistence = self
            .run_db("persist_torrent_fields", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                let updated = rt_db::update_fields_in_tx(
                    &tx,
                    &info_hash_for_db,
                    &name_for_db,
                    &save_path_for_db,
                )
                .map_err(|error| error.to_string())?;
                if !updated {
                    return Err(format!(
                        "torrent {info_hash_for_db} is missing from the database"
                    ));
                }
                rt_db::append_session_event_in_tx(&tx, &fields_event)
                    .map_err(|error| error.to_string())?;
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            self.restore_registry_fields(info_hash, previous, was_dormant)
                .await;
            return Err(error);
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(super) fn plan_torrent_payload_files(
        &self,
        source_root: &std::path::Path,
        destination_root: &std::path::Path,
        file_entries: &[(rt_path::SafeRelPath, u64)],
    ) -> CmdResult<Option<StoragePlan>> {
        self.authorize_storage_path(source_root)?;
        self.authorize_storage_path(destination_root)?;
        let mut steps = Vec::new();
        let mut rollback_steps = Vec::new();
        let mut issues = Vec::new();
        for (rel_path, bytes) in file_entries.iter() {
            let source = rel_path.resolve(source_root);
            match std::fs::symlink_metadata(&source) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "failed to inspect payload source {}: {error}",
                        source.display()
                    ));
                }
            }
            let destination = rel_path.resolve(destination_root);
            self.authorize_storage_path(&source)?;
            self.authorize_storage_path(&destination)?;
            let plan = rt_storage::plan_move(&rt_storage::MovePlanRequest {
                source,
                destination,
                bytes: *bytes,
                available_bytes: None,
                dry_run: false,
            });
            issues.extend(plan.issues);
            steps.extend(plan.steps);
            rollback_steps.extend(plan.rollback_steps);
        }
        if steps.is_empty() {
            return Ok(None);
        }
        let plan = StoragePlan {
            dry_run: false,
            can_apply: issues.is_empty(),
            issues,
            steps,
            rollback_steps,
        };

        Ok(Some(plan))
    }

    #[cfg(test)]
    pub(super) fn torrent_file_entries(
        &self,
        info_hash: &str,
    ) -> CmdResult<Vec<(rt_path::SafeRelPath, u64)>> {
        let file_rows = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::list_torrent_files(&db, info_hash).map_err(|error| error.to_string())?
        };
        file_entries_from_rows(&file_rows)
    }

    pub(super) async fn torrent_file_entries_async(
        &self,
        info_hash: &str,
    ) -> CmdResult<Vec<(rt_path::SafeRelPath, u64)>> {
        let info_hash_for_db = info_hash.to_owned();
        let file_rows = self
            .run_db("list_torrent_files_for_storage_plan", move |db| {
                rt_db::list_torrent_files(db, &info_hash_for_db).map_err(|error| error.to_string())
            })
            .await?;
        file_entries_from_rows(&file_rows)
    }

    pub(super) async fn plan_torrent_payload_delete(
        &self,
        save_root: &Path,
        file_entries: &[(rt_path::SafeRelPath, u64)],
    ) -> CmdResult<Option<StoragePlan>> {
        self.authorize_storage_path_async(save_root).await?;
        if file_entries.is_empty() {
            return Ok(None);
        }

        let mut steps = Vec::with_capacity(file_entries.len());
        let mut prune_dirs = HashSet::new();
        for (rel_path, bytes) in file_entries.iter() {
            let path = rel_path.resolve(save_root);
            // The worker repeats root confinement immediately before
            // execution. This admission check catches a bad configured path
            // before a job is made visible, without stat-ing every payload
            // file on the actor thread.
            if !path.starts_with(save_root) {
                return Err(format!(
                    "torrent payload path escapes save root: {}",
                    path.display()
                ));
            }
            steps.push(StoragePlanStep {
                action: rt_storage::PlannedStorageAction::SafeDeleteIfPresent,
                source: Some(path.clone()),
                destination: None,
                bytes: *bytes,
            });

            let mut parent = path.parent();
            while let Some(dir) = parent {
                if dir == save_root || !dir.starts_with(save_root) {
                    break;
                }
                prune_dirs.insert(dir.to_path_buf());
                parent = dir.parent();
            }
        }

        // All file removals must precede directory pruning. Each prune step
        // stops at the first non-empty directory, preserving files belonging
        // to another torrent or operator-managed content.
        let mut prune_dirs = prune_dirs.into_iter().collect::<Vec<_>>();
        prune_dirs.sort_by_key(|left| std::cmp::Reverse(left.components().count()));
        steps.extend(prune_dirs.into_iter().map(|dir| StoragePlanStep {
            action: rt_storage::PlannedStorageAction::PruneEmptyDirs,
            source: Some(dir),
            destination: Some(save_root.to_path_buf()),
            bytes: 0,
        }));

        Ok(Some(StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps,
            rollback_steps: Vec::new(),
        }))
    }

    pub(super) fn try_reserve_storage_move(
        &mut self,
        info_hash: &str,
        torrent_handle: TorrentHandle,
    ) -> bool {
        use std::collections::hash_map::Entry;

        match self.pending_storage_moves.entry(info_hash.to_owned()) {
            Entry::Vacant(entry) => {
                entry.insert(PendingStorageMove {
                    torrent_handle,
                    quiesced: None,
                });
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    pub(super) fn record_storage_move_quiescence(
        &mut self,
        info_hash: &str,
        torrent_handle: TorrentHandle,
        quiesced: Option<bool>,
    ) {
        if let Some(pending) = self.pending_storage_moves.get_mut(info_hash) {
            debug_assert_eq!(pending.torrent_handle, torrent_handle);
            pending.quiesced = quiesced;
        }
    }

    pub(super) fn release_storage_move_reservation(&mut self, info_hash: &str) {
        self.pending_storage_moves.remove(info_hash);
    }

    pub(super) fn storage_move_pending_error(&self, info_hash: &str) -> Option<String> {
        if self.pending_storage_moves.contains_key(info_hash) {
            return Some(format!(
                "torrent {info_hash} has a storage move being prepared; retry later"
            ));
        }
        None
    }

    pub(super) async fn resume_pending_storage_moves_on_shutdown(&mut self) {
        let pending = std::mem::take(&mut self.pending_storage_moves);
        let engine = &*self;
        let results = stream::iter(pending)
            .map(|(info_hash, pending)| async move {
                let result = engine
                    .resume_torrent_after_storage_move(
                        &info_hash,
                        pending.quiesced,
                        None,
                        Some(pending.torrent_handle),
                    )
                    .await;
                (info_hash, result)
            })
            .buffer_unordered(8)
            .collect::<Vec<_>>()
            .await;
        for (info_hash, result) in results {
            if let Err(error) = result {
                warn!(
                    component = "storage_jobs",
                    operation = "shutdown_resume_unsubmitted_move",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "failed to restore torrent state for a storage move that was not submitted"
                );
            }
        }
    }

    pub(super) fn ensure_storage_move_not_pending(&self, info_hashes: &[String]) -> CmdResult<()> {
        if let Some(error) = info_hashes
            .iter()
            .find_map(|info_hash| self.storage_move_pending_error(info_hash))
        {
            return Err(error);
        }
        Ok(())
    }

    /// Quiesces the running task for `info_hash` before a storage move, if
    /// one exists. Returns `Some(was_already_paused)` when a task was
    /// quiesced (the caller must resume it afterward via
    /// `resume_torrent_after_storage_move`), or `None` when there is no
    /// running task -- nothing to quiesce, and correspondingly nothing to
    /// resume. A live task that cannot acknowledge quiescence is an error;
    /// storage work must never proceed in that case.
    pub(super) async fn quiesce_torrent_for_storage_move(
        &self,
        info_hash: &str,
    ) -> CmdResult<Option<bool>> {
        let Some(tx) = self.runtime.torrent_chans.get(info_hash).cloned() else {
            return Ok(None);
        };
        let (_, result) = self
            .quiesce_torrent_for_storage_plan(info_hash.to_owned(), tx)
            .await;
        result.map(Some)
    }

    pub(super) async fn torrent_handle_for(&self, info_hash: &str) -> Option<TorrentHandle> {
        self.registry
            .read()
            .await
            .get(info_hash)
            .map(|entry| entry.handle)
    }

    pub(super) async fn torrent_handles_for_targets(
        &self,
        targets: &[String],
    ) -> Vec<(String, TorrentHandle)> {
        let registry = self.registry.read().await;
        targets
            .iter()
            .filter_map(|info_hash| {
                registry
                    .get(info_hash)
                    .map(|entry| (info_hash.clone(), entry.handle))
            })
            .collect()
    }

    pub(super) async fn torrent_handles_for_quiesced(
        &self,
        quiesced: &[(String, bool)],
    ) -> Vec<(String, TorrentHandle)> {
        let targets = quiesced
            .iter()
            .map(|(info_hash, _)| info_hash.clone())
            .collect::<Vec<_>>();
        self.torrent_handles_for_targets(&targets).await
    }

    /// Resumes a task previously quiesced by
    /// `quiesce_torrent_for_storage_move`. `new_save_root` should be
    /// `Some(destination)` when the move committed, `None` when it failed
    /// or was rolled back, so the task keeps using its original path
    /// unchanged. If restart recovery has restored only the dormant registry
    /// projection, queue a normal resume command so the actor promotes it
    /// after the storage job is terminal. A no-op if `quiesced` is `None`
    /// (nothing was quiesced) or the torrent was already paused.
    pub(super) async fn resume_torrent_after_storage_move(
        &self,
        info_hash: &str,
        quiesced: Option<bool>,
        new_save_root: Option<std::path::PathBuf>,
        expected_handle: Option<TorrentHandle>,
    ) -> CmdResult<()> {
        let Some(was_paused) = quiesced else {
            return Ok(());
        };
        if let Some(expected_handle) = expected_handle {
            let current_handle = self.torrent_handle_for(info_hash).await;
            if current_handle != Some(expected_handle) {
                warn!(
                    component = "storage_jobs",
                    operation = "resume_after_storage_move",
                    torrent = %info_hash,
                    result = "stale",
                    "discarding storage completion for a replaced torrent task"
                );
                return Ok(());
            }
        }
        if let Some(tx) = self.runtime.torrent_chans.get(info_hash).cloned() {
            if !was_paused
                && self
                    .metadata_placeholder_row_checked(info_hash)
                    .await?
                    .is_some()
            {
                // Metadata tasks cannot persist lifecycle state themselves.
                // Restore the durable pending projection before allowing the
                // task to resume, so a process exit immediately afterward
                // cannot resurrect it as paused or leave it out of sync with
                // its runtime tracker state.
                self.update_metadata_placeholder_state_with_event(
                    info_hash,
                    TorrentState::MetadataPending,
                    None,
                )
                .await?;
            }
            let (reply, response) = oneshot::channel();
            send_torrent_command_until_delivered(
                &tx,
                TorrentCmd::ResumeAfterStorageMove {
                    new_save_root,
                    resume_paused: was_paused,
                    reply,
                },
            )
            .await?;
            return timeout(ENGINE_COMMAND_REPLY_TIMEOUT, response)
                .await
                .map_err(|_| "torrent storage-move resume timed out".to_owned())?
                .map_err(|_| "torrent task dropped storage-move resume reply".to_owned())?;
        } else if !was_paused {
            // A storage job can outlive the task it quiesced. Restart restores
            // the durable `Paused` row as a dormant projection until this
            // completion path is reached, so do not silently lose the
            // pre-quiesce active state. The queued command is asynchronous;
            // report it as deferred so a storage-move commit remains
            // `commit_pending` until a later retry observes the promoted
            // task and completes the durable job.
            let (reply, _receiver) = oneshot::channel();
            let cmd_tx = self.cmd_tx.clone();
            let info_hash = info_hash.to_owned();
            let command = EngineCmd::ResumeTorrent { info_hash, reply };
            match cmd_tx.try_send(command) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(command)) => {
                    tokio::spawn(async move {
                        send_engine_command_until_delivered(cmd_tx, command, "storage_move_resume")
                            .await;
                    });
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    warn!(
                        component = "engine",
                        operation = "storage_move_resume",
                        result = "actor_gone",
                        "could not queue resume after storage move because the engine stopped"
                    );
                    return Err("engine command channel closed".to_owned());
                }
            }
            return Err("torrent resume deferred until dormant promotion completes".to_owned());
        }
        Ok(())
    }

    /// Quiesces every torrent in `info_hashes` that currently has a
    /// running task, for the duration of a generic (non-save-path-owning)
    /// storage plan execution. Returns the `(info_hash, was_already_paused)`
    /// pairs that actually got quiesced, for `resume_torrents_after_storage_plan`.
    /// If any live task fails to acknowledge, already-quiesced tasks are
    /// resumed and the plan is rejected before the worker can touch files.
    pub(super) async fn quiesce_torrents_for_storage_plan(
        &self,
        info_hashes: &[String],
    ) -> CmdResult<Vec<(String, bool)>> {
        let targets = info_hashes
            .iter()
            .filter_map(|info_hash| {
                self.runtime
                    .torrent_chans
                    .get(info_hash)
                    .cloned()
                    .map(|tx| (info_hash.clone(), tx))
            })
            .collect::<Vec<_>>();
        let results: Vec<(String, CmdResult<bool>)> = stream::iter(
            targets
                .into_iter()
                .map(|(info_hash, tx)| self.quiesce_torrent_for_storage_plan(info_hash, tx)),
        )
        .buffer_unordered(TIER_IDLE_RECONCILE_CONCURRENCY)
        .collect()
        .await;
        let (quiesced, first_error) = collect_quiesce_results(results);
        if let Some(error) = first_error {
            self.resume_torrents_after_storage_plan(quiesced, Vec::new())
                .await;
            return Err(error);
        }
        Ok(quiesced)
    }

    pub(super) async fn quiesce_torrent_for_storage_plan(
        &self,
        info_hash: String,
        tx: mpsc::Sender<TorrentCmd>,
    ) -> (String, CmdResult<bool>) {
        // Metadata tasks do not own a database executor. Persist their paused
        // projection at the engine boundary just like TorrentTask does
        // internally, otherwise a restart during a generic storage plan can
        // relaunch tracker/metadata work from a durable MetadataPending row.
        let is_metadata_placeholder = match self.metadata_placeholder_row_checked(&info_hash).await
        {
            Ok(row) => row.is_some(),
            Err(error) => return (info_hash, Err(error)),
        };
        let result = quiesce_torrent_channel(&tx).await;
        if is_metadata_placeholder {
            if let Ok(was_paused) = result {
                if let Err(error) = self
                    .update_metadata_placeholder_state_with_event(
                        &info_hash,
                        TorrentState::Paused,
                        None,
                    )
                    .await
                {
                    // The task is already quiesced, but the storage plan has
                    // not started. Restore its prior runtime activity before
                    // returning the persistence failure to the caller.
                    if let Err(resume_error) = send_torrent_lifecycle_command(&tx, was_paused).await
                    {
                        return (
                            info_hash,
                            Err(format!("failed to persist metadata quiesce state: {error}; failed to restore metadata task: {resume_error}")),
                        );
                    }
                    return (
                        info_hash,
                        Err(format!("failed to persist metadata quiesce state: {error}")),
                    );
                }
                return (info_hash, Ok(was_paused));
            }
        }
        (info_hash, result)
    }

    /// Resumes every torrent previously quiesced by
    /// `quiesce_torrents_for_storage_plan`. This generic executor never
    /// changes a torrent's canonical save_path, so every resume carries
    /// `new_save_root: None`.
    pub(super) async fn resume_torrents_after_storage_plan(
        &self,
        quiesced: Vec<(String, bool)>,
        expected_handles: Vec<(String, TorrentHandle)>,
    ) -> bool {
        let expected_handles = expected_handles.into_iter().collect::<HashMap<_, _>>();
        let mut all_resumed = true;
        for (info_hash, was_paused) in quiesced {
            if let Err(error) = self
                .resume_torrent_after_storage_move(
                    &info_hash,
                    Some(was_paused),
                    None,
                    expected_handles.get(&info_hash).copied(),
                )
                .await
            {
                all_resumed = false;
                warn!(
                    component = "storage_jobs",
                    operation = "resume_after_storage_plan",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "failed to restore torrent activity after storage plan"
                );
            }
        }
        all_resumed
    }

    pub(super) async fn complete_storage_plan_job_async(
        &self,
        job_id: &str,
        completed_steps: &[usize],
        completed_byte_offset: Option<i64>,
    ) -> Result<(), String> {
        let job_id_for_db = job_id.to_owned();
        let completed_steps = completed_steps.to_vec();
        self.run_db("complete_storage_plan_job", move |db| {
            // The worker has already proved that the filesystem transaction is
            // complete. Take the write lock before loading the row so a
            // concurrent control update is ordered against this final commit
            // rather than being overwritten by a stale read.
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| error.to_string())?;
            let mut job = rt_db::get_job(&tx, &job_id_for_db).map_err(|error| error.to_string())?;
            if job.kind != JOB_KIND_STORAGE_PLAN {
                return Err(format!("job {job_id_for_db} is not a storage plan"));
            }
            if job.state == JOB_STATE_COMPLETED && job.finished_at.is_some() {
                return Ok(());
            }
            // A completed filesystem commit wins over a control update that
            // raced after the last storage step. The caller only reaches this
            // method with commit-pending evidence, so preserving cancelled or
            // failed here would strand a live destination behind a terminal
            // job row that restart recovery will never revisit.
            let now = unix_now_i64();
            job.state = JOB_STATE_COMPLETED.to_owned();
            job.error = None;
            job.done = db_i64_usize(completed_steps.len());
            job.checkpoint = job.done;
            job.file_index = Some(job.done);
            if completed_byte_offset.is_some() {
                job.byte_offset = completed_byte_offset;
            }
            job.updated_at = now;
            job.finished_at = Some(now);
            let event = rt_db::JobEventRow {
                event_id: None,
                job_id: job_id_for_db.clone(),
                occurred_at: now,
                kind: "storage_plan_completed".to_owned(),
                message: Some("storage plan actor-side commit completed".to_owned()),
                payload: serde_json::json!({
                    "state": JOB_STATE_COMPLETED,
                    "completed_steps": completed_steps,
                })
                .to_string(),
            };
            rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
            rt_db::append_job_event_in_tx(&tx, &event).map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())
        })
        .await
    }

    /// Complete a payload-delete job. The registry/database projection stays
    /// addressable until the worker reports a successful delete, so failed or
    /// cancelled cleanup can resume the torrent and be retried. The same
    /// finalizer handles a crash/restart where the durable job finishes
    /// against a restored torrent row.
    pub(super) async fn finish_storage_delete(
        &mut self,
        completion: StorageDeleteCompletion,
    ) -> CmdResult<()> {
        let StorageDeleteCompletion {
            job_id,
            info_hash,
            succeeded,
            terminal_state,
            error,
            completed_steps,
            completed_byte_offset,
            requires_manual_recovery,
            quiesced,
            quiesced_handle,
            retry_attempt,
        } = completion;
        // Shutdown requeues the work. Keep the task quiesced and the
        // projection visible; restart recovery will reattach the job and
        // complete or cancel it. User cancellation/failure, in contrast,
        // releases the quiesce so the payload remains usable.
        if terminal_state == JOB_STATE_QUEUED {
            return Ok(());
        }
        if let Some(expected_handle) = quiesced_handle {
            if self.torrent_handle_for(&info_hash).await != Some(expected_handle) {
                // The worker completion belongs to an older torrent
                // incarnation.  The job is terminal, so retaining this
                // in-memory deletion guard would make the replacement
                // permanently reject lifecycle commands until the daemon is
                // restarted.  Do not touch the replacement projection, but
                // release the guard so it remains operable; its payload can
                // be rechecked if the old worker already removed files.
                self.runtime.pending_torrent_deletes.remove(&info_hash);
                warn!(
                    component = "storage_jobs",
                    operation = "finish_storage_delete",
                    job_id = %job_id,
                    torrent = %info_hash,
                    result = "stale",
                    "discarding payload-delete completion for a replaced torrent"
                );
                if succeeded && terminal_state == STORAGE_JOB_STATE_COMMIT_PENDING {
                    if let Err(error) = self
                        .complete_storage_plan_job_async(
                            &job_id,
                            &completed_steps,
                            completed_byte_offset,
                        )
                        .await
                    {
                        self.schedule_storage_delete_commit_retry(
                            &job_id,
                            &info_hash,
                            completed_steps,
                            completed_byte_offset,
                            quiesced.clone(),
                            Some(expected_handle),
                            retry_attempt,
                        );
                        return Err(error);
                    }
                }
                if !succeeded {
                    self.persist_storage_terminal_state_or_schedule(
                        &job_id,
                        &terminal_state,
                        error.clone(),
                        retry_attempt,
                    )
                    .await;
                }
                return Ok(());
            }
        }
        if terminal_state == JOB_STATE_PAUSED {
            // Keep the removal guard and the task quiesced while the worker
            // is paused. Releasing either one would let a later delete step
            // race the torrent actor's payload I/O.
            return Ok(());
        }
        let filesystem_commit_pending = terminal_state == STORAGE_JOB_STATE_COMMIT_PENDING;
        if !succeeded
            || !matches!(
                terminal_state.as_str(),
                JOB_STATE_COMPLETED | STORAGE_JOB_STATE_COMMIT_PENDING
            )
        {
            if requires_manual_recovery {
                warn!(
                    component = "storage_jobs",
                    operation = "delete",
                    job_id = %job_id,
                    torrent = %info_hash,
                    result = "manual_recovery_required",
                    checkpoint = ?completed_steps,
                    error = ?error,
                    "payload delete left filesystem state that cannot be resumed safely"
                );
                let reason = manual_recovery_reason(
                    error.unwrap_or_else(|| "payload delete requires manual recovery".to_owned()),
                );
                // Do not leave a quiesced actor attached to an ambiguous
                // payload. A later resume or inbound-peer command could
                // otherwise write against a path that this delete failed to
                // classify safely.
                self.stop_torrent_task(&info_hash).await;
                if let Err(persist_error) = self
                    .persist_storage_job_manual_recovery(
                        &job_id,
                        std::slice::from_ref(&info_hash),
                        reason.clone(),
                        "payload delete completion required manual recovery",
                    )
                    .await
                {
                    warn!(
                        component = "storage_jobs",
                        operation = "persist_manual_recovery",
                        job_id = %job_id,
                        torrent = %info_hash,
                        result = "error",
                        error = %persist_error,
                        "failed to persist payload-delete manual-recovery marker at completion"
                    );
                    self.persist_storage_terminal_state_or_schedule(
                        &job_id,
                        JOB_STATE_FAILED,
                        Some(format!(
                            "{reason}; failed to persist manual-recovery marker: {persist_error}"
                        )),
                        retry_attempt,
                    )
                    .await;
                }
                if let Err(mark_error) =
                    self.mark_torrent_manual_recovery(&info_hash, &reason).await
                {
                    warn!(
                        component = "storage_jobs",
                        operation = "mark_manual_recovery",
                        job_id = %job_id,
                        torrent = %info_hash,
                        result = "error",
                        error = %mark_error,
                        "failed to persist payload-delete manual-recovery state after stopping its task"
                    );
                    return Ok(());
                }
                self.runtime.pending_torrent_deletes.remove(&info_hash);
                return Ok(());
            }
            self.runtime.pending_torrent_deletes.remove(&info_hash);
            self.persist_storage_terminal_state_or_schedule(
                &job_id,
                &terminal_state,
                error.clone(),
                retry_attempt,
            )
            .await;
            self.resume_torrents_after_storage_plan(
                quiesced,
                quiesced_handle
                    .map(|handle| vec![(info_hash.clone(), handle)])
                    .unwrap_or_default(),
            )
            .await;
            self.append_session_event(
                Some(&info_hash),
                EVENT_TORRENT_REMOVE_FAILED,
                Some("torrent payload cleanup did not complete"),
                serde_json::json!({
                    "job_id": job_id,
                    "payload_cleanup": "failed",
                    "terminal_state": terminal_state,
                    "error": error,
                    "completed_steps": completed_steps,
                }),
            );
            return Ok(());
        }

        self.runtime.pending_torrent_deletes.remove(&info_hash);

        // Stop the quiesced task before deleting its metadata. A task restored
        // in the crash window is handled by this same path.
        self.stop_torrent_task(&info_hash).await;
        self.runtime.tier_controller.remove(&info_hash);
        self.runtime.tier_last_active.remove(&info_hash);

        // Keep the restored registry row visible if bounded metadata cleanup
        // fails. A caller/operator can then retry the durable job instead of
        // getting a silently split registry/DB projection.
        let removal_event = self.session_event_row(
            Some(&info_hash),
            EVENT_TORRENT_REMOVED,
            Some("torrent removed after recovered payload cleanup"),
            serde_json::json!({
                "delete_files": true,
                "payload_delete_job_id": job_id,
                "recovered": true,
            }),
        );
        if let Err(error) = self
            .delete_persisted_torrent(&info_hash, Some(&removal_event))
            .await
        {
            let reason = format!("payload deleted but metadata cleanup failed: {error}");
            let reason = manual_recovery_reason(reason);
            if let Err(persist_error) = self
                .persist_storage_job_manual_recovery(
                    &job_id,
                    std::slice::from_ref(&info_hash),
                    reason.clone(),
                    "payload delete metadata cleanup required manual recovery",
                )
                .await
            {
                warn!(
                    component = "storage_jobs",
                    operation = "persist_manual_recovery",
                    job_id = %job_id,
                    torrent = %info_hash,
                    result = "error",
                    error = %persist_error,
                    "failed to persist payload-delete metadata manual-recovery marker"
                );
                self.persist_storage_terminal_state_or_schedule(
                    &job_id,
                    JOB_STATE_FAILED,
                    Some(format!(
                        "{reason}; failed to persist manual-recovery marker: {persist_error}"
                    )),
                    retry_attempt,
                )
                .await;
            }
            if let Err(mark_error) = self.mark_torrent_manual_recovery(&info_hash, &reason).await {
                warn!(
                    component = "storage_jobs",
                    operation = "mark_manual_recovery",
                    job_id = %job_id,
                    torrent = %info_hash,
                    result = "error",
                    error = %mark_error,
                    "failed to persist payload-delete metadata failure state; torrent remains stopped"
                );
            }
            return Err(error.to_string());
        }

        let _ = self.registry.write().await.remove(&info_hash);
        if filesystem_commit_pending {
            // A worker can report commit_pending when the filesystem is
            // complete but its terminal job-row write failed. Metadata
            // cleanup must finish before the durable storage job is marked
            // completed; otherwise a restart can re-run cleanup against a
            // projection that has already been removed.
            if let Err(error) = self
                .complete_storage_plan_job_async(&job_id, &completed_steps, completed_byte_offset)
                .await
            {
                self.schedule_storage_delete_commit_retry(
                    &job_id,
                    &info_hash,
                    completed_steps,
                    completed_byte_offset,
                    quiesced,
                    quiesced_handle,
                    retry_attempt,
                );
                return Err(error);
            }
        }
        Ok(())
    }

    // Single call site (the storage-plan-job completion handler); each
    // parameter is a distinct piece of state needed to finish committing
    // or rolling back a move, not a natural grouping.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_storage_move(
        &mut self,
        job_id: &str,
        info_hash: &str,
        name: Option<String>,
        old_save_path: PathBuf,
        save_path: PathBuf,
        quiesced: Option<bool>,
        torrent_handle: Option<TorrentHandle>,
        succeeded: bool,
        terminal_state: String,
        error: Option<String>,
        completed_steps: Vec<usize>,
        completed_byte_offset: Option<i64>,
        requires_manual_recovery: bool,
        retry_attempt: u8,
    ) -> CmdResult<()> {
        // Shutdown requeues the worker without committing the filesystem
        // transaction. Keep the task quiesced so restart recovery can resume
        // from the durable checkpoint; reopening it here would race the
        // shutdown path against a still-owned storage plan.
        if terminal_state == JOB_STATE_QUEUED {
            return Ok(());
        }
        if let Some(expected_handle) = torrent_handle {
            if self.torrent_handle_for(info_hash).await != Some(expected_handle) {
                warn!(
                    component = "storage_jobs",
                    operation = "finish_storage_move",
                    job_id = %job_id,
                    torrent = %info_hash,
                    result = "stale",
                    "discarding storage-move completion for a replaced torrent"
                );
                if succeeded && terminal_state == STORAGE_JOB_STATE_COMMIT_PENDING {
                    if let Err(error) = self
                        .complete_storage_plan_job_async(
                            job_id,
                            &completed_steps,
                            completed_byte_offset,
                        )
                        .await
                    {
                        self.schedule_storage_move_commit_retry(
                            job_id,
                            info_hash,
                            name.clone(),
                            old_save_path.clone(),
                            save_path.clone(),
                            quiesced,
                            torrent_handle,
                            completed_steps.clone(),
                            completed_byte_offset,
                            retry_attempt,
                        );
                        return Err(error);
                    }
                }
                if !succeeded {
                    self.persist_storage_terminal_state_or_schedule(
                        job_id,
                        &terminal_state,
                        error.clone(),
                        retry_attempt,
                    )
                    .await;
                }
                return Ok(());
            }
        }
        if terminal_state == JOB_STATE_PAUSED {
            // The move owns the torrent's quiesce until the job reaches a
            // terminal result. Resuming here would make a later worker
            // attempt race the torrent actor against the same payload.
            return Ok(());
        }
        if !succeeded {
            if requires_manual_recovery {
                warn!(
                    component = "storage_jobs",
                    operation = "move",
                    job_id = %job_id,
                    torrent = %info_hash,
                    result = "manual_recovery_required",
                    checkpoint = ?completed_steps,
                    error = ?error,
                    "storage move left filesystem state that cannot be resumed safely"
                );
                let reason = manual_recovery_reason(
                    error
                        .clone()
                        .unwrap_or_else(|| "storage move requires manual recovery".to_owned()),
                );
                let info_hashes = [info_hash.to_owned()];
                // Do not leave a quiesced actor attached to an ambiguous
                // payload. A later resume or inbound-peer command could
                // otherwise write against a path that this move failed to
                // classify safely.
                self.stop_torrent_task(info_hash).await;
                if let Err(persist_error) = self
                    .persist_storage_job_manual_recovery(
                        job_id,
                        &info_hashes,
                        reason.clone(),
                        "storage move completion required manual recovery",
                    )
                    .await
                {
                    warn!(
                        component = "storage_jobs",
                        operation = "persist_manual_recovery",
                        job_id = %job_id,
                        torrent = %info_hash,
                        result = "error",
                        error = %persist_error,
                        "failed to persist storage-move manual-recovery marker at completion"
                    );
                    self.persist_storage_terminal_state_or_schedule(
                        job_id,
                        JOB_STATE_FAILED,
                        Some(format!(
                            "{reason}; failed to persist manual-recovery marker: {persist_error}"
                        )),
                        retry_attempt,
                    )
                    .await;
                }
                if let Err(mark_error) = self.mark_torrent_manual_recovery(info_hash, &reason).await
                {
                    warn!(
                        component = "storage_jobs",
                        operation = "mark_manual_recovery",
                        job_id = %job_id,
                        torrent = %info_hash,
                        result = "error",
                        error = %mark_error,
                        "failed to persist storage-move manual-recovery state after stopping its task"
                    );
                    return Ok(());
                }
                self.append_session_event(
                    Some(info_hash),
                    EVENT_FIELDS_UPDATED,
                    Some("storage move requires manual recovery; torrent task stopped"),
                    serde_json::json!({
                        "job_id": job_id,
                        "old_save_path": old_save_path,
                        "save_path": save_path,
                        "storage_move": "manual_recovery_required",
                        "terminal_state": terminal_state,
                        "error": error,
                        "completed_steps": completed_steps,
                    }),
                );
                return Ok(());
            }
            self.persist_storage_terminal_state_or_schedule(
                job_id,
                &terminal_state,
                error.clone(),
                retry_attempt,
            )
            .await;
            if let Err(resume_error) = self
                .resume_torrent_after_storage_move(info_hash, quiesced, None, torrent_handle)
                .await
            {
                warn!(
                    component = "storage_jobs",
                    operation = "resume_after_move_failure",
                    job_id = %job_id,
                    torrent = %info_hash,
                    result = "error",
                    error = %resume_error,
                    "failed to restore torrent activity after storage move failed"
                );
            }
            self.append_session_event(
                Some(info_hash),
                EVENT_FIELDS_UPDATED,
                Some("storage move failed; save path unchanged"),
                serde_json::json!({
                    "job_id": job_id,
                    "save_path": old_save_path,
                    "storage_move": "failed",
                    "terminal_state": terminal_state,
                    "error": error,
                    "completed_steps": completed_steps,
                }),
            );
            return Ok(());
        }

        let retry_name = name.clone();
        let entry = {
            let mut registry = self.registry.write().await;
            let was_dormant = registry.is_dormant(info_hash);
            let updated_entry = {
                let Some(mut entry) = registry.get_mut(info_hash) else {
                    drop(registry);
                    if let Err(resume_error) = self
                        .resume_torrent_after_storage_move(
                            info_hash,
                            quiesced,
                            None,
                            torrent_handle,
                        )
                        .await
                    {
                        warn!(
                            component = "storage_jobs",
                            operation = "resume_after_move_missing_torrent",
                            job_id = %job_id,
                            torrent = %info_hash,
                            result = "error",
                            error = %resume_error,
                            "failed to restore torrent activity after storage move lost its registry entry"
                        );
                    }
                    return Err(format!(
                        "torrent {info_hash} disappeared during storage move"
                    ));
                };
                if let Some(name) = name {
                    entry.name = name;
                }
                entry.save_path = save_path.to_string_lossy().to_string();
                entry.clone()
            };
            // `get_mut` materializes dormant rows so it can expose the
            // normal mutable projection. A successful storage move does not
            // create a runtime task, so restore the compact representation
            // before releasing the registry lock; otherwise every move of a
            // cold torrent permanently promotes it into the active tier.
            if was_dormant {
                if let Err(error) = registry.demote(info_hash) {
                    warn!(
                        component = "storage_jobs",
                        operation = "demote_after_storage_move",
                        job_id = %job_id,
                        torrent = %info_hash,
                        result = "error",
                        error = %error,
                        "failed to restore dormant registry representation after storage move"
                    );
                }
            }
            updated_entry
        };
        let info_hash_for_db = info_hash.to_owned();
        let name_for_db = entry.name.clone();
        let save_path_for_db = entry.save_path.clone();
        let persistence_error = self
            .run_db("persist_storage_move_projection", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                let updated = rt_db::update_fields_in_tx(
                    &tx,
                    &info_hash_for_db,
                    &name_for_db,
                    &save_path_for_db,
                )
                .map_err(|error| error.to_string())?;
                if !updated {
                    return Err(format!(
                        "torrent {info_hash_for_db} is missing from the database"
                    ));
                }
                tx.commit().map_err(|error| error.to_string())
            })
            .await
            .err();
        if let Some(error) = persistence_error {
            // The worker's filesystem transaction is already committed and
            // the durable job remains `commit_pending`. Keeping the live
            // projection on the destination is safer than resuming a task
            // against the now-missing old path; restart recovery will retry
            // the database projection commit from the durable plan context.
            if let Err(resume_error) = self
                .resume_torrent_after_storage_move(
                    info_hash,
                    quiesced,
                    Some(save_path.clone()),
                    torrent_handle,
                )
                .await
            {
                warn!(
                    component = "storage_jobs",
                    operation = "resume_after_move_db_persistence_failure",
                    job_id = %job_id,
                    torrent = %info_hash,
                    result = "error",
                    error = %resume_error,
                    "failed to repoint torrent task after filesystem-committed storage move"
                );
            }
            self.schedule_storage_move_commit_retry(
                job_id,
                info_hash,
                retry_name,
                old_save_path.clone(),
                save_path.clone(),
                quiesced,
                torrent_handle,
                completed_steps.clone(),
                completed_byte_offset,
                retry_attempt,
            );
            self.append_session_event(
                Some(info_hash),
                EVENT_FIELDS_UPDATED,
                Some("storage move completed on disk but durable save path persistence failed"),
                serde_json::json!({
                    "job_id": job_id,
                    "old_save_path": old_save_path,
                    "save_path": save_path,
                    "storage_move": "filesystem_committed_db_failed",
                    "terminal_state": terminal_state,
                    "error": error,
                    "completed_steps": completed_steps,
                    "retry_attempt": retry_attempt,
                }),
            );
            return Err(error);
        }
        if let Err(error) = self
            .resume_torrent_after_storage_move(
                info_hash,
                quiesced,
                Some(save_path.clone()),
                torrent_handle,
            )
            .await
        {
            // The filesystem and torrent projection are already committed,
            // but the live task still owns its old save root until it accepts
            // this command. Leave the durable job commit-pending and retry
            // the actor-side handoff before reporting completion.
            self.schedule_storage_move_commit_retry(
                job_id,
                info_hash,
                retry_name,
                old_save_path.clone(),
                save_path.clone(),
                quiesced,
                torrent_handle,
                completed_steps.clone(),
                completed_byte_offset,
                retry_attempt,
            );
            self.append_session_event(
                Some(info_hash),
                EVENT_FIELDS_UPDATED,
                Some("storage move committed but task save path handoff failed"),
                serde_json::json!({
                    "job_id": job_id,
                    "old_save_path": old_save_path,
                    "save_path": save_path,
                    "storage_move": "committed_task_handoff_failed",
                    "terminal_state": terminal_state,
                    "error": error,
                    "completed_steps": completed_steps,
                    "retry_attempt": retry_attempt,
                }),
            );
            return Err(error);
        }
        if let Err(error) = self
            .complete_storage_plan_job_async(job_id, &completed_steps, completed_byte_offset)
            .await
        {
            self.schedule_storage_move_commit_retry(
                job_id,
                info_hash,
                retry_name,
                old_save_path.clone(),
                save_path.clone(),
                quiesced,
                torrent_handle,
                completed_steps.clone(),
                completed_byte_offset,
                retry_attempt,
            );
            self.append_session_event(
                Some(info_hash),
                EVENT_FIELDS_UPDATED,
                Some("storage move committed but durable job completion failed"),
                serde_json::json!({
                    "job_id": job_id,
                    "save_path": save_path,
                    "storage_move": "committed_job_completion_failed",
                    "terminal_state": terminal_state,
                    "error": error,
                    "completed_steps": completed_steps,
                    "retry_attempt": retry_attempt,
                }),
            );
            return Err(error);
        }
        self.append_session_event(
            Some(info_hash),
            EVENT_FIELDS_UPDATED,
            Some("torrent fields updated after storage move"),
            serde_json::json!({
                "job_id": job_id,
                "name": entry.name,
                "save_path": entry.save_path,
                "storage_move": "completed",
            }),
        );
        Ok(())
    }

    /// Persist a specialized storage completion's terminal state without
    /// replaying its torrent handoff. Move/delete finalizers can safely
    /// release or stop their task even when this write fails; the bounded
    /// retry keeps a transient SQLite failure from leaving the durable job
    /// active until the next daemon restart.
    pub(super) async fn persist_storage_terminal_state_or_schedule(
        &self,
        job_id: &str,
        state: &str,
        error: Option<String>,
        retry_attempt: u8,
    ) {
        if !matches!(state, JOB_STATE_CANCELLED | JOB_STATE_FAILED) {
            return;
        }
        if let Err(persist_error) = self
            .update_job_state_async(
                job_id,
                state,
                error.clone(),
                Some("storage worker terminal state persisted by engine"),
            )
            .await
        {
            warn!(
                component = "storage_jobs",
                operation = "persist_terminal_completion",
                job_id,
                state,
                retry_attempt,
                result = "retry",
                error = %persist_error,
                "specialized storage worker terminal state was not durable"
            );
            self.schedule_storage_terminal_state_retry(
                job_id,
                state,
                error.or(Some(persist_error)),
                retry_attempt,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn schedule_storage_delete_commit_retry(
        &self,
        job_id: &str,
        info_hash: &str,
        completed_steps: Vec<usize>,
        completed_byte_offset: Option<i64>,
        quiesced: Vec<(String, bool)>,
        quiesced_handle: Option<TorrentHandle>,
        retry_attempt: u8,
    ) {
        if retry_attempt >= STORAGE_COMMIT_MAX_RETRIES {
            warn!(
                component = "storage_jobs",
                operation = "retry_storage_delete_commit",
                job_id,
                torrent = %info_hash,
                retry_attempt,
                result = "exhausted",
                "stale payload-delete completion could not finalize its durable job"
            );
            return;
        }
        let exponent = u32::from(retry_attempt).min(4);
        let delay = STORAGE_COMMIT_RETRY_BASE
            .checked_mul(1_u32 << exponent)
            .unwrap_or(Duration::from_secs(5));
        let cmd_tx = self.cmd_tx.clone();
        let job_id = job_id.to_owned();
        let info_hash = info_hash.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            send_engine_command_until_actor_stops(
                cmd_tx,
                EngineCmd::StorageDeleteFinished {
                    job_id,
                    info_hash,
                    succeeded: true,
                    terminal_state: STORAGE_JOB_STATE_COMMIT_PENDING.to_owned(),
                    error: None,
                    completed_steps,
                    completed_byte_offset,
                    requires_manual_recovery: false,
                    quiesced,
                    quiesced_handle,
                    retry_attempt: retry_attempt.saturating_add(1),
                },
                "storage_delete_commit_retry",
            )
            .await;
        });
    }

    pub(super) fn schedule_storage_terminal_state_retry(
        &self,
        job_id: &str,
        state: &str,
        error: Option<String>,
        retry_attempt: u8,
    ) {
        if retry_attempt >= STORAGE_COMMIT_MAX_RETRIES {
            warn!(
                component = "storage_jobs",
                operation = "retry_terminal_state",
                job_id,
                state,
                retry_attempt,
                result = "exhausted",
                "specialized storage job terminal-state retries exhausted"
            );
            return;
        }
        let exponent = u32::from(retry_attempt).min(4);
        let delay = STORAGE_COMMIT_RETRY_BASE
            .checked_mul(1_u32 << exponent)
            .unwrap_or(Duration::from_secs(5));
        let cmd_tx = self.cmd_tx.clone();
        let job_id = job_id.to_owned();
        let state = state.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            send_engine_command_until_actor_stops(
                cmd_tx,
                EngineCmd::StorageTerminalStateRetry {
                    job_id,
                    state,
                    error,
                    retry_attempt: retry_attempt.saturating_add(1),
                },
                "storage_terminal_state_retry",
            )
            .await;
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn schedule_storage_plan_completion_retry(
        &self,
        job_id: &str,
        affected_torrents: Vec<(String, bool)>,
        affected_torrent_handles: Vec<(String, TorrentHandle)>,
        manual_recovery_torrents: Vec<String>,
        succeeded: bool,
        terminal_state: String,
        error: Option<String>,
        completed_steps: Vec<usize>,
        completed_byte_offset: Option<i64>,
        requires_manual_recovery: bool,
        resume_torrents: bool,
        retry_attempt: u8,
    ) {
        if retry_attempt >= STORAGE_COMMIT_MAX_RETRIES {
            warn!(
                component = "storage_jobs",
                operation = "retry_storage_plan_commit",
                job_id,
                retry_attempt,
                result = "exhausted",
                "storage plan actor-side commit retries exhausted; job remains commit_pending"
            );
            return;
        }
        let exponent = u32::from(retry_attempt).min(4);
        let delay = STORAGE_COMMIT_RETRY_BASE
            .checked_mul(1_u32 << exponent)
            .unwrap_or(Duration::from_secs(5));
        let cmd_tx = self.cmd_tx.clone();
        let job_id = job_id.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            send_engine_command_until_actor_stops(
                cmd_tx,
                EngineCmd::StoragePlanFinished {
                    job_id,
                    affected_torrents,
                    affected_torrent_handles,
                    manual_recovery_torrents,
                    succeeded,
                    terminal_state,
                    error,
                    completed_steps,
                    completed_byte_offset,
                    requires_manual_recovery,
                    resume_torrents,
                    retry_attempt: retry_attempt.saturating_add(1),
                },
                "storage_plan_commit_retry",
            )
            .await;
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn schedule_storage_move_commit_retry(
        &self,
        job_id: &str,
        info_hash: &str,
        name: Option<String>,
        old_save_path: PathBuf,
        save_path: PathBuf,
        quiesced: Option<bool>,
        torrent_handle: Option<TorrentHandle>,
        completed_steps: Vec<usize>,
        completed_byte_offset: Option<i64>,
        retry_attempt: u8,
    ) {
        if retry_attempt >= STORAGE_COMMIT_MAX_RETRIES {
            warn!(
                component = "storage_jobs",
                operation = "retry_storage_move_commit",
                job_id,
                torrent = %info_hash,
                retry_attempt,
                result = "exhausted",
                "storage move actor-side commit retries exhausted; job remains commit_pending"
            );
            return;
        }
        let exponent = u32::from(retry_attempt).min(4);
        let delay = STORAGE_COMMIT_RETRY_BASE
            .checked_mul(1_u32 << exponent)
            .unwrap_or(Duration::from_secs(5));
        let cmd_tx = self.cmd_tx.clone();
        let job_id = job_id.to_owned();
        let info_hash = info_hash.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // This retry carries the durable filesystem-commit handoff. It
            // must not expire while the actor is temporarily processing a
            // long command, or the job remains commit-pending with a stale
            // live projection.
            send_engine_command_until_actor_stops(
                cmd_tx,
                EngineCmd::StorageMoveFinished {
                    job_id,
                    info_hash,
                    name,
                    old_save_path,
                    save_path,
                    quiesced,
                    torrent_handle,
                    succeeded: true,
                    terminal_state: STORAGE_JOB_STATE_COMMIT_PENDING.to_owned(),
                    error: None,
                    completed_steps,
                    completed_byte_offset,
                    requires_manual_recovery: false,
                    retry_attempt: retry_attempt.saturating_add(1),
                },
                "storage_move_commit_retry",
            )
            .await;
        });
    }
}
