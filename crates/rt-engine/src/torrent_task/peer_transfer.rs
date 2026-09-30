//! Peer protocol events, block scheduling, piece assembly, and transfer accounting.
//! Torrent state and storage completion remain ordered by the task actor.

use super::*;

impl TorrentTask {
    pub(super) async fn handle_peer_event(&mut self, event: PeerEvent) {
        let (peer, id) = event.identity();
        if !self
            .active_peers
            .get(&peer)
            .is_some_and(|handle| handle.id == id)
        {
            debug!(
                component = "peer",
                operation = "handle_event",
                torrent = %self.info_hash_hex,
                peer = %peer,
                peer_id = ?id,
                result = "ignored",
                reason = "stale_connection",
                "dropping an event from a replaced peer connection"
            );
            return;
        }
        match event {
            PeerEvent::Bitfield {
                peer,
                pieces,
                _memory_lease,
                ..
            } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    reconcile_peer_availability(
                        &mut self.picker.availability,
                        &handle.peer_has,
                        &pieces,
                    );
                    handle.peer_has = pieces;
                }
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::Have { peer, piece, .. } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    if !handle.peer_has.get(piece as usize).unwrap_or(false) {
                        handle.peer_has.set(piece as usize, true);
                        self.picker.availability.add_have(piece as usize);
                    }
                }
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::Unchoked { peer, .. } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.choked = false;
                }
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::Choked {
                peer, outstanding, ..
            } => {
                let outstanding = if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.choked = true;
                    handle.outstanding = 0;
                    let requested = std::mem::take(&mut handle.requested);
                    Self::merge_unique_block_requests(requested, outstanding)
                } else {
                    outstanding
                };
                for req in outstanding {
                    self.picker.cancel_request(req.piece as usize, req.begin);
                }
            }
            PeerEvent::Interested { peer, .. } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.interested = true;
                }
            }
            PeerEvent::NotInterested { peer, .. } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.interested = false;
                }
            }
            PeerEvent::Piece {
                peer,
                block,
                _memory_lease,
                ..
            } => {
                // A peer task can race shutdown and leave one final block in
                // the bounded event channel. Once its handle is gone, that
                // block is stale and must not be written after a recheck or a
                // storage move has switched the torrent's filesystem state.
                let Some(handle) = self.active_peers.get_mut(&peer) else {
                    return;
                };
                handle.outstanding = handle.outstanding.saturating_sub(1);
                remove_requested_block(&mut handle.requested, block.piece, block.offset);
                record_peer_transfer(handle, false, block.data.len() as u64);
                self.handle_block(block).await;
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::RequestRejected { peer, rejected, .. } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.outstanding = handle.outstanding.saturating_sub(1);
                    remove_requested_block(&mut handle.requested, rejected.piece, rejected.begin);
                }
                self.picker
                    .cancel_request(rejected.piece as usize, rejected.begin);
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::Uploaded { peer, bytes, .. } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    record_peer_transfer(handle, true, bytes);
                }
                self.record_upload(bytes).await;
            }
            PeerEvent::Disconnected {
                peer, outstanding, ..
            } => {
                // The peer loop's view can omit commands still queued in its
                // mailbox. Union it with the engine's view so a disconnect
                // cannot strand picker reservations, while deduplicating the
                // same coordinate reported by both sides.
                let outstanding = if let Some(handle) = self.active_peers.get_mut(&peer) {
                    let requested = std::mem::take(&mut handle.requested);
                    Self::merge_unique_block_requests(requested, outstanding)
                } else {
                    outstanding
                };
                if let Some(handle) = self.active_peers.get(&peer) {
                    remove_peer_availability(&mut self.picker.availability, &handle.peer_has);
                }
                for req in outstanding {
                    self.picker.cancel_request(req.piece as usize, req.begin);
                }
                self.active_peers.remove(&peer);
            }
            PeerEvent::RequestTimedOut {
                peer, timed_out, ..
            } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.outstanding = handle.outstanding.saturating_sub(timed_out.len());
                    for req in &timed_out {
                        remove_requested_block(&mut handle.requested, req.piece, req.begin);
                    }
                }
                for req in timed_out {
                    self.picker.cancel_request(req.piece as usize, req.begin);
                }
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::ExtendedHandshake {
                peer,
                ut_metadata_id,
                ut_pex_id,
                metadata_size,
                ..
            } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.ut_metadata_id = ut_metadata_id;
                    handle.ut_pex_id = ut_pex_id;
                    handle.metadata_size = metadata_size;
                }
            }
            PeerEvent::PeerExchange {
                peer,
                peers,
                dropped,
                ..
            } => {
                if self.meta.private {
                    return;
                }
                let peer_count = peers.len();
                self.remember_tracker_peers(&peers);
                // PEX's dropped list is advisory: remove stale retry
                // candidates, but do not forcibly tear down a connection that
                // may still be valid from our side.
                for dropped_peer in dropped {
                    self.known_tracker_peers.remove(&dropped_peer);
                }
                self.connect_peers(peers, PeerSource::PeerExchange).await;
                debug!(
                    torrent = %self.info_hash_hex,
                    peer = %peer,
                    peers = peer_count,
                    "peer exchange discovered peers"
                );
            }
        }
    }

    pub(super) async fn run_choker(&mut self) {
        let snapshots: Vec<PeerSnapshot> = self
            .active_peers
            .values()
            .map(|peer| PeerSnapshot {
                id: peer.id,
                interested: peer.interested,
                upload_rate: peer.upload_rate,
                current_choke: if peer.upload_choked {
                    ChokeState::Choked
                } else {
                    ChokeState::Unchoked
                },
            })
            .collect();

        let decisions = self.choker.run(&snapshots);
        let peers: Vec<SocketAddr> = self.active_peers.keys().copied().collect();
        let mut queue_full = 0u64;
        for addr in peers {
            let Some(handle) = self.active_peers.get_mut(&addr) else {
                continue;
            };
            if let Some(decision) = decisions.get(&handle.id).copied() {
                let (upload_choked, delivery_failed) = Self::try_apply_choke_decision(
                    &handle.cmd_tx,
                    &handle.upload_control,
                    handle.upload_choked,
                    decision,
                );
                handle.upload_choked = upload_choked;
                if delivery_failed {
                    queue_full = queue_full.saturating_add(1);
                }
            }
        }
        self.peer_command_queue_full = self.peer_command_queue_full.saturating_add(queue_full);
    }

    /// Apply the local choke state only after the command has entered the
    /// bounded peer mailbox. A full mailbox is a delivery failure, not a
    /// successful protocol transition; leaving the old state intact lets the
    /// next choker pass retry the command instead of permanently lying about
    /// what the remote peer received.
    pub(super) fn try_apply_choke_decision(
        tx: &mpsc::Sender<PeerCommand>,
        control: &RateLimitCancellation,
        currently_choked: bool,
        decision: ChokeDecision,
    ) -> (bool, bool) {
        let (desired, command) = match decision {
            ChokeDecision::Unchoke if currently_choked => (false, PeerCommand::Unchoke),
            ChokeDecision::Choke if !currently_choked => (true, PeerCommand::Choke),
            _ => return (currently_choked, false),
        };
        match tx.try_send(command) {
            Ok(()) => {
                control.cancel();
                (desired, false)
            }
            Err(_) => (currently_choked, true),
        }
    }

    pub(super) async fn refill_peer_requests(&mut self, peer: SocketAddr) {
        self.refill_download_tokens();
        let mut download_tokens = self.download_tokens;
        let download_limited = self.download_limit_bytes_per_sec.is_some();
        let Some(handle) = self.active_peers.get_mut(&peer) else {
            return;
        };
        if handle.choked {
            return;
        }

        let request_pipeline = memory_aware_request_pipeline(
            self.piece_assembly_bytes,
            self.piece_assembly_soft_cap_bytes,
        );
        if request_pipeline < PEER_REQUEST_PIPELINE_NORMAL {
            self.peer_request_window_reductions =
                self.peer_request_window_reductions.saturating_add(1);
        }

        let mut queue_full = 0u64;
        while handle.outstanding < request_pipeline {
            if download_limited && download_tokens == 0 {
                break;
            }
            let req = match self.picker.pick(&handle.peer_has) {
                Some(req) => req,
                None => {
                    let Some(req) = self
                        .picker
                        .pick_endgame(&handle.peer_has, &handle.requested)
                    else {
                        break;
                    };
                    req
                }
            };
            if download_limited && download_tokens < u64::from(req.length) {
                self.picker.cancel_request(req.piece as usize, req.begin);
                break;
            }
            if handle.cmd_tx.try_send(PeerCommand::Request(req)).is_err() {
                queue_full = queue_full.saturating_add(1);
                self.picker.cancel_request(req.piece as usize, req.begin);
                break;
            }
            if download_limited {
                download_tokens = download_tokens.saturating_sub(u64::from(req.length));
            }
            handle.outstanding += 1;
            handle.requested.push(req);
        }
        if download_limited {
            self.download_tokens = download_tokens;
        }
        self.peer_command_queue_full = self.peer_command_queue_full.saturating_add(queue_full);
    }

    pub(super) async fn handle_block(&mut self, block: BlockEvent) {
        let piece = block.piece;
        if !self.picker.is_piece_in_progress(piece as usize) {
            debug!(
                component = "torrent",
                operation = "receive_block",
                torrent = %self.info_hash_hex,
                piece,
                offset = block.offset,
                result = "ignored",
                reason = "piece_not_in_progress",
                "dropping a late block after the piece request state was released"
            );
            return;
        }
        if self.picker.is_block_received(piece as usize, block.offset) {
            debug!(
                component = "torrent",
                operation = "receive_block",
                torrent = %self.info_hash_hex,
                piece,
                offset = block.offset,
                result = "ignored",
                reason = "duplicate_block",
                "dropping a duplicate block already accepted for this piece"
            );
            return;
        }
        if let Err(error) =
            validate_padding_block(&self.piece_map, piece, block.offset, &block.data)
        {
            warn!(
                component = "torrent",
                operation = "receive_block",
                torrent = %self.info_hash_hex,
                piece,
                offset = block.offset,
                result = "rejected",
                error = %error,
                "peer supplied non-zero or malformed BEP 47 padding data"
            );
            self.picker.cancel_request(piece as usize, block.offset);
            self.remove_piece_assembly(piece);
            return;
        }
        let aggregate_piece_write = self.can_aggregate_piece_write(piece);
        if aggregate_piece_write {
            if let Err(e) = self.record_piece_block(&block) {
                warn!(
                    component = "torrent",
                    operation = "assemble_piece",
                    torrent = %self.info_hash_hex,
                    piece,
                    offset = block.offset,
                    result = "error",
                    error = %e,
                    "failed to assemble in-memory piece for verification"
                );
                self.remove_piece_assembly(piece);
                self.picker.reject_piece(piece as usize);
                return;
            }
        }
        if !aggregate_piece_write {
            if let Err(e) = self.write_block(&block).await {
                warn!(
                    component = "storage",
                    operation = "write_block",
                    torrent = %self.info_hash_hex,
                    piece,
                    offset = block.offset,
                    result = "error",
                    error = %e,
                    "block write failed"
                );
                // `handle_peer_event` removes the block from the peer's
                // request list before handing it here, but the picker still
                // owns the corresponding in-progress reservation. Release
                // that reservation when the disk write fails; otherwise the
                // picker considers the block permanently outstanding even
                // though no peer will deliver it again.
                self.picker.cancel_request(piece as usize, block.offset);
                self.remove_piece_assembly(piece);
                return;
            }
        }
        self.record_download(block.data.len() as u64).await;

        let complete = self
            .picker
            .block_received(block.piece as usize, block.offset);
        if !complete {
            // Persist progress periodically so amount_left stays current even
            // before the first piece verifies. The picker tracks partial
            // piece bytes so progress is visible as blocks arrive. A complete
            // piece is deliberately held out until after verification: the
            // fastresume writer maps picker completion to `Valid` and can
            // flush an in-memory assembly to disk.
            self.persist_progress_throttled(false).await;
            return;
        }

        match self.verify_completed_piece(block.piece).await {
            VerifyResult::Valid => {
                if aggregate_piece_write {
                    if let Err(e) = self.write_completed_piece(block.piece).await {
                        warn!(
                            component = "storage",
                            operation = "write_completed_piece",
                            piece = block.piece,
                            torrent = %self.info_hash_hex,
                            result = "error",
                            error = %e,
                            "completed piece write failed"
                        );
                        self.picker.reject_piece(block.piece as usize);
                        self.remove_piece_assembly(block.piece);
                        return;
                    }
                }
                self.restored_partial_pieces.remove(&block.piece);
                self.remove_piece_assembly(block.piece);
                self.dirty_pieces_since_barrier.insert(block.piece);
                info!(
                    component = "torrent",
                    operation = "complete_piece",
                    torrent = %self.info_hash_hex,
                    piece = block.piece,
                    result = "ok",
                    "piece complete"
                );
                self.send_have_to_peers(block.piece).await;
                if self.picker.is_complete() {
                    self.persist_progress_throttled(true).await;
                    self.save_fastresume(false).await;
                    self.tracker_event = TrackerEvent::Completed;
                    self.schedule_trackers_now();
                    match self.set_state_checked(TorrentState::Seeding).await {
                        Ok(()) => info!(
                            component = "torrent",
                            operation = "complete_download",
                            torrent = %self.info_hash_hex,
                            result = "ok",
                            "download complete"
                        ),
                        Err(error) => {
                            // `set_state_checked` rolled the registry
                            // back to Downloading when its durable write
                            // failed. Keep the runtime on that same
                            // active state; marking only the actor as
                            // paused would leave the public projection
                            // claiming downloading while no task work was
                            // possible.
                            self.paused = false;
                            self.recheck_restore_state = None;
                            self.restart_tracker_session();
                            self.shutdown_peers().await;
                            warn!(
                                component = "torrent",
                                operation = "complete_download",
                                torrent = %self.info_hash_hex,
                                result = "error",
                                error = %error,
                                "failed to persist seeding state; retaining downloading state"
                            );
                        }
                    }
                }
            }
            VerifyResult::Invalid => {
                warn!(
                    piece = block.piece,
                    torrent = %self.info_hash_hex,
                    "piece verification failed"
                );
                self.picker.reject_piece(block.piece as usize);
                self.restored_partial_pieces.remove(&block.piece);
                self.remove_piece_assembly(block.piece);
            }
            VerifyResult::Missing { file_index, reason } => {
                warn!(
                    piece = block.piece,
                    file_index,
                    reason = %reason,
                    torrent = %self.info_hash_hex,
                    "piece verification could not read data"
                );
                self.picker.reject_piece(block.piece as usize);
                self.restored_partial_pieces.remove(&block.piece);
                self.remove_piece_assembly(block.piece);
            }
        }
        // A completed piece is persisted only after the hash outcome is
        // known. If it was rejected, this also records the picker state that
        // must be recovered after a crash instead of leaving a stale
        // completion claim on disk.
        if !self.picker.is_complete() {
            self.persist_progress_throttled(false).await;
        }
    }

    pub(super) fn can_aggregate_piece_write(&self, piece: u32) -> bool {
        self.piece_length(piece)
            .map(|len| {
                len as usize <= self.piece_assembly_soft_cap_bytes
                    && !self.restored_partial_pieces.contains(&piece)
            })
            .unwrap_or(false)
    }

    pub(super) fn record_piece_block(&mut self, block: &BlockEvent) -> anyhow::Result<()> {
        let len = self.piece_length(block.piece)? as usize;
        if len > self.piece_assembly_soft_cap_bytes {
            return Ok(());
        }

        let inserted = if self.piece_assemblies.contains_key(&block.piece) {
            false
        } else {
            let memory_lease = reserve_piece_assembly_bytes(&self.resources, len)?;
            self.piece_assembly_bytes = self.piece_assembly_bytes.saturating_add(len);
            self.piece_assemblies.insert(
                block.piece,
                PieceAssembly::with_memory_lease(len, memory_lease),
            );
            true
        };

        let result = self
            .piece_assemblies
            .get_mut(&block.piece)
            .expect("piece assembly inserted or already present")
            .insert(block.offset, &block.data);
        if result.is_err() && inserted {
            self.remove_piece_assembly(block.piece);
        }
        result?;
        self.enforce_piece_assembly_budget(block.piece);
        Ok(())
    }

    pub(super) fn remove_piece_assembly(&mut self, piece: u32) {
        if let Some(assembly) = self.piece_assemblies.remove(&piece) {
            self.piece_assembly_bytes = self.piece_assembly_bytes.saturating_sub(assembly.len());
        }
    }

    pub(super) fn clear_piece_assemblies(&mut self) {
        let discarded_pieces = self.piece_assemblies.keys().copied().collect::<Vec<_>>();
        self.piece_assemblies.clear();
        self.piece_assembly_bytes = 0;
        // In-memory assemblies contain the only copy of partial blocks when
        // aggregation is enabled. Any clear that is not preceded by a
        // successful fastresume flush must also release the picker's partial
        // reservations, otherwise a later peer can be asked only for the
        // missing suffix while the discarded prefix is no longer available.
        for piece in discarded_pieces {
            self.picker.reject_piece(piece as usize);
        }
    }

    pub(super) fn enforce_piece_assembly_budget(&mut self, current_piece: u32) {
        let evictions = evict_piece_assemblies_to_budget(
            &mut self.piece_assemblies,
            &mut self.piece_assembly_bytes,
            current_piece,
            MAX_IN_MEMORY_PIECE_ASSEMBLIES,
            self.piece_assembly_soft_cap_bytes,
        );
        self.piece_assembly_evictions = self
            .piece_assembly_evictions
            .saturating_add(evictions.len() as u64);
        // The assembly is the only copy of these bytes until a complete piece
        // is written. Dropping it while leaving the picker's received-bit
        // state intact would make the next block recreate an incomplete
        // zero-filled assembly and then fail completion. Reject the affected
        // pieces so their already-received blocks are requested again.
        for piece in evictions {
            self.picker.reject_piece(piece as usize);
        }
    }

    pub(super) async fn send_have_to_peers(&mut self, piece: u32) {
        if self.super_seeding && self.picker.is_complete() {
            return;
        }
        let peers: Vec<SocketAddr> = self.active_peers.keys().copied().collect();
        let mut queue_full = 0u64;
        for peer in peers {
            if let Some(handle) = self.active_peers.get_mut(&peer) {
                if !Self::try_send_have(&handle.cmd_tx, &mut handle.pending_have, piece) {
                    queue_full = queue_full.saturating_add(1);
                }
            }
        }
        self.peer_command_queue_full = self.peer_command_queue_full.saturating_add(queue_full);
    }

    pub(super) fn retry_pending_peer_controls(&mut self) {
        let mut queue_full = 0u64;
        for handle in self.active_peers.values_mut() {
            if let Some(limit) = handle.pending_upload_limit {
                match handle
                    .cmd_tx
                    .try_send(PeerCommand::UpdateUploadLimit(limit))
                {
                    Ok(()) => {
                        handle.pending_upload_limit = None;
                        handle.upload_control.cancel();
                    }
                    Err(_) => queue_full = queue_full.saturating_add(1),
                }
            }

            while let Some(piece) = handle.pending_have.first_set_u32() {
                match handle.cmd_tx.try_send(PeerCommand::Have(piece)) {
                    Ok(()) => handle.pending_have.set(piece as usize, false),
                    Err(_) => {
                        queue_full = queue_full.saturating_add(1);
                        break;
                    }
                }
            }
        }
        self.peer_command_queue_full = self.peer_command_queue_full.saturating_add(queue_full);
    }

    pub(super) fn try_send_have(
        tx: &mpsc::Sender<PeerCommand>,
        pending: &mut PieceBitmap,
        piece: u32,
    ) -> bool {
        match tx.try_send(PeerCommand::Have(piece)) {
            Ok(()) => {
                pending.set(piece as usize, false);
                true
            }
            Err(_) => {
                pending.set(piece as usize, true);
                false
            }
        }
    }

    pub(super) fn merge_unique_block_requests(
        mut requests: Vec<BlockRequest>,
        additional: Vec<BlockRequest>,
    ) -> Vec<BlockRequest> {
        let mut seen = requests
            .iter()
            .map(|request| (request.piece, request.begin))
            .collect::<HashSet<_>>();
        for request in additional {
            if seen.insert((request.piece, request.begin)) {
                requests.push(request);
            }
        }
        requests
    }

    pub(super) fn download_tokens_available(&mut self, bytes: u32) -> bool {
        self.refill_download_tokens();
        if self.download_limit_bytes_per_sec.is_none() {
            return true;
        }
        self.download_tokens >= u64::from(bytes)
    }

    pub(super) fn try_consume_download_tokens(&mut self, bytes: u32) -> bool {
        self.refill_download_tokens();
        let Some(limit) = self.download_limit_bytes_per_sec else {
            return true;
        };
        consume_download_tokens(&mut self.download_tokens, limit, u64::from(bytes))
    }

    pub(super) fn refill_download_tokens(&mut self) {
        let now = Instant::now();
        let Some(limit) = self.download_limit_bytes_per_sec else {
            self.download_tokens = u64::MAX;
            self.download_tokens_updated = now;
            return;
        };
        let elapsed = now.saturating_duration_since(self.download_tokens_updated);
        self.download_tokens_updated = now;
        let refill = (elapsed.as_secs_f64() * limit as f64).floor() as u64;
        self.download_tokens = self
            .download_tokens
            .saturating_add(refill)
            .min(download_bucket_capacity(limit));
    }

    pub(super) async fn shutdown_peers(&mut self) {
        let handles: Vec<(
            mpsc::Sender<PeerCommand>,
            Option<tokio::task::AbortHandle>,
            RateLimitCancellation,
            RateLimitCancellation,
        )> = self
            .active_peers
            .values()
            .map(|peer| {
                (
                    peer.cmd_tx.clone(),
                    peer.abort.clone(),
                    peer.upload_control.clone(),
                    peer.shutdown_control.clone(),
                )
            })
            .collect();

        for peer in self.active_peers.values() {
            remove_peer_availability(&mut self.picker.availability, &peer.peer_has);
        }
        for (tx, abort, upload_control, shutdown_control) in handles {
            upload_control.cancel();
            shutdown_control.cancel();
            let _ = tx.try_send(PeerCommand::Shutdown);
            if let Some(abort) = abort {
                abort.abort();
            }
        }
        self.active_peers.clear();
        self.picker.reset_outstanding_requests();
        self.clear_piece_assemblies();
        while self.peer_event_rx.try_recv().is_ok() {}
        while self.peer_disconnect_rx.try_recv().is_ok() {}
    }

    /// A ban update can race a full torrent command queue. Re-check the
    /// authoritative policy on a timer so an active connection is eventually
    /// evicted even if the best-effort control message was not enqueued.
    pub(super) async fn evict_banned_peers(&mut self) {
        if self.active_peers.is_empty() {
            return;
        }
        let registry = self.registry.read().await;
        let victims = self
            .active_peers
            .keys()
            .copied()
            .filter(|peer| registry.is_peer_banned(*peer))
            .collect::<Vec<_>>();
        drop(registry);
        for peer in victims {
            self.evict_peer(peer);
        }
    }

    pub(super) async fn record_download(&mut self, bytes: u64) {
        self.update_transfer(bytes, false).await;
    }

    pub(super) async fn record_upload(&mut self, bytes: u64) {
        self.last_upload_at = Instant::now();
        self.update_transfer(bytes, true).await;
        self.enforce_seed_limits().await;
    }

    pub(super) async fn update_transfer(&mut self, bytes: u64, upload: bool) {
        let mut reg = self.registry.write().await;
        let Some(mut entry) = reg.get_mut(&self.info_hash_hex) else {
            return;
        };
        if upload {
            entry.stats.add_upload(bytes);
        } else {
            entry.stats.add_download(bytes);
        }
        self.transfer_stats_dirty = true;
    }

    pub(super) async fn enforce_seed_limits(&mut self) {
        if self.paused || !self.picker.is_complete() {
            return;
        }
        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let ratio_reached = self
            .seed_ratio_limit
            .is_some_and(|limit| downloaded > 0 && (uploaded as f64 / downloaded as f64) >= limit);
        let idle_reached = self.seed_idle_limit.is_some_and(|limit| {
            self.seeding_started_at
                .is_some_and(|started| started.elapsed() >= limit)
                && self.last_upload_at.elapsed() >= limit
        });
        if !(ratio_reached || idle_reached) {
            return;
        }
        let previous_recheck_restore_state = self.recheck_restore_state;
        self.paused = true;
        self.recheck_restore_state = Some(TorrentState::Paused);
        self.cancel_tracker_announces();
        self.announce_stopped_with_control_deadline().await;
        self.save_fastresume(false).await;
        self.shutdown_peers().await;
        match self.set_state_checked(TorrentState::Paused).await {
            Ok(()) => {
                self.tracker_event = TrackerEvent::Started;
            }
            Err(error) => {
                self.paused = false;
                self.recheck_restore_state = previous_recheck_restore_state;
                self.restart_tracker_session();
                warn!(
                    component = "torrent",
                    operation = "seed_limit",
                    torrent = %self.info_hash_hex,
                    result = "error",
                    error = %error,
                    "failed to persist seed-limit pause; retaining active state"
                );
                return;
            }
        }
        info!(
            component = "torrent",
            operation = "seed_limit",
            torrent = %self.info_hash_hex,
            ratio_reached,
            idle_reached,
            result = "paused",
            "torrent paused after reaching its seeding limit"
        );
    }

    pub(super) async fn transfer_snapshot(&self) -> (u64, u64) {
        let reg = self.registry.read().await;
        reg.get(&self.info_hash_hex)
            .map(|entry| (entry.stats.uploaded, entry.stats.downloaded))
            .unwrap_or((0, 0))
    }
}
