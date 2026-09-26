//! Cloneable API command facade. Methods send bounded commands to the engine actor;
//! the actor remains the source of ordering and mutable session state.

use super::*;

impl EngineHandle {
    /// Return whether the engine actor task is still running and owns its
    /// command receiver. The separate liveness flag becomes false when the
    /// actor exits, panics, or is cancelled; a sender-only check would report
    /// healthy until the channel happened to be dropped.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire) && !self.tx.is_closed()
    }

    /// Return whether the engine's TCP peer listener is accepting work. Test
    /// handles without a listener treat this seam as healthy; a running
    /// daemon supplies the shared health flag owned by its actor.
    pub fn peer_listener_healthy(&self) -> bool {
        self.task
            .peer_listener_healthy
            .as_ref()
            .map(|healthy| healthy.load(Ordering::Acquire))
            .unwrap_or(true)
    }

    /// Snapshot counters for inbound peer-handshake admission and failures.
    /// Handles without a running listener expose an all-zero snapshot.
    pub fn peer_ingress_stats(&self) -> PeerIngressStats {
        self.task
            .peer_ingress
            .as_ref()
            .map(|budget| budget.stats())
            .unwrap_or_default()
    }

    /// Snapshot queue, completion, failure, and latency counters for the
    /// dedicated database worker without sending another actor command.
    pub fn database_worker_stats(&self) -> EngineDatabaseWorkerStats {
        self.task.db_worker_metrics.snapshot()
    }

    /// Enqueue a command without allowing a saturated actor mailbox to pin
    /// an API task forever. A bounded send is part of the fault-isolation
    /// contract: a dead or wedged actor must fail the caller, not turn every
    /// subsequent request into an unbounded waiter.
    async fn send_command(&self, command: EngineCmd) -> CmdResult<()> {
        match timeout(ENGINE_COMMAND_SEND_TIMEOUT, self.tx.send(command)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err("engine shut down".to_owned()),
            Err(_) => Err("engine command queue timed out".to_owned()),
        }
    }

    /// Add a torrent; returns the v1 info_hash hex string.
    pub async fn add_torrent(
        &self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
    ) -> CmdResult<String> {
        self.add_torrent_with_labels(meta, save_path, paused, None, Vec::new())
            .await
    }

    pub async fn add_torrent_with_labels(
        &self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        validate_torrent_meta(&meta)?;
        validate_category_value(category.as_deref())?;
        validate_save_path_value(save_path.as_deref())?;
        validate_meta_tracker_urls(&meta)?;
        validate_command_item_len(tags.len(), MAX_ENGINE_MUTATION_ITEMS, "torrent tag list")?;
        validate_label_bytes(&tags, "torrent tag list")?;
        let Some(add_guard) = try_acquire_engine_add_task() else {
            return Err("torrent add preparation capacity exhausted".to_owned());
        };
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::AddTorrent {
            add_guard,
            meta: Box::new(meta),
            save_path,
            paused,
            category,
            tags,
            reply,
        })
        .await?;
        timeout(ENGINE_COMMAND_REPLY_TIMEOUT, rx)
            .await
            .map_err(|_| "engine command timed out".to_owned())?
            .map_err(|_| "engine dropped reply".to_owned())?
    }

    /// Add a torrent from raw metainfo. Parsing and persistence are detached
    /// from the engine actor, which keeps a large bencoded file from blocking
    /// lifecycle, health, or peer-routing commands.
    pub async fn add_torrent_raw_with_labels(
        &self,
        raw: Vec<u8>,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        if raw.len() > MAX_TORRENT_BYTES {
            return Err(format!(
                "torrent metainfo contains {} bytes; maximum is {MAX_TORRENT_BYTES}",
                raw.len()
            ));
        }
        validate_category_value(category.as_deref())?;
        validate_save_path_value(save_path.as_deref())?;
        validate_command_item_len(tags.len(), MAX_ENGINE_MUTATION_ITEMS, "torrent tag list")?;
        validate_label_bytes(&tags, "torrent tag list")?;
        let Some(add_guard) = try_acquire_engine_add_task() else {
            return Err("torrent add preparation capacity exhausted".to_owned());
        };
        let parse_memory_lease =
            reserve_torrent_parse_memory(&self.resources, raw.capacity(), "torrent add command")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::AddTorrentRaw {
            add_guard,
            raw,
            parse_memory_lease,
            save_path,
            paused,
            category,
            tags,
            reply,
        })
        .await?;
        timeout(ENGINE_COMMAND_REPLY_TIMEOUT, rx)
            .await
            .map_err(|_| "engine command timed out".to_owned())?
            .map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn add_magnet_with_labels(
        &self,
        magnet: MagnetLink,
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
        let Some(add_guard) = try_acquire_engine_add_task() else {
            return Err("torrent add preparation capacity exhausted".to_owned());
        };
        let input_memory_lease = reserve_magnet_input_memory(&self.resources, &magnet)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::AddMagnet {
            add_guard,
            magnet,
            input_memory_lease,
            save_path,
            paused,
            category,
            tags,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn remove_torrent(
        &self,
        info_hash: String,
        delete_files: bool,
    ) -> CmdResult<Option<String>> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RemoveTorrent {
            info_hash,
            delete_files,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn pause_torrent(&self, info_hash: String) -> CmdResult<()> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::PauseTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn resume_torrent(&self, info_hash: String) -> CmdResult<()> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ResumeTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn recheck_torrent(&self, info_hash: String) -> CmdResult<()> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RecheckTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn pause_job(&self, job_id: String) -> CmdResult<()> {
        validate_text_bytes(&job_id, MAX_ENGINE_JOB_ID_BYTES, "job id")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::PauseJob { job_id, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn resume_job(&self, job_id: String) -> CmdResult<()> {
        validate_text_bytes(&job_id, MAX_ENGINE_JOB_ID_BYTES, "job id")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ResumeJob { job_id, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn cancel_job(&self, job_id: String) -> CmdResult<()> {
        validate_text_bytes(&job_id, MAX_ENGINE_JOB_ID_BYTES, "job id")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::CancelJob { job_id, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn reannounce_torrent(&self, info_hash: String) -> CmdResult<()> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ReannounceTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_metadata(&self, info_hash: String) -> CmdResult<EngineTorrentMetadata> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentMetadata { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_blob(&self, info_hash: String) -> CmdResult<Vec<u8>> {
        self.torrent_blob_limited(info_hash, MAX_TORRENT_BYTES)
            .await
    }

    pub async fn torrent_blob_size(&self, info_hash: String) -> CmdResult<u64> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentBlobSize { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_blob_limited(
        &self,
        info_hash: String,
        max_bytes: usize,
    ) -> CmdResult<Vec<u8>> {
        if max_bytes > MAX_TORRENT_BYTES {
            return Err(format!(
                "torrent blob read limit is {max_bytes} bytes; maximum is {MAX_TORRENT_BYTES}"
            ));
        }
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentBlob {
            info_hash,
            max_bytes,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_trackers(
        &self,
        info_hash: String,
    ) -> CmdResult<Vec<EngineTrackerSnapshot>> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentTrackers { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_tracker_projection(
        &self,
        info_hash: String,
    ) -> CmdResult<Option<(String, u32)>> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentTrackerProjection { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_tracker_urls(&self, info_hash: String) -> CmdResult<Vec<String>> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentTrackerUrls { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_tracker_snapshot_size(&self, info_hash: String) -> CmdResult<(u64, u64)> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentTrackerSnapshotSize { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn tracker_health(&self) -> CmdResult<Vec<EngineTrackerHealth>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTrackerHealth { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn tracker_health_snapshot_size(&self) -> CmdResult<(u64, u64)> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTrackerHealthSnapshotSize { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_hashes_by_tracker(&self, tracker: String) -> CmdResult<Vec<String>> {
        validate_text_bytes(&tracker, MAX_TRACKER_URL_BYTES, "tracker filter")?;
        let tracker = tracker.trim().to_owned();
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListTorrentHashesByTracker { tracker, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_hashes_by_tracker_snapshot_size(
        &self,
        tracker: String,
    ) -> CmdResult<(u64, u64)> {
        validate_text_bytes(&tracker, MAX_TRACKER_URL_BYTES, "tracker filter")?;
        let tracker = tracker.trim().to_owned();
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentHashesByTrackerSnapshotSize { tracker, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn get_setting(&self, key: String) -> CmdResult<Option<String>> {
        validate_text_bytes(&key, rt_db::MAX_SETTING_KEY_BYTES, "setting key")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetSetting { key, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn set_setting(&self, key: String, value: String) -> CmdResult<()> {
        validate_text_bytes(&key, rt_db::MAX_SETTING_KEY_BYTES, "setting key")?;
        validate_text_bytes(&value, rt_db::MAX_SETTING_VALUE_BYTES, "setting value")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::SetSetting { key, value, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn execute_storage_plan(
        &self,
        operation: String,
        affected_torrents: Vec<String>,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
    ) -> CmdResult<String> {
        validate_storage_plan(&plan)?;
        validate_text_bytes(
            &operation,
            MAX_ENGINE_STORAGE_OPERATION_BYTES,
            "storage operation",
        )?;
        validate_command_item_len(
            affected_torrents.len(),
            MAX_STORAGE_PLAN_AFFECTED_TORRENTS,
            "storage plan torrent list",
        )?;
        validate_command_item_len(
            completed_steps.len(),
            MAX_ENGINE_MUTATION_ITEMS,
            "storage plan checkpoint list",
        )?;
        let affected_torrents = affected_torrents
            .into_iter()
            .map(canonical_info_hash_checked)
            .collect::<CmdResult<Vec<_>>>()?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ExecuteStoragePlan {
            operation,
            affected_torrents,
            plan,
            completed_steps,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn list_jobs(&self) -> CmdResult<Vec<EngineJob>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListJobs { reply }).await?;
        await_engine_reply(rx).await
    }

    pub async fn list_storage_roots(&self) -> CmdResult<Vec<EngineStorageRoot>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListStorageRoots { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn configured_storage_roots(&self) -> CmdResult<Vec<PathBuf>> {
        let roots = self.list_storage_roots().await?;
        ServerStorageRoots::from_configured_paths(
            roots.into_iter().map(|root| root.path).collect::<Vec<_>>(),
        )
        .map(ServerStorageRoots::into_roots)
        .map_err(|error| error.to_string())
    }

    pub async fn stats(&self) -> CmdResult<EngineStats> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetStats { reply }).await?;
        timeout(ENGINE_COMMAND_REPLY_TIMEOUT, rx)
            .await
            .map_err(|_| "engine stats command timed out".to_owned())?
            .map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn subsystem_health(&self) -> CmdResult<EngineSubsystemHealth> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetHealth { reply }).await?;
        timeout(Duration::from_millis(500), rx)
            .await
            .map_err(|_| "engine health command timed out".to_owned())?
            .map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn session_events(
        &self,
        info_hash: Option<String>,
        limit: usize,
    ) -> CmdResult<Vec<rt_db::SessionEventRow>> {
        self.session_events_filtered(info_hash, None, Vec::new(), None, limit)
            .await
    }

    pub async fn session_events_filtered(
        &self,
        info_hash: Option<String>,
        kind: Option<String>,
        levels: Vec<String>,
        last_known_id: Option<i64>,
        limit: usize,
    ) -> CmdResult<Vec<rt_db::SessionEventRow>> {
        validate_command_item_len(
            levels.len(),
            MAX_ENGINE_EVENT_FILTER_ITEMS,
            "event level list",
        )?;
        validate_text_list_bytes(
            &levels,
            MAX_ENGINE_EVENT_FILTER_VALUE_BYTES,
            MAX_ENGINE_EVENT_FILTER_BYTES,
            "event level list",
        )?;
        if let Some(kind) = kind.as_deref() {
            validate_text_bytes(kind, MAX_ENGINE_EVENT_FILTER_VALUE_BYTES, "event kind")?;
        }
        let info_hash = info_hash.map(canonical_info_hash_checked).transpose()?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListSessionEvents {
            info_hash,
            kind,
            levels,
            last_known_id,
            limit,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn reserve_memory(
        &self,
        class: MemoryClass,
        bytes: u64,
    ) -> CmdResult<Option<rt_metrics::MemoryLease>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ReserveMemory {
            class,
            bytes,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn diagnose_torrent(&self, info_hash: String) -> CmdResult<TorrentDiagnostic> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::DiagnoseTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_labels(
        &self,
        info_hash: String,
        category: Option<Option<String>>,
        add_tags: Vec<String>,
        remove_tags: Vec<String>,
    ) -> CmdResult<()> {
        if let Some(Some(category)) = category.as_ref() {
            validate_category_value(Some(category))?;
        }
        validate_command_item_len(
            add_tags.len(),
            MAX_ENGINE_MUTATION_ITEMS,
            "torrent tag list",
        )?;
        validate_command_item_len(
            remove_tags.len(),
            MAX_ENGINE_MUTATION_ITEMS,
            "torrent tag removal list",
        )?;
        validate_label_bytes(&add_tags, "torrent tag list")?;
        validate_label_bytes(&remove_tags, "torrent tag removal list")?;
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentLabels {
            info_hash,
            category,
            add_tags,
            remove_tags,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn list_categories(&self) -> CmdResult<Vec<EngineCategory>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListCategories { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn create_category(&self, name: String, save_path: Option<String>) -> CmdResult<()> {
        validate_text_bytes(&name, MAX_ENGINE_CATEGORY_BYTES, "category name")?;
        if let Some(save_path) = save_path.as_deref() {
            validate_text_bytes(save_path, MAX_ENGINE_SAVE_PATH_BYTES, "category save path")?;
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::CreateCategory {
            name,
            save_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn rename_category(
        &self,
        old_name: String,
        new_name: String,
        save_path: Option<String>,
    ) -> CmdResult<()> {
        validate_text_bytes(&old_name, MAX_ENGINE_CATEGORY_BYTES, "category name")?;
        validate_text_bytes(&new_name, MAX_ENGINE_CATEGORY_BYTES, "new category name")?;
        if let Some(save_path) = save_path.as_deref() {
            validate_text_bytes(save_path, MAX_ENGINE_SAVE_PATH_BYTES, "category save path")?;
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RenameCategory {
            old_name,
            new_name,
            save_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn remove_categories(&self, names: Vec<String>) -> CmdResult<()> {
        validate_command_item_len(names.len(), MAX_ENGINE_MUTATION_ITEMS, "category list")?;
        validate_text_list_bytes(
            &names,
            MAX_ENGINE_CATEGORY_BYTES,
            rt_db::MAX_CATEGORY_DEFINITION_BYTES,
            "category list",
        )?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RemoveCategories { names, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn list_tags(&self) -> CmdResult<Vec<String>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListTags { reply }).await?;
        await_engine_reply(rx).await
    }

    pub async fn create_tags(&self, names: Vec<String>) -> CmdResult<()> {
        validate_command_item_len(names.len(), MAX_ENGINE_MUTATION_ITEMS, "tag list")?;
        validate_label_bytes(&names, "tag list")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::CreateTags { names, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn remove_tags(&self, names: Vec<String>) -> CmdResult<()> {
        validate_command_item_len(names.len(), MAX_ENGINE_MUTATION_ITEMS, "tag list")?;
        validate_label_bytes(&names, "tag list")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RemoveTags { names, reply })
            .await?;
        await_engine_reply(rx).await
    }

    /// Ban peer endpoints in the engine-owned policy set. The set is shared
    /// with every torrent task, so a ban applies to incoming connections that
    /// would otherwise trigger promotion and schedules active-peer eviction.
    pub async fn ban_peers(&self, peers: Vec<SocketAddr>) -> CmdResult<()> {
        validate_peer_command_len(peers.len(), MAX_BANNED_PEERS, "peer ban list")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::BanPeers { peers, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn banned_peers(&self) -> CmdResult<Vec<SocketAddr>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetBannedPeers { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_fields(
        &self,
        info_hash: String,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<()> {
        if let Some(name) = name.as_deref() {
            validate_text_bytes(name, MAX_ENGINE_NAME_BYTES, "torrent name")?;
        }
        validate_save_path_value(save_path.as_deref())?;
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentFields {
            info_hash,
            name,
            save_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_fields_with_job(
        &self,
        info_hash: String,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<Option<String>> {
        if let Some(name) = name.as_deref() {
            validate_text_bytes(name, MAX_ENGINE_NAME_BYTES, "torrent name")?;
        }
        validate_save_path_value(save_path.as_deref())?;
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentFieldsWithJob {
            info_hash,
            name,
            save_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_trackers(
        &self,
        info_hash: String,
        trackers: Vec<String>,
    ) -> CmdResult<()> {
        validate_tracker_urls(&trackers)?;
        let Some(update_guard) = try_acquire_engine_tracker_update_task() else {
            return Err("torrent tracker update capacity exhausted".to_owned());
        };
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentTrackers {
            update_guard,
            info_hash,
            trackers,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_limits(&self, info_hash: String) -> CmdResult<EngineTorrentLimits> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentLimits { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_limits(
        &self,
        info_hash: String,
        limits: EngineTorrentLimits,
    ) -> CmdResult<()> {
        validate_torrent_limits(&limits)?;
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentLimits {
            info_hash,
            limits,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_file_priorities(
        &self,
        info_hash: String,
        file_ids: Vec<u32>,
        priority: i64,
    ) -> CmdResult<()> {
        validate_command_item_len(file_ids.len(), MAX_ENGINE_MUTATION_ITEMS, "file ID list")?;
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateFilePriorities {
            info_hash,
            file_ids,
            priority,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn rename_file_path(
        &self,
        info_hash: String,
        file_id: u32,
        new_path: String,
    ) -> CmdResult<()> {
        validate_text_bytes(&new_path, rt_db::MAX_TORRENT_FILE_PATH_BYTES, "file path")?;
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RenameFilePath {
            info_hash,
            file_id,
            new_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn rename_folder_path(
        &self,
        info_hash: String,
        old_path: String,
        new_path: String,
    ) -> CmdResult<()> {
        validate_text_bytes(&old_path, rt_db::MAX_TORRENT_FILE_PATH_BYTES, "folder path")?;
        validate_text_bytes(
            &new_path,
            rt_db::MAX_TORRENT_FILE_PATH_BYTES,
            "new folder path",
        )?;
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RenameFolderPath {
            info_hash,
            old_path,
            new_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn add_peers(&self, info_hash: String, peers: Vec<SocketAddr>) -> CmdResult<()> {
        validate_peer_command_len(peers.len(), MAX_MANUAL_PEER_ADDRESSES, "manual peer list")?;
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::AddPeers {
            info_hash,
            peers,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_peers(&self, info_hash: String) -> CmdResult<Vec<EnginePeerSnapshot>> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentPeers { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn active_torrent_peers(&self) -> CmdResult<ActiveTorrentPeers> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetActiveTorrentPeers { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_live_stats(
        &self,
        info_hashes: Vec<String>,
    ) -> CmdResult<Vec<TorrentLiveStats>> {
        validate_command_item_len(
            info_hashes.len(),
            MAX_ENGINE_LIVE_STATS,
            "live torrent hash list",
        )?;
        let info_hashes = canonical_info_hash_list(info_hashes)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentLiveStats { info_hashes, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_webseeds(
        &self,
        info_hash: String,
    ) -> CmdResult<Vec<EngineWebseedSnapshot>> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentWebseeds { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn global_limits(&self) -> CmdResult<EngineGlobalLimits> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetGlobalLimits { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_global_limits(&self, limits: EngineGlobalLimits) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateGlobalLimits { limits, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn network_features(&self) -> CmdResult<EngineNetworkFeatures> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetNetworkFeatures { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn listen_port(&self) -> CmdResult<u16> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetListenPort { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_listen_port(&self, port: u16) -> CmdResult<()> {
        if port == 0 {
            return Err("listen_port must be between 1 and 65535".to_owned());
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateListenPort { port, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_network_features(&self, features: EngineNetworkFeatures) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateNetworkFeatures { features, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn set_user_agent(&self, user_agent: String) -> CmdResult<()> {
        validate_text_bytes(&user_agent, 256, "user agent")?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::SetUserAgent { user_agent, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn queue_priority(&self, info_hash: String) -> CmdResult<i32> {
        let info_hash = canonical_info_hash_checked(info_hash)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetQueuePriority { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_queue_order(
        &self,
        info_hashes: Vec<String>,
        queue_move: QueueMove,
    ) -> CmdResult<()> {
        validate_command_item_len(
            info_hashes.len(),
            MAX_ENGINE_MUTATION_ITEMS,
            "queue torrent list",
        )?;
        let info_hashes = canonical_info_hash_list(info_hashes)?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateQueueOrder {
            info_hashes,
            queue_move,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn shutdown(&self) {
        self.task
            .shutdown
            .get_or_init(|| async { self.shutdown_inner().await })
            .await;
    }

    async fn shutdown_inner(&self) {
        let (reply, rx) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + self.task.shutdown_timeout;
        let send_budget = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        match timeout(send_budget, self.tx.send(EngineCmd::Shutdown { reply })).await {
            Ok(Ok(())) => {
                let wait_budget = deadline
                    .checked_duration_since(tokio::time::Instant::now())
                    .unwrap_or_default();
                if timeout(wait_budget, rx).await.is_err() {
                    warn!(
                        component = "engine",
                        operation = "shutdown",
                        result = "timeout",
                        "engine actor did not stop before the shutdown deadline; aborting it"
                    );
                    if let Some(abort) = &self.task.abort {
                        abort.abort();
                    }
                }
            }
            Ok(Err(_)) => {}
            Err(_) => {
                warn!(
                    component = "engine",
                    operation = "shutdown",
                    result = "timeout",
                    "engine command channel did not accept shutdown before the deadline; aborting it"
                );
                if let Some(abort) = &self.task.abort {
                    abort.abort();
                }
            }
        }
        #[cfg(not(test))]
        let mut actor_task = ActorTaskShutdownGuard {
            task: lock_task_handle(&self.task.actor_task).take(),
        };
        #[cfg(not(test))]
        if let Some(actor_task) = actor_task.task.as_mut() {
            let wait_budget = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or_default();
            let _ = crate::shutdown_join_task(
                actor_task,
                wait_budget,
                TASK_ABORT_GRACE,
                "engine",
                "join",
                "engine actor",
                || {},
            )
            .await;
        }
        self.wait_for_peer_listener_shutdown(deadline).await;
        #[cfg(not(test))]
        self.reap_peer_listener_task(deadline).await;
    }

    async fn wait_for_peer_listener_shutdown(&self, deadline: tokio::time::Instant) {
        let Some(done) = self.task.peer_listener_done.as_ref() else {
            return;
        };
        if done.done.load(Ordering::Acquire) {
            return;
        }
        let mut notified = std::pin::pin!(done.notify.notified());
        notified.as_mut().enable();
        if done.done.load(Ordering::Acquire) {
            return;
        }
        let Some(wait_budget) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            if let Some(abort) = &self.task.peer_listener_abort {
                abort.abort();
            }
            return;
        };
        if timeout(wait_budget, notified).await.is_err() {
            warn!(
                component = "peer_listener",
                operation = "shutdown",
                result = "timeout",
                "peer listener did not stop before the engine shutdown deadline; aborting it"
            );
            if let Some(abort) = &self.task.peer_listener_abort {
                abort.abort();
            }
        }
    }

    #[cfg(not(test))]
    async fn reap_peer_listener_task(&self, deadline: tokio::time::Instant) {
        let listener_task = lock_task_handle(&self.task.peer_listener_task).take();
        let mut listener_task = ShutdownTaskGuard {
            task: listener_task,
        };
        let Some(task) = listener_task.task.as_mut() else {
            return;
        };
        let wait_budget = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        let _ = crate::shutdown_join_task(
            task,
            wait_budget,
            TASK_ABORT_GRACE,
            "peer_listener",
            "shutdown",
            "peer listener",
            || {
                if let Some(abort) = &self.task.peer_listener_abort {
                    abort.abort();
                }
            },
        )
        .await;
    }
}
