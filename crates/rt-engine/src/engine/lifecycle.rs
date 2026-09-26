//! Torrent creation, removal, task promotion/demotion, and peer-routing lifecycle.
//! These methods run on the engine actor and preserve its state transition order.

use super::*;

impl Engine {
    pub(super) async fn begin_torrent_add(&mut self, request: TorrentAddRequest) {
        let info_hash = meta_info_hash_hex(&request.meta);
        if self.runtime.torrent_chans.contains_key(&info_hash)
            || self.runtime.pending_torrent_adds.contains(&info_hash)
            || self.registry.read().await.get(&info_hash).is_some()
        {
            let _ = request
                .reply
                .send(Err(format!("torrent {info_hash} already added")));
            return;
        }
        self.runtime.pending_torrent_adds.insert(info_hash.clone());
        if !self.start_torrent_add_from_meta(request) {
            self.runtime.pending_torrent_adds.remove(&info_hash);
        }
    }

    pub(super) fn start_torrent_add_from_meta(&self, request: TorrentAddRequest) -> bool {
        let TorrentAddRequest {
            meta,
            save_path,
            paused,
            category,
            tags,
            reply,
            add_guard,
            parse_memory_lease,
        } = request;
        if let Err(error) = validate_torrent_meta(&meta) {
            let _ = reply.send(Err(error));
            return false;
        }
        let Some(add_guard) = add_guard.or_else(try_acquire_engine_add_task) else {
            let _ = reply.send(Err("torrent add preparation capacity exhausted".to_owned()));
            return false;
        };
        let info_hash = meta_info_hash_hex(&meta);
        let raw = meta_raw(&meta).to_vec();
        let config = Arc::clone(&self.config);
        let cmd_tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let failure_info_hash = info_hash.clone();
            let failure_cmd_tx = cmd_tx.clone();
            let blob_result = match tokio::task::spawn_blocking(move || {
                save_torrent_blob_staging_from_config(&config, &info_hash, &raw)
                    .map_err(|error| error.to_string())
            })
            .await
            {
                Ok(result) => result,
                Err(error) => Err(crate::task_join_error_summary(
                    "torrent blob worker",
                    &error,
                )),
            };
            let staged_blob_for_cleanup = blob_result.as_ref().ok().cloned();
            let delivered = send_engine_command_until_delivered(
                cmd_tx,
                EngineCmd::PreparedTorrentAdd {
                    add_guard,
                    meta: Box::new(meta),
                    staged_blob: blob_result,
                    parse_memory_lease,
                    save_path,
                    paused,
                    category,
                    tags,
                    reply,
                },
                "torrent_add_blob_completion",
            )
            .await;
            if !delivered {
                if let Some(path) = staged_blob_for_cleanup.as_deref() {
                    remove_staged_blob_path_best_effort(path, "torrent_add_actor_gone_cleanup");
                }
                send_engine_command_until_actor_stops(
                    failure_cmd_tx,
                    EngineCmd::TorrentAddDeliveryFailed {
                        info_hash: failure_info_hash,
                        error: "torrent add completion delivery timed out".to_owned(),
                    },
                    "torrent_add_failure_cleanup",
                )
                .await;
            }
        });
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_torrent_add_from_raw(
        &self,
        add_guard: EngineAddTaskGuard,
        raw: Vec<u8>,
        parse_memory_lease: MemoryLease,
        save_path: Option<PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
        reply: oneshot::Sender<CmdResult<String>>,
    ) {
        let cmd_tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let prepared = tokio::task::spawn_blocking(move || {
                let mut parse_memory_lease = parse_memory_lease;
                let result = parse_torrent_with_memory_lease(&raw, &mut parse_memory_lease)
                    .map(Box::new)
                    .map_err(|error| error.to_string());
                (result, parse_memory_lease)
            })
            .await;
            let (prepared, parse_memory_lease) = match prepared {
                Ok((prepared, parse_memory_lease)) => (prepared, parse_memory_lease),
                Err(error) => {
                    drop(add_guard);
                    let _ = reply.send(Err(crate::task_join_error_summary(
                        "torrent preparation worker",
                        &error,
                    )));
                    return;
                }
            };
            send_engine_command_until_delivered(
                cmd_tx,
                EngineCmd::PreparedTorrentMeta {
                    add_guard,
                    prepared,
                    parse_memory_lease,
                    save_path,
                    paused,
                    category,
                    tags,
                    reply,
                },
                "torrent_metadata_parse_completion",
            )
            .await;
        });
    }

    #[cfg(test)]
    pub(super) async fn add_torrent(
        &mut self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let info_hash_hex = meta_info_hash_hex(&meta);
        self.save_torrent_blob(&info_hash_hex, meta_raw(&meta))
            .map_err(|error| error.to_string())?;
        self.add_torrent_after_blob(meta, save_path, paused, category, tags, None)
            .await
    }

    pub(super) async fn add_torrent_after_blob(
        &mut self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
        _parse_memory_lease: Option<MemoryLease>,
    ) -> CmdResult<String> {
        validate_torrent_meta(&meta)?;
        validate_category_value(category.as_deref())?;
        validate_save_path_value(save_path.as_deref())?;
        validate_meta_tracker_urls(&meta)?;
        validate_command_item_len(tags.len(), MAX_ENGINE_MUTATION_ITEMS, "torrent tag list")?;
        validate_label_bytes(&tags, "torrent tag list")?;
        let info_hash_hex = meta_info_hash_hex(&meta);
        let piece_index_memory_lease =
            reserve_piece_index_memory_for_meta(&self.services.resources, &meta)?;
        let torrent_metadata_memory_lease =
            reserve_torrent_metadata_memory_for_meta(&self.services.resources, &meta)?;

        if self.runtime.torrent_chans.contains_key(&info_hash_hex)
            || self.runtime.pending_torrent_adds.contains(&info_hash_hex)
            || self.registry.read().await.get(&info_hash_hex).is_some()
        {
            return Err(format!("torrent {info_hash_hex} already added"));
        }

        let save = save_path.unwrap_or_else(|| self.config.storage.download_dir.clone());
        self.authorize_storage_path_async(&save).await?;

        // Register in session
        {
            let mut reg = self.registry.write().await;
            let mut entry = TorrentEntry::new(
                info_hash_hex.clone(),
                meta.name().to_owned(),
                save.to_string_lossy().into_owned(),
            );
            entry.total_length = meta_total_length(&meta);
            entry.amount_left = entry.total_length;
            entry.category = normalize_category(category);
            entry.tags = normalize_tags(tags);
            reg.add(entry).map_err(|e| e.to_string())?;
            // TorrentEntry starts in Stopped; transition to target state.
            let target = if paused {
                TorrentState::Paused
            } else {
                TorrentState::Downloading
            };
            if let Some(mut e) = reg.get_mut(&info_hash_hex) {
                let _ = e.transition(target);
            };
        }

        let is_private = meta.is_private();
        let torrent_name = meta.name().to_owned();
        let v2_only = matches!(meta, TorrentMeta::V2(_));
        let dht_info_hash = meta_dht_info_hash(&meta);
        let added_event = self.session_event_row(
            Some(&info_hash_hex),
            EVENT_TORRENT_ADDED,
            Some("torrent added"),
            serde_json::json!({
                "paused": paused,
                "private": is_private,
                "name": torrent_name,
                "v2_only": v2_only,
            }),
        );
        let persisted = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(&info_hash_hex)
                .ok_or_else(|| format!("torrent {info_hash_hex} missing from registry"))?;
            self.persist_entry_with_event(&entry, &meta, Some(&added_event))
                .await
        };
        if let Err(error) = persisted {
            // Same rollback, and also clean up the blob the previous step
            // wrote -- left alone it would be an orphan file with nothing
            // in the registry or DB pointing at it.
            let _ = self.registry.write().await.remove(&info_hash_hex);
            if let Err(cleanup_error) =
                rt_storage::remove_file_no_follow(&torrent_blob_path(&self.config, &info_hash_hex))
            {
                if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                    warn!(
                        component = "engine",
                        operation = "add_torrent_rollback",
                        torrent = %info_hash_hex,
                        error = %cleanup_error,
                        "failed to remove orphaned torrent blob after a failed add"
                    );
                }
            }
            return Err(error.to_string());
        }

        let Some(piece_index_memory_lease) = piece_index_memory_lease else {
            return Err("torrent add lost its piece-index memory reservation".to_owned());
        };
        let Some(torrent_metadata_memory_lease) = torrent_metadata_memory_lease else {
            return Err("torrent add lost its metadata memory reservation".to_owned());
        };
        let initial_state = if paused {
            TorrentState::Paused
        } else {
            TorrentState::Downloading
        };
        let _cmd_tx = self
            .spawn_torrent_task_for_meta(
                info_hash_hex.clone(),
                meta,
                save,
                paused,
                initial_state,
                piece_index_memory_lease,
                torrent_metadata_memory_lease,
            )
            .await;
        if !paused && !is_private {
            self.register_dht_torrent(dht_info_hash, &info_hash_hex)
                .await;
        }
        info!(
            component = "engine",
            operation = "add_torrent",
            torrent = %info_hash_hex,
            paused,
            result = "ok",
            "torrent added"
        );
        Ok(info_hash_hex)
    }

    pub(super) async fn add_magnet(
        &mut self,
        mut magnet: MagnetLink,
        _input_memory_lease: MemoryLease,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        validate_magnet_input(&magnet)?;
        validate_category_value(category.as_deref())?;
        validate_save_path_value(save_path.as_deref())?;
        validate_tracker_urls(&magnet.trackers)?;
        validate_command_item_len(tags.len(), MAX_ENGINE_MUTATION_ITEMS, "torrent tag list")?;
        validate_label_bytes(&tags, "torrent tag list")?;
        let metadata_identity = match (magnet.info_hash_v1, magnet.info_hash_v2) {
            (Some(v1), Some(v2)) => Some(MetadataInfoHash::Hybrid { v1, v2 }),
            (Some(v1), None) => Some(MetadataInfoHash::V1(v1)),
            (None, Some(v2)) => Some(MetadataInfoHash::V2(v2)),
            (None, None) => None,
        };
        let expected_v2_hash = magnet.info_hash_v1.and(magnet.info_hash_v2);
        let tracker_input = std::mem::take(&mut magnet.trackers);
        let direct_peer_input = std::mem::take(&mut magnet.peer_addresses);
        let (trackers, metadata_task_memory) = if metadata_identity.is_some() {
            let (trackers, memory) = prepare_metadata_task_memory_with_peers(
                &self.services.resources,
                tracker_input,
                direct_peer_input,
                self.config.network.max_peers,
            )?;
            (trackers, Some(memory))
        } else {
            (compact_metadata_task_trackers(tracker_input), None)
        };
        let info_hash_hex = magnet
            .info_hash_v1
            .map(hex::encode)
            .or_else(|| magnet.info_hash_v2.map(hex::encode))
            .ok_or_else(|| "magnet is missing an info hash".to_owned())?;
        drop(_input_memory_lease);
        if self.runtime.torrent_chans.contains_key(&info_hash_hex)
            || self.runtime.pending_torrent_adds.contains(&info_hash_hex)
            || self.registry.read().await.get(&info_hash_hex).is_some()
        {
            return Err(format!("torrent {info_hash_hex} already added"));
        }

        let save = save_path.unwrap_or_else(|| self.config.storage.download_dir.clone());
        self.authorize_storage_path_async(&save).await?;
        let name = magnet
            .display_name
            .clone()
            .unwrap_or_else(|| info_hash_hex.clone());
        let mut entry = TorrentEntry::new(
            info_hash_hex.clone(),
            name,
            save.to_string_lossy().into_owned(),
        );
        entry.category = normalize_category(category);
        entry.tags = normalize_tags(tags);
        entry.state = if paused {
            TorrentState::Paused
        } else {
            TorrentState::MetadataPending
        };

        {
            let mut reg = self.registry.write().await;
            reg.add(entry.clone()).map_err(|e| e.to_string())?;
        }

        let row = TorrentRow {
            info_hash: entry.info_hash.clone(),
            name: entry.name.clone(),
            total_length: 0,
            piece_length: 0,
            piece_count: 0,
            is_private: false,
            save_path: entry.save_path.clone(),
            category: entry.category.clone(),
            tags: entry.tags.clone(),
            state: entry.state.as_str().to_owned(),
            added_at: db_i64(entry.added_at),
            completed_at: None,
            uploaded: 0,
            downloaded: 0,
            amount_left: 0,
            ratio: 0.0,
            trackers: trackers.clone(),
        };
        let tracker_rows = tracker_rows_from_urls(
            &entry.info_hash,
            &trackers,
            db_i64(entry.stats.uploaded),
            db_i64(entry.stats.downloaded),
            db_i64(entry.amount_left),
        );
        let tracker_count = trackers.len();
        let tracker_bytes = tracker_urls_bytes(&trackers);
        let added_event = self.session_event_row(
            Some(&info_hash_hex),
            EVENT_MAGNET_ADDED,
            Some("magnet added as metadata pending"),
            serde_json::json!({
                "paused": paused,
                "tracker_count": tracker_count,
                "tracker_bytes": tracker_bytes,
                "v2_only": magnet.info_hash_v1.is_none(),
            }),
        );
        let retention = self.config.logging.event_retention;
        let persistence = self
            .run_db("add_magnet", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                if let Some(expected_v2_hash) = expected_v2_hash.as_ref() {
                    rt_db::upsert_torrent_metadata_v2_hash_in_tx(
                        &tx,
                        &row.info_hash,
                        expected_v2_hash,
                    )
                    .map_err(|error| error.to_string())?;
                }
                rt_db::replace_torrent_trackers_in_tx(&tx, &entry.info_hash, &tracker_rows)
                    .map_err(|error| error.to_string())?;
                rt_db::append_session_event_in_tx(&tx, &added_event)
                    .map_err(|error| error.to_string())?;
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            let _ = self.registry.write().await.remove(&info_hash_hex);
            return Err(error);
        }

        if let Some(metadata_identity) = metadata_identity {
            let Some(metadata_task_memory) = metadata_task_memory else {
                return Err("magnet add lost its metadata task memory reservation".to_owned());
            };
            let _cmd_tx = self.spawn_metadata_task(
                metadata_identity,
                info_hash_hex.clone(),
                trackers,
                paused,
                if paused {
                    TorrentState::Paused
                } else {
                    TorrentState::MetadataPending
                },
                metadata_task_memory,
            );
            if !paused {
                self.register_dht_torrent(metadata_identity.wire_hash(), &info_hash_hex)
                    .await;
            }
        }
        info!(
            component = "engine",
            operation = "add_magnet",
            torrent = %info_hash_hex,
            paused,
            result = "ok",
            "magnet added as metadata pending"
        );
        Ok(info_hash_hex)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_magnet_blob_persistence(
        &self,
        info_hash: String,
        raw: Vec<u8>,
        meta: CmdResult<TorrentMeta>,
        metadata_memory_lease: MemoryLease,
        parse_memory_lease: MemoryLease,
        completion_guard: EngineMagnetCompletionGuard,
        source: mpsc::Sender<TorrentCmd>,
    ) {
        let config = Arc::clone(&self.config);
        let cmd_tx = self.cmd_tx.clone();
        let blob_info_hash = info_hash.clone();
        let failure_info_hash = info_hash.clone();
        let failure_source = source.clone();
        let failure_cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            let blob = if meta.is_ok() {
                let staging_path = magnet_blob_staging_path(&config, &blob_info_hash);
                match tokio::task::spawn_blocking(move || {
                    save_magnet_blob_staging(&raw, &staging_path)
                        .map(Some)
                        .map_err(|error| error.to_string())
                })
                .await
                {
                    Ok(result) => result,
                    Err(error) => Err(crate::task_join_error_summary("magnet blob worker", &error)),
                }
            } else {
                Ok(None)
            };
            let staged_blob_for_cleanup = blob.as_ref().ok().and_then(|path| path.clone());
            let delivered = send_engine_command_until_delivered(
                cmd_tx,
                EngineCmd::PreparedMagnetBlob {
                    info_hash,
                    meta,
                    blob,
                    metadata_memory_lease,
                    parse_memory_lease,
                    completion_guard,
                    source,
                },
                "magnet_blob_completion",
            )
            .await;
            if !delivered {
                if let Some(path) = staged_blob_for_cleanup.as_deref() {
                    remove_staged_blob_path_best_effort(path, "magnet_blob_actor_gone_cleanup");
                }
                send_metadata_completion_failure(
                    failure_cmd_tx,
                    failure_info_hash,
                    failure_source,
                    "magnet_blob_failure_cleanup",
                )
                .await;
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn retry_magnet_metadata_after_storage_job(
        &self,
        info_hash: String,
        raw: Vec<u8>,
        meta: CmdResult<TorrentMeta>,
        metadata_memory_lease: MemoryLease,
        parse_memory_lease: MemoryLease,
        completion_guard: EngineMagnetCompletionGuard,
        source: mpsc::Sender<TorrentCmd>,
    ) {
        let cmd_tx = self.cmd_tx.clone();
        let failure_info_hash = info_hash.clone();
        let failure_source = source.clone();
        let failure_cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(MAGNET_METADATA_STORAGE_RETRY_DELAY).await;
            let delivered = send_engine_command_until_delivered(
                cmd_tx,
                EngineCmd::PreparedMagnetMetadata {
                    info_hash,
                    raw,
                    meta,
                    metadata_memory_lease,
                    parse_memory_lease,
                    completion_guard,
                    source,
                },
                "magnet_metadata_storage_retry",
            )
            .await;
            if !delivered {
                send_metadata_completion_failure(
                    failure_cmd_tx,
                    failure_info_hash,
                    failure_source,
                    "magnet_metadata_retry_failure_cleanup",
                )
                .await;
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn retry_magnet_blob_after_storage_job(
        &self,
        info_hash: String,
        meta: TorrentMeta,
        staged_blob: PathBuf,
        metadata_memory_lease: MemoryLease,
        parse_memory_lease: MemoryLease,
        completion_guard: EngineMagnetCompletionGuard,
        source: mpsc::Sender<TorrentCmd>,
    ) {
        let cmd_tx = self.cmd_tx.clone();
        let failure_info_hash = info_hash.clone();
        let failure_source = source.clone();
        let failure_cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(MAGNET_METADATA_STORAGE_RETRY_DELAY).await;
            let cleanup_path = staged_blob.clone();
            let delivered = send_engine_command_until_delivered(
                cmd_tx,
                EngineCmd::PreparedMagnetBlob {
                    info_hash,
                    meta: Ok(meta),
                    blob: Ok(Some(staged_blob)),
                    metadata_memory_lease,
                    parse_memory_lease,
                    completion_guard,
                    source,
                },
                "magnet_blob_storage_retry",
            )
            .await;
            if !delivered {
                remove_staged_blob_path_best_effort(
                    &cleanup_path,
                    "magnet_blob_retry_actor_gone_cleanup",
                );
                send_metadata_completion_failure(
                    failure_cmd_tx,
                    failure_info_hash,
                    failure_source,
                    "magnet_blob_retry_failure_cleanup",
                )
                .await;
            }
        });
    }

    pub(super) async fn complete_magnet_persisted(
        &mut self,
        info_hash_hex: &str,
        meta: &mut Option<TorrentMeta>,
        staged_blob: &mut Option<PathBuf>,
    ) -> CmdResult<()> {
        if self.runtime.pending_torrent_deletes.contains(info_hash_hex) {
            self.remove_magnet_blob_candidate_best_effort(
                info_hash_hex,
                staged_blob.as_deref(),
                "magnet_completion_remove",
            );
            return Err(format!(
                "torrent {info_hash_hex} is being removed; wait for payload cleanup"
            ));
        }
        let Some(meta_ref) = meta.as_ref() else {
            return Err("magnet completion lost parsed metadata".to_owned());
        };
        if let Err(error) = validate_meta_tracker_urls(meta_ref) {
            self.remove_magnet_blob_candidate_best_effort(
                info_hash_hex,
                staged_blob.as_deref(),
                "magnet_completion_tracker_limit",
            );
            return Err(error);
        }
        if let Err(error) = self.ensure_torrent_storage_idle(info_hash_hex).await {
            if !is_storage_job_busy_error(&error) {
                self.remove_magnet_blob_candidate_best_effort(
                    info_hash_hex,
                    staged_blob.as_deref(),
                    "magnet_completion_reject",
                );
            }
            return Err(error);
        }
        let meta = meta
            .take()
            .ok_or_else(|| "magnet completion lost parsed metadata".to_owned())?;
        let mut staged_blob = staged_blob.take();
        let fetched_hash = meta_info_hash_hex(&meta);
        if fetched_hash != info_hash_hex {
            self.remove_magnet_blob_candidate_best_effort(
                info_hash_hex,
                staged_blob.as_deref(),
                "magnet_completion_hash_mismatch",
            );
            return Err(format!(
                "fetched metadata hash {fetched_hash} does not match magnet {info_hash_hex}"
            ));
        }
        let is_private = meta.is_private();
        let torrent_name = meta.name().to_owned();
        let total_length = meta_total_length(&meta);
        let v2_only = matches!(meta, TorrentMeta::V2(_));
        let dht_info_hash = meta_dht_info_hash(&meta);
        let piece_index_memory_lease =
            match reserve_piece_index_memory_for_meta(&self.services.resources, &meta) {
                Ok(lease) => lease,
                Err(error) => {
                    self.remove_magnet_blob_candidate_best_effort(
                        info_hash_hex,
                        staged_blob.as_deref(),
                        "magnet_completion_piece_index_limit",
                    );
                    return Err(error);
                }
            };
        let torrent_metadata_memory_lease =
            match reserve_torrent_metadata_memory_for_meta(&self.services.resources, &meta) {
                Ok(lease) => lease,
                Err(error) => {
                    self.remove_magnet_blob_candidate_best_effort(
                        info_hash_hex,
                        staged_blob.as_deref(),
                        "magnet_completion_metadata_memory_limit",
                    );
                    return Err(error);
                }
            };

        let (save, category, tags, previous_entry, previous_was_dormant) = {
            let reg = self.registry.read().await;
            let Some(entry) = reg.get(info_hash_hex) else {
                // A detached metadata/blob worker can finish after a caller
                // removed the placeholder. Do not recreate an orphaned blob
                // after the durable/session projection is gone.
                self.remove_magnet_blob_candidate_best_effort(
                    info_hash_hex,
                    staged_blob.as_deref(),
                    "magnet_completion_orphan",
                );
                return Err(format!(
                    "metadata-pending torrent {info_hash_hex} not found"
                ));
            };
            (
                PathBuf::from(&entry.save_path),
                entry.category.clone(),
                entry.tags.clone(),
                entry.clone(),
                reg.is_dormant(info_hash_hex),
            )
        };
        // Metadata completion is serialized by the engine actor, so this is
        // the authoritative user intent captured before replacing the
        // metadata task. A pause or stopped state that arrived while metadata
        // was in flight must survive completion instead of being silently
        // converted into a downloading torrent.
        let initial_state = match previous_entry.state {
            TorrentState::Paused | TorrentState::Stopped => previous_entry.state,
            _ => TorrentState::Downloading,
        };
        let start_paused = matches!(initial_state, TorrentState::Paused | TorrentState::Stopped);
        if let Err(error) = self.authorize_storage_path_async(&save).await {
            self.remove_magnet_blob_candidate_best_effort(
                info_hash_hex,
                staged_blob.as_deref(),
                "magnet_completion_reject",
            );
            return Err(error);
        }

        let transition_error = {
            let mut reg = self.registry.write().await;
            let mut entry = reg
                .get_mut(info_hash_hex)
                .ok_or_else(|| format!("metadata-pending torrent {info_hash_hex} not found"))?;
            entry.name = torrent_name.clone();
            entry.total_length = total_length;
            entry.amount_left = total_length;
            entry.category = category;
            entry.tags = tags;
            entry.transition(initial_state).err()
        };
        if let Some(error) = transition_error {
            self.restore_registry_entry(info_hash_hex, previous_entry, previous_was_dormant)
                .await;
            self.remove_magnet_blob_candidate_best_effort(
                info_hash_hex,
                staged_blob.as_deref(),
                "magnet_completion_reject",
            );
            return Err(error.to_string());
        }
        if let Some(staged_blob_path) = staged_blob.take() {
            let destination = torrent_blob_path(&self.config, info_hash_hex);
            if let Err(error) = rt_storage::rename_no_follow(&staged_blob_path, &destination) {
                self.restore_registry_entry(info_hash_hex, previous_entry, previous_was_dormant)
                    .await;
                self.remove_magnet_blob_path_best_effort(
                    &staged_blob_path,
                    "magnet_completion_staging_cleanup",
                );
                return Err(format!(
                    "failed to publish fetched magnet metadata blob: {error}"
                ));
            }
            if let Some(parent) = destination.parent() {
                if let Err(error) = rt_storage::sync_dir_no_follow(parent) {
                    self.restore_registry_entry(
                        info_hash_hex,
                        previous_entry,
                        previous_was_dormant,
                    )
                    .await;
                    self.remove_magnet_blob_best_effort(
                        info_hash_hex,
                        "magnet_completion_rollback",
                    );
                    return Err(format!(
                        "fetched magnet metadata blob was not durably committed: {error}"
                    ));
                }
            }
        }
        let persisted = {
            let reg = self.registry.read().await;
            match reg.get(info_hash_hex) {
                Some(entry) => {
                    let resolved_event = self.session_event_row(
                        Some(info_hash_hex),
                        EVENT_METADATA_RESOLVED,
                        Some("magnet metadata resolved"),
                        serde_json::json!({
                            "name": torrent_name,
                            "total_length": total_length,
                            "private": is_private,
                            "v2_only": v2_only,
                        }),
                    );
                    self.persist_entry_with_event(&entry, &meta, Some(&resolved_event))
                        .await
                }
                None => Err(anyhow::anyhow!(
                    "torrent {info_hash_hex} missing after metadata update"
                )),
            }
        };
        if let Err(error) = persisted {
            self.restore_registry_entry(info_hash_hex, previous_entry, previous_was_dormant)
                .await;
            self.remove_magnet_blob_best_effort(info_hash_hex, "magnet_completion_rollback");
            return Err(error.to_string());
        }

        if let Some(old_tx) = self.runtime.torrent_chans.remove(info_hash_hex) {
            let _ = old_tx.try_send(TorrentCmd::Shutdown);
        }
        if let Some(old_task) = self.runtime.torrent_tasks.remove(info_hash_hex) {
            // Metadata completion replaces the task incarnation. Do not
            // leave the old tracker/fetch loop detached while the new
            // torrent task starts; it can otherwise retain peer permits and
            // continue external work against the superseded placeholder.
            old_task.abort();
            let mut old_task = ShutdownTaskGuard {
                task: Some(old_task),
            };
            if let Some(task) = old_task.task.as_mut() {
                let _ = task.await;
            }
        }
        // A v1 magnet has to use DHT while metadata is pending because its
        // private flag is not known yet. Once the authoritative metadata says
        // it is private, remove that provisional registration before the new
        // runtime task is installed. Otherwise private torrents continue
        // receiving DHT peers after completion.
        if is_private {
            self.unregister_dht_torrent(info_hash_hex).await;
        }
        let Some(piece_index_memory_lease) = piece_index_memory_lease else {
            return Err("magnet completion lost its piece-index memory reservation".to_owned());
        };
        let Some(torrent_metadata_memory_lease) = torrent_metadata_memory_lease else {
            return Err("magnet completion lost its metadata memory reservation".to_owned());
        };
        let _tx = self
            .spawn_torrent_task_for_meta(
                info_hash_hex.to_owned(),
                meta,
                save,
                start_paused,
                initial_state,
                piece_index_memory_lease,
                torrent_metadata_memory_lease,
            )
            .await;
        if !start_paused && !is_private {
            self.register_dht_torrent(dht_info_hash, info_hash_hex)
                .await;
        }
        info!(
            component = "engine",
            operation = "complete_magnet",
            torrent = %info_hash_hex,
            result = "ok",
            "magnet metadata completed"
        );
        Ok(())
    }

    #[cfg(test)]
    pub(super) async fn complete_magnet(
        &mut self,
        info_hash_hex: &str,
        raw: Vec<u8>,
    ) -> CmdResult<()> {
        let meta = parse_torrent(&raw).map_err(|error| error.to_string())?;
        self.save_torrent_blob(info_hash_hex, &raw)
            .map_err(|error| error.to_string())?;
        let mut meta = Some(meta);
        let mut staged_blob = None;
        self.complete_magnet_persisted(info_hash_hex, &mut meta, &mut staged_blob)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn spawn_torrent_task(
        &mut self,
        info_hash_hex: String,
        meta: TorrentMetaV1,
        save: PathBuf,
        paused: bool,
        initial_state: TorrentState,
        piece_index_memory_lease: MemoryLease,
        torrent_metadata_memory_lease: MemoryLease,
    ) -> mpsc::Sender<TorrentCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);
        let mut task = TorrentTask::new(
            meta,
            save,
            paused,
            initial_state,
            Arc::clone(&self.registry),
            self.db_executor(),
            self.services.resources.clone(),
            cmd_rx,
            fastresume_dir(&self.config),
            self.config.network.max_peers,
            self.config.network.listen_port,
            self.config.tracker.http_timeout_secs,
            self.config.tracker.udp_timeout_secs,
            self.config.tracker.min_interval_secs,
            self.config
                .memory
                .piece_assembly_cap_mb
                .saturating_mul(1024 * 1024) as usize,
            storage_io_config_from_config(&self.config),
            self.peer_exchange_enabled().await,
            OutboundEgressPolicy::from_config(&self.config.tracker),
            self.services.network_budget.clone(),
            self.config.logging.event_retention,
            Some(piece_index_memory_lease),
        )
        .await;
        task.attach_torrent_metadata_memory_lease(torrent_metadata_memory_lease);
        let handle = tokio::spawn(task.run());
        let tier_key = info_hash_hex.clone();
        self.runtime
            .torrent_chans
            .insert(info_hash_hex.clone(), cmd_tx.clone());
        self.runtime.torrent_tasks.insert(info_hash_hex, handle);
        let now = Instant::now();
        self.runtime.tier_last_active.insert(tier_key.clone(), now);
        self.runtime.tier_controller.apply_input(
            tier_key,
            TierInput {
                state: initial_state,
                connected_peers: 0,
                outstanding_requests: 0,
                inbound_peer: false,
                tracker_due: false,
                last_active: Some(now),
                now,
            },
        );
        cmd_tx
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn spawn_torrent_task_for_meta(
        &mut self,
        info_hash_hex: String,
        meta: TorrentMeta,
        save: PathBuf,
        paused: bool,
        initial_state: TorrentState,
        piece_index_memory_lease: MemoryLease,
        torrent_metadata_memory_lease: MemoryLease,
    ) -> mpsc::Sender<TorrentCmd> {
        match meta {
            TorrentMeta::V1(meta) => {
                self.spawn_torrent_task(
                    info_hash_hex,
                    meta,
                    save,
                    paused,
                    initial_state,
                    piece_index_memory_lease,
                    torrent_metadata_memory_lease,
                )
                .await
            }
            TorrentMeta::Hybrid(meta, _) => {
                self.spawn_torrent_task(
                    info_hash_hex,
                    *meta,
                    save,
                    paused,
                    initial_state,
                    piece_index_memory_lease,
                    torrent_metadata_memory_lease,
                )
                .await
            }
            TorrentMeta::V2(meta) => {
                self.spawn_v2_torrent_task(
                    info_hash_hex,
                    meta,
                    save,
                    paused,
                    initial_state,
                    piece_index_memory_lease,
                    torrent_metadata_memory_lease,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn spawn_v2_torrent_task(
        &mut self,
        info_hash_hex: String,
        meta: TorrentMetaV2,
        save: PathBuf,
        paused: bool,
        initial_state: TorrentState,
        piece_index_memory_lease: MemoryLease,
        torrent_metadata_memory_lease: MemoryLease,
    ) -> mpsc::Sender<TorrentCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);
        let task = V2TorrentTask::new(
            meta,
            save,
            paused,
            initial_state,
            Arc::clone(&self.registry),
            self.db_executor(),
            self.services.resources.clone(),
            cmd_rx,
            self.config.network.max_peers,
            self.config
                .memory
                .piece_assembly_cap_mb
                .saturating_mul(1024 * 1024) as usize,
            storage_io_config_from_config(&self.config),
            OutboundEgressPolicy::from_config(&self.config.tracker),
            self.config.network.listen_port,
            self.config.tracker.http_timeout_secs,
            self.config.tracker.udp_timeout_secs,
            self.config.tracker.min_interval_secs,
            self.services.network_budget.clone(),
            Some(piece_index_memory_lease),
            Some(torrent_metadata_memory_lease),
        )
        .await;
        let handle = tokio::spawn(task.run());
        let tier_key = info_hash_hex.clone();
        self.runtime
            .torrent_chans
            .insert(info_hash_hex.clone(), cmd_tx.clone());
        self.runtime.torrent_tasks.insert(info_hash_hex, handle);
        let now = Instant::now();
        self.runtime.tier_last_active.insert(tier_key.clone(), now);
        self.runtime.tier_controller.apply_input(
            tier_key,
            TierInput {
                state: initial_state,
                connected_peers: 0,
                outstanding_requests: 0,
                inbound_peer: false,
                tracker_due: false,
                last_active: Some(now),
                now,
            },
        );
        cmd_tx
    }

    pub(super) fn spawn_metadata_task(
        &mut self,
        info_hash: MetadataInfoHash,
        info_hash_hex: String,
        trackers: Vec<String>,
        paused: bool,
        initial_state: TorrentState,
        task_memory: MetadataTaskMemory,
    ) -> mpsc::Sender<TorrentCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);
        let handle = tokio::spawn(run_metadata_task(
            info_hash,
            info_hash_hex.clone(),
            trackers,
            task_memory,
            cmd_rx,
            self.cmd_tx.clone(),
            cmd_tx.clone(),
            Arc::clone(&self.registry),
            self.services.resources.clone(),
            self.config.network.listen_port,
            self.config.network.max_peers,
            self.config.tracker.http_timeout_secs,
            self.config.tracker.udp_timeout_secs,
            paused,
            OutboundEgressPolicy::from_config(&self.config.tracker),
            self.services.network_budget.clone(),
        ));
        let tier_key = info_hash_hex.clone();
        self.runtime
            .torrent_chans
            .insert(info_hash_hex.clone(), cmd_tx.clone());
        self.runtime.torrent_tasks.insert(info_hash_hex, handle);
        let now = Instant::now();
        self.runtime.tier_last_active.insert(tier_key.clone(), now);
        self.runtime.tier_controller.apply_input(
            tier_key,
            TierInput {
                state: initial_state,
                connected_peers: 0,
                outstanding_requests: 0,
                inbound_peer: false,
                tracker_due: false,
                last_active: Some(now),
                now,
            },
        );
        cmd_tx
    }

    pub(super) fn metadata_task_is_current(
        &self,
        info_hash: &str,
        source: &mpsc::Sender<TorrentCmd>,
    ) -> bool {
        self.runtime
            .torrent_chans
            .get(info_hash)
            .is_some_and(|current| current.same_channel(source))
    }

    pub(super) async fn stop_metadata_task_if_current(
        &mut self,
        info_hash: &str,
        source: &mpsc::Sender<TorrentCmd>,
    ) {
        if self.metadata_task_is_current(info_hash, source) {
            // A metadata task that has queued `CompleteMagnet` remains alive
            // until the engine either publishes the blob or explicitly
            // rejects the completion. Stop the source on terminal failure so
            // the reaper does not have to discover it later and so a future
            // resume can create a fresh metadata worker.
            self.stop_torrent_task(info_hash).await;
        }
    }

    /// Undo the engine-side activation sequence for a metadata placeholder.
    /// The durable state is changed before a task can be created because the
    /// metadata task has no database executor. If task admission or command
    /// delivery then fails, leaving that transition in place strands the row
    /// in `MetadataPending` and can also promote a dormant registry record.
    pub(super) async fn rollback_metadata_placeholder_activation(
        &mut self,
        info_hash: &str,
        previous: TorrentEntry,
        was_dormant: bool,
        previous_dormant_snapshot: Option<DormantTorrentSnapshot>,
        task_was_present: bool,
        error: String,
    ) -> CmdResult<()> {
        let rollback = self
            .update_metadata_placeholder_state_with_event(info_hash, previous.state, None)
            .await;
        if rollback.is_err() {
            // `update_metadata_placeholder_state_with_event` restores the
            // registry to the state it observed at the start of its own
            // attempt. That is the already-failed activation state here, so
            // restore the caller's original projection as a second line of
            // defense even though SQLite may still require operator retry.
            self.restore_registry_lifecycle(info_hash, previous.clone(), was_dormant)
                .await;
        }

        if !task_was_present {
            // A task created solely for this failed activation must not keep
            // its metadata/retry reservations alive after the operation is
            // rejected. `stop_torrent_task` also removes any DHT route that
            // could otherwise target the soon-to-be taskless row.
            if self.runtime.torrent_chans.contains_key(info_hash) {
                self.stop_torrent_task(info_hash).await;
            }

            let now = Instant::now();
            let (state, last_active) = previous_dormant_snapshot
                .as_ref()
                .map(|snapshot| (snapshot.state, snapshot.last_active))
                .unwrap_or((previous.state, None));
            self.runtime.tier_controller.apply_input(
                info_hash.to_owned(),
                TierInput {
                    state,
                    connected_peers: 0,
                    outstanding_requests: 0,
                    inbound_peer: false,
                    tracker_due: false,
                    last_active,
                    now,
                },
            );
            self.runtime.tier_last_active.remove(info_hash);
            if was_dormant {
                let snapshot = previous_dormant_snapshot.unwrap_or_else(|| {
                    dormant_snapshot_from_fields(info_hash, previous.state, None)
                });
                self.runtime
                    .tier_controller
                    .set_dormant_snapshot(info_hash.to_owned(), snapshot);
                // `update_metadata_placeholder_state_with_event` promotes a
                // dormant registry entry through `get_mut`; put it back only
                // after the runtime task is known to be gone.
                if let Err(demote_error) = self.registry.write().await.demote(info_hash) {
                    warn!(
                        component = "tiering",
                        operation = "rollback_metadata_placeholder_demotion",
                        torrent = %info_hash,
                        result = "error",
                        error = %demote_error,
                        "failed to restore dormant metadata placeholder representation"
                    );
                }
            }
        } else if matches!(
            previous.state,
            TorrentState::Paused | TorrentState::Stopped | TorrentState::Queued
        ) && self.runtime.torrent_chans.contains_key(info_hash)
        {
            // The task predated this activation. Restore its paused runtime
            // flag when the failed operation had temporarily resumed it.
            if let Err(pause_error) = self.send_lifecycle_to_torrent(info_hash, true).await {
                warn!(
                    component = "engine",
                    operation = "rollback_metadata_placeholder_task",
                    torrent = %info_hash,
                    result = "error",
                    error = %pause_error,
                    "failed to pause metadata task after activation rollback"
                );
                self.stop_torrent_task(info_hash).await;
            }
        }

        match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(format!(
                "{error}; failed to restore metadata placeholder state: {rollback_error}"
            )),
        }
    }

    pub(super) async fn remove_torrent_inner(
        &mut self,
        info_hash: &str,
        delete_files: bool,
    ) -> CmdResult<Option<String>> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
        let save_path = {
            let registry = self.registry.read().await;
            registry
                .get(info_hash)
                .map(|entry| PathBuf::from(&entry.save_path))
                .ok_or_else(|| format!("torrent {info_hash} not found"))?
        };

        // Build the cleanup plan from the durable torrent-file projection.
        // The file paths are already persisted at add/import time, so delete
        // admission does not reread and parse an arbitrarily large metainfo
        // blob while the engine actor is handling commands. The worker still
        // revalidates every path against the persisted server roots.
        let (payload_plan, v2_only) = if delete_files {
            let info_hash_for_db = info_hash.to_owned();
            let file_rows = self
                .run_db("load_torrent_payload_projection", move |db| {
                    rt_db::list_torrent_files(db, &info_hash_for_db)
                        .map_err(|error| error.to_string())
                })
                .await?;
            let v2_only = info_hash.len() == 64;
            let file_entries = file_entries_from_rows(&file_rows)?;
            if file_entries.is_empty()
                && self
                    .metadata_placeholder_row_checked(info_hash)
                    .await?
                    .is_none()
            {
                return Err(format!(
                    "cannot prepare payload cleanup for torrent {info_hash}: durable file metadata is missing"
                ));
            }
            (
                self.plan_torrent_payload_delete(&save_path, &file_entries)
                    .await?,
                v2_only,
            )
        } else {
            (None, info_hash.len() == 64)
        };

        // A cleanup job may start as soon as it is queued. Quiesce first so
        // the worker cannot delete a file while the torrent task is writing
        // it. If a live task cannot acknowledge quiescence, leave the
        // torrent intact and report the admission failure.
        let quiesced = if payload_plan.is_some() {
            self.quiesce_torrent_for_storage_move(info_hash)
                .await
                .map_err(|error| {
                    format!(
                        "torrent {info_hash} could not be quiesced for payload cleanup: {error}"
                    )
                })?
        } else {
            None
        };

        let payload_delete_job_id = if let Some(plan) = payload_plan.as_ref() {
            match self
                .queue_torrent_payload_delete_job(info_hash, plan, quiesced)
                .await
            {
                Ok(job_id) => {
                    self.runtime
                        .pending_torrent_deletes
                        .insert(info_hash.to_owned());
                    Some(job_id)
                }
                Err(error) => {
                    if let Err(resume_error) = self
                        .resume_torrent_after_storage_move(info_hash, quiesced, None, None)
                        .await
                    {
                        warn!(
                            component = "storage_jobs",
                            operation = "resume_after_delete_submit_failure",
                            torrent = %info_hash,
                            result = "error",
                            error = %resume_error,
                            "failed to restore torrent activity after payload-delete submission failed"
                        );
                    }
                    return Err(error);
                }
            }
        } else {
            None
        };

        if let Some(job_id) = payload_delete_job_id.as_ref() {
            // Keep the durable/session projection until the worker has
            // successfully removed the payload. A failed or cancelled delete
            // must leave an addressable torrent so an operator can retry it;
            // deleting the row before the worker completed made a permission
            // failure permanently orphan the payload.
            self.append_session_event(
                Some(info_hash),
                EVENT_TORRENT_REMOVE_QUEUED,
                Some("torrent removal queued after payload cleanup"),
                serde_json::json!({
                    "delete_files": true,
                    "payload_delete_job_id": job_id,
                    "save_path": save_path,
                }),
            );
            return Ok(payload_delete_job_id);
        }

        self.stop_torrent_task(info_hash).await;
        self.runtime.tier_controller.remove(&info_hash.to_owned());
        self.runtime.tier_last_active.remove(info_hash);

        let removal_event = self.session_event_row(
            Some(info_hash),
            EVENT_TORRENT_REMOVED,
            Some("torrent removed"),
            serde_json::json!({
                "delete_files": delete_files,
                "v2_only": v2_only,
                "payload_delete_job_id": Option::<String>::None,
                "save_path": save_path,
            }),
        );
        if let Err(error) = self
            .delete_persisted_torrent(info_hash, Some(&removal_event))
            .await
        {
            warn!(
                component = "db",
                operation = "delete_torrent",
                torrent = %info_hash,
                result = "error",
                error = %error,
                "failed to delete persisted torrent"
            );
            self.append_session_event(
                Some(info_hash),
                EVENT_TORRENT_REMOVE_FAILED,
                Some("torrent removal could not delete its metadata"),
                serde_json::json!({
                    "delete_files": false,
                    "error": error.to_string(),
                }),
            );
            return Err(error.to_string());
        }
        {
            let mut registry = self.registry.write().await;
            registry
                .remove(info_hash)
                .map_err(|error| error.to_string())?
        };
        Ok(None)
    }

    pub(super) async fn stop_torrent_task(&mut self, info_hash: &str) {
        // Every path that tears down a runtime actor must also remove its DHT
        // registration.  Failure isolation and metadata/storage recovery use
        // this helper without going through the ordinary removal path; if the
        // unregister lived only in those callers, a dead actor sender could
        // remain in DHT until a later peer-forward happened to discover it.
        self.unregister_dht_torrent(info_hash).await;
        if let Some(tx) = self.runtime.torrent_chans.remove(info_hash) {
            let _ = send_torrent_command_until_delivered(&tx, TorrentCmd::Shutdown).await;
        }
        if let Some(task) = self.runtime.torrent_tasks.remove(info_hash) {
            let mut task = ShutdownTaskGuard { task: Some(task) };
            let task = task
                .task
                .as_mut()
                .expect("shutdown task guard must contain the torrent task");
            match timeout(Duration::from_secs(10), &mut *task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    warn!(
                        component = "engine",
                        operation = "remove_torrent_task",
                        torrent = %info_hash,
                        result = "join_error",
                        error = %crate::task_join_error_summary("torrent task", &error),
                        "torrent task ended with a join error during removal"
                    );
                }
                Err(_) => {
                    warn!(
                        component = "engine",
                        operation = "remove_torrent_task",
                        torrent = %info_hash,
                        result = "timeout",
                        "torrent task did not stop during removal; aborting"
                    );
                    task.abort();
                    let _ = crate::reap_aborted_task(
                        task,
                        TASK_ABORT_GRACE,
                        "engine",
                        "remove_torrent_task",
                        "torrent task",
                    )
                    .await;
                }
            }
        }
    }

    pub(super) async fn queue_torrent_payload_delete_job(
        &self,
        info_hash: &str,
        plan: &StoragePlan,
        quiesced: Option<bool>,
    ) -> Result<String, String> {
        let (completion, completion_rx) = oneshot::channel();
        let quiesced_handle = if quiesced.is_some() {
            self.torrent_handle_for(info_hash).await
        } else {
            None
        };
        let quiesced = quiesced
            .map(|was_paused| vec![(info_hash.to_owned(), was_paused)])
            .unwrap_or_default();
        let job_id = self
            .queue_storage_plan_job_with_context(
                STORAGE_JOB_OPERATION_PAYLOAD_DELETE,
                vec![info_hash.to_owned()],
                plan,
                Vec::new(),
                {
                    let mut context = serde_json::json!({
                        "info_hash": info_hash,
                        "save_path_cleanup": true,
                    });
                    context["quiesced"] = storage_quiesced_context(&quiesced);
                    context
                },
                completion,
            )
            .await?;
        let cmd_tx = self.cmd_tx.clone();
        let job_id_for_task = job_id.clone();
        let info_hash_for_task = info_hash.to_owned();
        tokio::spawn(async move {
            let completion = completion_rx.await.unwrap_or_else(|_| {
                StorageJobCompletion::failed_with_manual_recovery(
                    "storage worker completion channel closed",
                    Vec::new(),
                )
            });
            // The delete completion is the only actor-side release of the
            // removal guard and the quiesce. Its producer is bounded by the
            // storage-job dispatcher, so retain this stateful handoff until
            // the actor accepts it or closes.
            send_engine_command_until_actor_stops(
                cmd_tx,
                EngineCmd::StorageDeleteFinished {
                    job_id: job_id_for_task,
                    info_hash: info_hash_for_task,
                    succeeded: completion.succeeded,
                    terminal_state: completion.state,
                    error: completion.error,
                    completed_steps: completion.completed_steps,
                    completed_byte_offset: completion.completed_byte_offset,
                    requires_manual_recovery: completion.requires_manual_recovery,
                    quiesced,
                    quiesced_handle,
                    retry_attempt: 0,
                },
                "storage_delete_completion",
            )
            .await;
        });
        Ok(job_id)
    }

    /// Reap torrent actors that exited without going through an explicit
    /// removal or demotion path. A closed sender alone is not enough here:
    /// the old channel entry made a panicked task look alive, caused active
    /// gauges to lie, and made `ensure_torrent_task` refuse to recreate it.
    /// Marking the durable projection as an error contains the failure while
    /// preserving the normal resume/recheck path as the recovery action.
    pub(super) async fn reap_finished_torrent_tasks(&mut self) {
        let finished = finished_torrent_hashes_limited(&self.runtime.torrent_tasks);
        for info_hash in finished {
            let Some(task) = self.runtime.torrent_tasks.remove(&info_hash) else {
                continue;
            };
            let reason = match task.await {
                Ok(()) => "torrent task exited unexpectedly".to_owned(),
                Err(error) => crate::task_join_error_summary("torrent task", &error),
            };
            self.runtime.torrent_chans.remove(&info_hash);
            // A task can disappear without running its normal removal path.
            // Drop its DHT registration as part of failure isolation, or the
            // DHT task retains a sender to the dead actor until a future
            // lookup happens to discover the closed channel.
            self.unregister_dht_torrent(&info_hash).await;
            self.runtime.tier_controller.remove(&info_hash);
            self.runtime.tier_last_active.remove(&info_hash);

            let previous_entry = {
                let mut registry = self.registry.write().await;
                let previous = if let Some(mut entry) = registry.get_mut(&info_hash) {
                    let previous = entry.clone();
                    entry.set_error(reason.clone());
                    Some(previous)
                } else {
                    None
                };
                previous
            };
            let Some(_previous_entry) = previous_entry else {
                continue;
            };

            let failure_event = self.session_event_row(
                Some(&info_hash),
                "torrent_task_failed",
                Some("torrent runtime task exited and was isolated"),
                serde_json::json!({
                    "state": TorrentState::Error,
                    "error": reason,
                    "runtime_task_removed": true,
                }),
            );
            let retention = self.config.logging.event_retention;
            let info_hash_for_db = info_hash.clone();
            let task_failure_reason = reason.clone();
            let persistence = self
                .run_db("persist_torrent_task_failure", move |db| {
                    let tx = db.transaction().map_err(|error| error.to_string())?;
                    let updated = rt_db::update_state_only_in_tx(
                        &tx,
                        &info_hash_for_db,
                        TorrentState::Error.as_str(),
                    )
                    .map_err(|error| error.to_string())?;
                    if !updated {
                        return Err(format!(
                            "torrent {info_hash_for_db} is missing from the database"
                        ));
                    }
                    rt_db::append_session_event_in_tx(&tx, &failure_event)
                        .map_err(|error| error.to_string())?;
                    rt_db::prune_session_events_in_tx(&tx, retention)
                        .map_err(|error| error.to_string())?;
                    // A recheck actor owns the only live execution path for
                    // its durable job. If that actor exits unexpectedly, the
                    // job cannot make progress or consume a later control
                    // command. Settle every affected active recheck in the
                    // same transaction as the torrent error projection so a
                    // failed task cannot leave an active-job admission lock
                    // behind.
                    let active_jobs =
                        rt_db::list_active_jobs(&tx).map_err(|error| error.to_string())?;
                    let now = unix_now_i64();
                    for mut job in active_jobs.into_iter().filter(|job| {
                        job.kind == JOB_KIND_RECHECK
                            && job
                                .affected_torrents
                                .iter()
                                .any(|hash| hash == &info_hash_for_db)
                    }) {
                        job.state = JOB_STATE_FAILED.to_owned();
                        job.error = Some(task_failure_reason.clone());
                        job.updated_at = now;
                        job.finished_at = Some(now);
                        let event = rt_db::JobEventRow {
                            event_id: None,
                            job_id: job.job_id.clone(),
                            occurred_at: now,
                            kind: "job_failed".to_owned(),
                            message: Some(
                                "recheck failed because its torrent task exited unexpectedly"
                                    .to_owned(),
                            ),
                            payload: serde_json::json!({
                                "state": JOB_STATE_FAILED,
                                "torrent_task_failed": true,
                                "info_hash": &info_hash_for_db,
                                "error": job.error.clone(),
                            })
                            .to_string(),
                        };
                        rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
                        rt_db::append_job_event_in_tx(&tx, &event)
                            .map_err(|error| error.to_string())?;
                    }
                    tx.commit().map_err(|error| error.to_string())
                })
                .await;
            if let Err(error) = persistence {
                warn!(
                    component = "db",
                    operation = "persist_torrent_task_failure",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "failed to persist torrent task failure state"
                );
            }
            // The runtime task is gone regardless of whether the failure
            // projection committed. Keep the in-memory projection truthful
            // and compact, and recreate the dormant tier record so the next
            // resume/recheck request has a coherent promotion path. Restoring
            // the old healthy state here would make the API claim the torrent
            // is still running while no actor exists.
            self.runtime.tier_controller.apply_input(
                info_hash.clone(),
                TierInput {
                    state: TorrentState::Error,
                    connected_peers: 0,
                    outstanding_requests: 0,
                    inbound_peer: false,
                    tracker_due: false,
                    last_active: None,
                    now: Instant::now(),
                },
            );
            self.runtime.tier_controller.set_dormant_snapshot(
                info_hash.clone(),
                dormant_snapshot_from_fields(&info_hash, TorrentState::Error, None),
            );
            if let Err(error) = self.registry.write().await.demote(&info_hash) {
                warn!(
                    component = "tiering",
                    operation = "compact_reaped_torrent",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "failed to compact reaped torrent registry entry"
                );
            }
            warn!(
                component = "engine",
                operation = "reap_torrent_task",
                torrent = %info_hash,
                result = "isolated",
                "torrent task failure was isolated; resume or recheck can recreate it"
            );
        }
    }

    pub(super) async fn shutdown_torrent_tasks(&mut self) {
        let task_count = self.runtime.torrent_chans.len();
        let timeout_secs = self.config.daemon.shutdown_timeout_secs.max(1);
        let timeout_budget = Duration::from_secs(timeout_secs);
        let deadline = tokio::time::Instant::now() + timeout_budget;
        let channels = self.runtime.torrent_chans.values().cloned().collect();
        self.runtime.torrent_chans.clear();
        // Do not let one wedged torrent queue serialize shutdown of every
        // other task, but do not drop a shutdown command just because a
        // healthy task's bounded mailbox is temporarily full either.
        send_torrent_shutdowns_until_deadline(channels, deadline).await;

        let mut timed_out = false;

        let mut tasks = TorrentTaskShutdownGuard {
            tasks: std::mem::take(&mut self.runtime.torrent_tasks)
                .into_iter()
                .collect(),
        };
        let join_budget = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        match timeout(
            join_budget,
            join_torrent_tasks_concurrently(&mut tasks.tasks),
        )
        .await
        {
            Ok(results) => {
                for (index, result) in results {
                    if let Err(error) = result {
                        if !error.is_cancelled() {
                            warn!(
                                component = "engine",
                                operation = "shutdown_torrent_task",
                                torrent = %tasks.tasks[index].0,
                                result = "error",
                                error = %crate::task_join_error_summary("torrent task", &error),
                                "torrent task failed during shutdown"
                            );
                        }
                    }
                }
            }
            Err(_) => {
                timed_out = true;
                let mut timed_out_torrents = Vec::new();
                for (info_hash, task) in &mut tasks.tasks {
                    if !task.is_finished() {
                        task.abort();
                        timed_out_torrents.push(info_hash.clone());
                    }
                }
                let reaped = timeout(
                    TASK_ABORT_GRACE,
                    join_torrent_tasks_concurrently(&mut tasks.tasks),
                )
                .await;
                for info_hash in timed_out_torrents {
                    warn!(
                        component = "engine",
                        operation = "shutdown_torrent_task",
                        torrent = %info_hash,
                        timeout_secs,
                        result = "timeout",
                        "aborted torrent task after shutdown deadline"
                    );
                }
                if reaped.is_err() {
                    warn!(
                        component = "engine",
                        operation = "shutdown_torrent_tasks",
                        timeout_secs,
                        result = "abort_grace_timeout",
                        "some aborted torrent tasks did not finish during abort grace"
                    );
                } else if let Ok(results) = reaped {
                    for (index, result) in results {
                        if let Err(error) = result {
                            if !error.is_cancelled() {
                                warn!(
                                    component = "engine",
                                    operation = "shutdown_torrent_task",
                                    torrent = %tasks.tasks[index].0,
                                    result = "error",
                                    error = %crate::task_join_error_summary("torrent task", &error),
                                    "torrent task failed after shutdown abort"
                                );
                            }
                        }
                    }
                }
            }
        }

        if !timed_out {
            info!(
                component = "engine",
                operation = "shutdown_torrent_tasks",
                tasks = task_count,
                result = "ok",
                "torrent tasks stopped cleanly"
            );
        }
    }

    /// Promote dormant torrents whose persisted tracker deadlines are due.
    /// Dormant torrents are represented by the registry/SQLite/blob and do
    /// not participate in a periodic per-torrent async loop. A seed that has
    /// been idle through the
    /// warm window is demoted and reconstructed on the next lifecycle, peer,
    /// or persisted tracker-deadline demand.
    pub(super) async fn promote_due_tracker_torrents(&mut self, now: Instant) {
        let due = self
            .runtime
            .tier_controller
            .pop_due_tracker_checks_limited(now, TIER_TRACKER_PROMOTION_MAX_PER_TICK);
        if due.is_empty() {
            return;
        }
        let now_unix = unix_now_i64();
        for info_hash in due {
            if self.runtime.torrent_chans.contains_key(&info_hash) {
                continue;
            }
            if let Err(error) = self.ensure_torrent_jobs_idle(&info_hash).await {
                // A storage move/delete/recheck can overlap the persisted
                // deadline after the entry was scheduled. Do not consume the
                // only wake-up in that case: the job completion does not
                // necessarily recreate the dormant tracker deadline.
                self.runtime
                    .tier_controller
                    .schedule_tracker_check(info_hash.clone(), now + TIER_TRACKER_RETRY_DELAY);
                warn!(
                    component = "tiering",
                    operation = "promote_tracker_due",
                    torrent = %info_hash,
                    result = "retry",
                    error = %error,
                    "deferred dormant tracker promotion while torrent storage work is busy"
                );
                continue;
            }
            let state = {
                let registry = self.registry.read().await;
                registry.get(&info_hash).map(|entry| entry.state)
            };
            if state != Some(TorrentState::Seeding) {
                continue;
            }
            let deadline = match self.persisted_tracker_deadline(&info_hash).await {
                Ok(Some(deadline)) => deadline,
                Ok(None) => continue,
                Err(error) => {
                    self.runtime
                        .tier_controller
                        .schedule_tracker_check(info_hash.clone(), now + TIER_TRACKER_RETRY_DELAY);
                    warn!(
                        component = "tiering",
                        operation = "load_tracker_deadline",
                        torrent = %info_hash,
                        result = "error",
                        error = %error,
                        "could not load persisted tracker deadline"
                    );
                    continue;
                }
            };
            if deadline > now_unix {
                if let Some(deadline) = unix_deadline_to_instant(deadline, now_unix, now) {
                    self.runtime
                        .tier_controller
                        .schedule_tracker_check(info_hash, deadline);
                }
                continue;
            }

            match self
                .begin_torrent_task_promotion(&info_hash, TorrentPromotionAction::TrackerReannounce)
                .await
            {
                TorrentPromotionBegin::Ready(action) => {
                    self.execute_torrent_promotion_action(&info_hash, *action, false, false)
                        .await;
                }
                TorrentPromotionBegin::Pending => {
                    info!(
                        component = "tiering",
                        operation = "promote_tracker_due",
                        torrent = %info_hash,
                        result = "queued",
                        "queued dormant torrent promotion for persisted tracker deadline"
                    );
                }
                TorrentPromotionBegin::Rejected => {}
            }
        }
    }

    /// Return the earliest persisted announce deadline for a torrent without
    /// retaining a per-torrent database object in the engine actor.
    pub(super) async fn persisted_tracker_deadline(
        &self,
        info_hash: &str,
    ) -> CmdResult<Option<i64>> {
        let info_hash = info_hash.to_owned();
        self.run_db("load_tracker_deadline", move |db| {
            rt_db::torrent_tracker_deadline(db, &info_hash).map_err(|error| error.to_string())
        })
        .await
    }

    /// Reconcile only promoted torrents whose activity deadline is due.
    /// Dormant torrents are represented by the registry/SQLite/blob and do
    /// not participate in a periodic per-torrent async loop. The old version
    /// walked every promoted actor on every five-second engine tick and
    /// awaited each runtime-stat reply serially; that made tier maintenance
    /// itself an engine-actor outage as the promoted set grew. The controller's
    /// deadline wheel is the admission list for this pass, and the pass has a
    /// hard work budget plus bounded parallel queries so a burst of deadlines
    /// cannot monopolize the actor.
    pub(super) async fn reconcile_activity_tiers(&mut self) {
        if !self.config.runtime.torrent_tiers_enabled {
            return;
        }
        let now = Instant::now();
        let task_ids = self
            .runtime
            .tier_controller
            .pop_due_idle_checks_limited(now, TIER_IDLE_RECONCILE_MAX_PER_TICK);
        if task_ids.is_empty() {
            return;
        }
        let states = {
            let registry = self.registry.read().await;
            task_ids
                .iter()
                .filter_map(|info_hash| {
                    if self.runtime.pending_torrent_deletes.contains(info_hash)
                        || !self.runtime.torrent_chans.contains_key(info_hash)
                    {
                        return None;
                    }
                    registry
                        .get(info_hash)
                        .map(|entry| (info_hash.clone(), entry.state))
                })
                .collect::<HashMap<_, _>>()
        };
        let task_queries = task_ids
            .into_iter()
            .filter_map(|info_hash| {
                let state = states.get(&info_hash).copied()?;
                let tx = self.runtime.torrent_chans.get(&info_hash).cloned()?;
                Some((info_hash, state, tx))
            })
            .collect::<Vec<_>>();
        let runtime_results = stream::iter(task_queries.into_iter().map(
            |(info_hash, state, tx)| async move {
                let runtime = {
                    let (reply, rx) = oneshot::channel();
                    if tx.try_send(TorrentCmd::GetRuntimeStats { reply }).is_err() {
                        Err("torrent task command channel is closed or full".to_owned())
                    } else {
                        match timeout(Duration::from_millis(50), rx).await {
                            Ok(Ok(runtime)) => Ok(runtime),
                            Ok(Err(_)) => {
                                Err("torrent task dropped runtime stats reply".to_owned())
                            }
                            Err(_) => Err("torrent task runtime stats query timed out".to_owned()),
                        }
                    }
                };
                (info_hash, state, runtime)
            },
        ))
        .buffer_unordered(TIER_IDLE_RECONCILE_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
        let mut demote = Vec::new();
        for (info_hash, state, runtime) in runtime_results {
            let runtime = match runtime {
                Ok(runtime) => runtime,
                Err(error) => {
                    // A failed actor probe is not evidence of inactivity.
                    // Treating it as zero peers can demote a live seed while
                    // its actor is merely busy or temporarily unavailable.
                    // Keep the deadline alive and retry after the actor has
                    // had a chance to recover.
                    warn!(
                        component = "tiering",
                        operation = "reconcile_activity",
                        torrent = %info_hash,
                        result = "retry",
                        error = %error,
                        "could not query torrent activity; preserving its current tier"
                    );
                    self.runtime
                        .tier_controller
                        .reschedule_idle_check(info_hash, now + TIER_IDLE_RETRY_DELAY);
                    continue;
                }
            };
            let connected = runtime.connected_peers as usize;
            let outstanding = runtime.outstanding_requests as usize;
            if connected > 0 || outstanding > 0 {
                self.runtime.tier_last_active.insert(info_hash.clone(), now);
            }
            let decision = self.runtime.tier_controller.apply_input(
                info_hash.clone(),
                TierInput {
                    state,
                    connected_peers: connected,
                    outstanding_requests: outstanding,
                    inbound_peer: false,
                    tracker_due: false,
                    last_active: self.runtime.tier_last_active.get(&info_hash).copied(),
                    now,
                },
            );
            if decision.tier == crate::tier::TorrentActivityTier::Dormant
                && state == TorrentState::Seeding
            {
                demote.push(info_hash);
            }
        }
        self.demote_torrent_tasks(demote).await;
    }

    pub(super) async fn demote_torrent_tasks(&mut self, info_hashes: Vec<String>) {
        self.demote_torrent_tasks_with_deadline(info_hashes, TIER_IDLE_DEMOTION_DEADLINE)
            .await;
    }

    pub(super) async fn demote_torrent_tasks_with_deadline(
        &mut self,
        info_hashes: Vec<String>,
        timeout_budget: Duration,
    ) {
        if info_hashes.is_empty() {
            return;
        }

        // Remove every owned handle before the first await. If the engine is
        // cancelled while DHT/command delivery or joining is in progress, the
        // guard still aborts each task instead of detaching it behind a
        // taskless channel entry.
        let mut channels = Vec::with_capacity(info_hashes.len());
        let mut tasks = TorrentTaskShutdownGuard {
            tasks: Vec::with_capacity(info_hashes.len()),
        };
        for info_hash in &info_hashes {
            if let Some(tx) = self.runtime.torrent_chans.remove(info_hash) {
                channels.push(tx);
            }
            if let Some(task) = self.runtime.torrent_tasks.remove(info_hash) {
                tasks.tasks.push((info_hash.clone(), task));
            }
        }

        for info_hash in &info_hashes {
            self.unregister_dht_torrent(info_hash).await;
        }

        let deadline = tokio::time::Instant::now() + timeout_budget;
        // Deliver shutdown to all demoted actors concurrently. A full mailbox
        // on one actor must not delay shutdown delivery to healthy actors.
        send_torrent_shutdowns_until_deadline(channels, deadline).await;

        let join_budget = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        match timeout(
            join_budget,
            join_torrent_tasks_concurrently(&mut tasks.tasks),
        )
        .await
        {
            Ok(results) => {
                for (index, result) in results {
                    if let Err(error) = result {
                        warn!(
                            component = "tiering",
                            operation = "demote_torrent_task",
                            torrent = %tasks.tasks[index].0,
                            result = "join_error",
                            error = %crate::task_join_error_summary("torrent task", &error),
                            "torrent task failed during idle demotion"
                        );
                    }
                }
            }
            Err(_) => {
                let mut timed_out_torrents = Vec::new();
                for (info_hash, task) in &mut tasks.tasks {
                    if !task.is_finished() {
                        task.abort();
                        timed_out_torrents.push(info_hash.clone());
                    }
                }
                let reaped = timeout(
                    TASK_ABORT_GRACE,
                    join_torrent_tasks_concurrently(&mut tasks.tasks),
                )
                .await;
                for info_hash in timed_out_torrents {
                    warn!(
                        component = "tiering",
                        operation = "demote_torrent_task",
                        torrent = %info_hash,
                        deadline_secs = timeout_budget.as_secs(),
                        result = "timeout",
                        "aborted idle torrent task after shared demotion deadline"
                    );
                }
                match reaped {
                    Ok(results) => {
                        for (index, result) in results {
                            if let Err(error) = result {
                                if !error.is_cancelled() {
                                    warn!(
                                        component = "tiering",
                                        operation = "demote_torrent_task",
                                        torrent = %tasks.tasks[index].0,
                                        result = "join_error",
                                        error = %crate::task_join_error_summary("torrent task", &error),
                                        "torrent task failed after idle demotion abort"
                                    );
                                }
                            }
                        }
                    }
                    Err(_) => warn!(
                        component = "tiering",
                        operation = "demote_torrent_tasks",
                        deadline_secs = timeout_budget.as_secs(),
                        result = "abort_grace_timeout",
                        "some idle torrent tasks did not finish during demotion abort grace"
                    ),
                }
            }
        }

        for info_hash in info_hashes {
            self.finish_torrent_demotion(&info_hash).await;
        }
    }

    pub(super) async fn finish_torrent_demotion(&mut self, info_hash: &str) {
        // Keep the dormant key in the controller. Removing it made the
        // controller's tracked set shrink on every demotion, so the runtime
        // could no longer account for all 100k rows even though the registry
        // still retained them. The controller entry is only a compact tier
        // state/timer record; actual torrent removal still calls `remove`.
        let state = self
            .registry
            .read()
            .await
            .get(info_hash)
            .map(|entry| entry.state)
            .unwrap_or(TorrentState::Seeding);
        self.runtime.tier_controller.apply_input(
            info_hash.to_owned(),
            TierInput {
                state,
                connected_peers: 0,
                outstanding_requests: 0,
                inbound_peer: false,
                tracker_due: false,
                last_active: None,
                now: Instant::now(),
            },
        );
        let now = Instant::now();
        let tracker_deadline = match self.persisted_tracker_deadline(info_hash).await {
            Ok(Some(deadline)) => {
                let converted = unix_deadline_to_instant(deadline, unix_now_i64(), now);
                if let Some(deadline) = converted {
                    self.runtime
                        .tier_controller
                        .schedule_tracker_check(info_hash.to_owned(), deadline);
                } else {
                    // A corrupt/extreme persisted timestamp must not silently
                    // remove the only in-memory wake-up for this dormant seed.
                    self.runtime.tier_controller.schedule_tracker_check(
                        info_hash.to_owned(),
                        now + TIER_TRACKER_RETRY_DELAY,
                    );
                    warn!(
                        component = "tiering",
                        operation = "demote_tracker_deadline",
                        torrent = %info_hash,
                        result = "retry",
                        "persisted tracker deadline could not be represented by the runtime clock"
                    );
                }
                converted
            }
            Ok(None) => None,
            Err(error) => {
                // This read is the only source for the dormant tracker's next
                // wake-up. Preserve liveness across a transient DB outage by
                // retrying the read instead of relying on process restart.
                self.runtime
                    .tier_controller
                    .schedule_tracker_check(info_hash.to_owned(), now + TIER_TRACKER_RETRY_DELAY);
                warn!(
                    component = "tiering",
                    operation = "demote_tracker_deadline",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "could not retain persisted tracker deadline while demoting torrent"
                );
                None
            }
        };
        self.runtime.tier_controller.set_dormant_snapshot(
            info_hash.to_owned(),
            dormant_snapshot_from_fields(info_hash, state, tracker_deadline),
        );
        if let Err(error) = self.registry.write().await.demote(info_hash) {
            warn!(
                component = "tiering",
                operation = "demote_registry_entry",
                torrent = %info_hash,
                result = "error",
                error = %error,
                "failed to compact dormant registry entry"
            );
        }
        self.runtime.tier_last_active.remove(info_hash);
        info!(
            component = "tiering",
            operation = "demote",
            torrent = %info_hash,
            result = "ok",
            "demoted idle torrent to dormant representation"
        );
    }

    pub(super) async fn route_incoming_peer(
        &mut self,
        info_hash: String,
        command: TorrentCmd,
    ) -> CmdResult<()> {
        // The peer-wire handshake has only 20 bytes for an infohash. Pure
        // v2 torrents are persisted under their full 32-byte SHA-256
        // infohash, so resolve the truncated wire key through the registry's
        // bounded prefix index before touching storage authority or promotion
        // state. Without this, inbound peers cannot reach pure-v2 tasks and
        // dormant v2 rows cannot be promoted.
        let info_hash = self.resolve_incoming_peer_hash(&info_hash).await?;
        if let Some(peer_addr) = torrent_command_peer_addr(&command) {
            if self.registry.read().await.is_peer_banned(peer_addr) {
                return Ok(());
            }
        }
        self.ensure_torrent_storage_idle(&info_hash).await?;
        let was_taskless = !self.runtime.torrent_chans.contains_key(&info_hash);
        let was_metadata_placeholder = if was_taskless {
            self.metadata_placeholder_row_checked(&info_hash)
                .await?
                .is_some()
        } else {
            false
        };
        let (previous_entry, was_dormant, previous_dormant_snapshot) = if was_metadata_placeholder {
            (
                self.registry.read().await.get(&info_hash),
                self.registry.read().await.is_dormant(&info_hash),
                self.runtime
                    .tier_controller
                    .dormant_snapshot(&info_hash)
                    .cloned(),
            )
        } else {
            (None, false, None)
        };
        if was_taskless {
            if was_metadata_placeholder {
                self.update_metadata_placeholder_state_with_event(
                    &info_hash,
                    TorrentState::MetadataPending,
                    None,
                )
                .await?;
                if let Err(error) = self.ensure_metadata_task(&info_hash).await {
                    return match previous_entry {
                        Some(previous) => {
                            self.rollback_metadata_placeholder_activation(
                                &info_hash,
                                previous,
                                was_dormant,
                                previous_dormant_snapshot,
                                false,
                                error,
                            )
                            .await
                        }
                        None => Err(error),
                    };
                }
            } else {
                match self
                    .begin_torrent_task_promotion(
                        &info_hash,
                        TorrentPromotionAction::IncomingPeer {
                            command: Box::new(command),
                        },
                    )
                    .await
                {
                    TorrentPromotionBegin::Ready(action) => {
                        self.execute_torrent_promotion_action(&info_hash, *action, false, false)
                            .await;
                    }
                    TorrentPromotionBegin::Pending => {}
                    TorrentPromotionBegin::Rejected => {}
                }
                return Ok(());
            }
        }
        let tx = match self.runtime.torrent_chans.get(&info_hash).cloned() {
            Some(tx) => tx,
            None => {
                let error = format!("torrent {info_hash} has no runtime task");
                if was_metadata_placeholder {
                    return match previous_entry {
                        Some(previous) => {
                            self.rollback_metadata_placeholder_activation(
                                &info_hash,
                                previous,
                                was_dormant,
                                previous_dormant_snapshot,
                                false,
                                error,
                            )
                            .await
                        }
                        None => Err(error),
                    };
                }
                return Err(error);
            }
        };
        if was_taskless {
            let result = if was_metadata_placeholder {
                self.send_lifecycle_to_torrent(&info_hash, false).await
            } else {
                self.resume_torrent_runtime(&info_hash).await
            };
            if let Err(error) = result {
                let error = format!("promoted torrent task stopped before resume: {error}");
                if was_metadata_placeholder {
                    return match previous_entry {
                        Some(previous) => {
                            self.rollback_metadata_placeholder_activation(
                                &info_hash,
                                previous,
                                was_dormant,
                                previous_dormant_snapshot,
                                false,
                                error,
                            )
                            .await
                        }
                        None => Err(error),
                    };
                }
                return Err(error);
            }
        }
        if let Err(error) = send_torrent_command(&tx, command).await {
            let error = format!("promoted torrent task stopped before peer delivery: {error}");
            if was_metadata_placeholder {
                return match previous_entry {
                    Some(previous) => {
                        self.rollback_metadata_placeholder_activation(
                            &info_hash,
                            previous,
                            was_dormant,
                            previous_dormant_snapshot,
                            false,
                            error,
                        )
                        .await
                    }
                    None => Err(error),
                };
            }
            return Err(error);
        }

        let now = Instant::now();
        self.runtime.tier_last_active.insert(info_hash.clone(), now);
        let state = self
            .registry
            .read()
            .await
            .get(&info_hash)
            .map(|entry| entry.state)
            .unwrap_or(TorrentState::Downloading);
        self.runtime.tier_controller.apply_event(
            info_hash,
            TierInput {
                state,
                connected_peers: 1,
                outstanding_requests: 0,
                inbound_peer: true,
                tracker_due: false,
                last_active: Some(now),
                now,
            },
            TierEvent::InboundPeer,
        );
        Ok(())
    }

    pub(super) async fn resolve_incoming_peer_hash(&self, presented: &str) -> CmdResult<String> {
        let presented = canonical_info_hash(presented.to_owned());
        let registry = self.registry.read().await;
        let exact_match = self.runtime.torrent_chans.contains_key(&presented)
            || registry.contains_hash(&presented);
        let v2_matches = registry.find_v2_wire_hash_matches(&presented);
        resolve_incoming_peer_hash_candidates(&presented, exact_match, &v2_matches)
    }
}
