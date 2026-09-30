//! Accepted TCP/uTP peer sessions and bounded webseed request handling.
//! This module owns transport setup while the torrent task owns session state.

use super::*;

impl TorrentTask {
    pub(super) async fn accept_peer(
        &mut self,
        stream: TcpStream,
        peer_addr: SocketAddr,
        handshake: Handshake,
        peer_permit: OwnedSemaphorePermit,
    ) {
        if handshake.peer_id == crate::peer_id::our_peer_id() {
            debug!(
                component = "peer",
                operation = "accept_incoming",
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                result = "rejected",
                reason = "self_peer_id",
                "rejecting an incoming connection using this client peer id"
            );
            return;
        }
        if self.registry.read().await.is_peer_banned(peer_addr) {
            debug!(
                component = "peer",
                operation = "accept_incoming",
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                result = "rejected",
                reason = "peer_banned",
                "rejecting banned incoming peer"
            );
            return;
        }
        if self.active_peers.len() >= self.peer_capacity()
            || self.active_peers.contains_key(&peer_addr)
        {
            return;
        }
        if !self.peer_source_allowed(peer_addr) {
            debug!(
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                "rejecting inbound peer not returned by private tracker"
            );
            return;
        }
        let info_hash = self.meta.info_hash;
        let Some((peer_id, peer_cmd_rx)) = self.register_peer(peer_addr, peer_permit) else {
            debug!(
                component = "peer",
                operation = "register_incoming",
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                result = "rejected",
                reason = "peer_state_memory_budget",
                "peer state memory budget exhausted"
            );
            return;
        };
        let peer_event_tx = self.peer_event_tx.clone();
        let peer_disconnect_tx = self.peer_disconnect_tx.clone();
        let Some(upload) = self.upload_context(peer_addr) else {
            self.active_peers.remove(&peer_addr);
            debug!(
                component = "peer",
                operation = "allocate_incoming_state",
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                result = "rejected",
                reason = "peer_state_memory_budget",
                "peer state memory budget exhausted"
            );
            return;
        };
        let peer_task = tokio::spawn(async move {
            let (result, outstanding) = match run_incoming_peer(
                stream,
                peer_addr,
                peer_id,
                info_hash,
                peer_event_tx,
                peer_cmd_rx,
                upload,
                handshake.reserved.supports_extension_protocol(),
                handshake.reserved.supports_fast_extension(),
            )
            .await
            {
                Ok(exit) => (exit.result, exit.outstanding),
                Err(error) => (Err(error), Vec::new()),
            };
            if let Err(e) = result {
                debug!(
                    component = "peer",
                    operation = "run_incoming",
                    peer = %peer_addr,
                    result = "ended",
                    error = %e,
                    "incoming peer ended"
                );
            }
            let _ = send_peer_disconnect_event(
                &peer_disconnect_tx,
                PeerEvent::Disconnected {
                    peer: peer_addr,
                    id: peer_id,
                    outstanding,
                },
            )
            .await;
        });
        self.attach_peer_abort(peer_addr, peer_task.abort_handle());
    }

    pub(super) async fn accept_utp_peer(
        &mut self,
        stream: UtpStream,
        peer_addr: SocketAddr,
        handshake: Handshake,
        peer_permit: OwnedSemaphorePermit,
    ) {
        if handshake.peer_id == crate::peer_id::our_peer_id() {
            debug!(
                component = "peer",
                operation = "accept_incoming_utp",
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                result = "rejected",
                reason = "self_peer_id",
                "rejecting an incoming uTP connection using this client peer id"
            );
            return;
        }
        if self.registry.read().await.is_peer_banned(peer_addr) {
            debug!(
                component = "peer",
                operation = "accept_incoming_utp",
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                result = "rejected",
                reason = "peer_banned",
                "rejecting banned incoming uTP peer"
            );
            return;
        }
        if self.active_peers.len() >= self.peer_capacity()
            || self.active_peers.contains_key(&peer_addr)
        {
            return;
        }
        if !self.peer_source_allowed(peer_addr) {
            debug!(
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                "rejecting inbound uTP peer not returned by private tracker"
            );
            return;
        }
        let info_hash = self.meta.info_hash;
        let Some((peer_id, peer_cmd_rx)) = self.register_peer(peer_addr, peer_permit) else {
            debug!(
                component = "peer",
                operation = "register_incoming_utp",
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                result = "rejected",
                reason = "peer_state_memory_budget",
                "peer state memory budget exhausted"
            );
            return;
        };
        let peer_event_tx = self.peer_event_tx.clone();
        let peer_disconnect_tx = self.peer_disconnect_tx.clone();
        let Some(upload) = self.upload_context(peer_addr) else {
            self.active_peers.remove(&peer_addr);
            debug!(
                component = "peer",
                operation = "allocate_incoming_utp_state",
                torrent = %self.info_hash_hex,
                peer = %peer_addr,
                result = "rejected",
                reason = "peer_state_memory_budget",
                "peer state memory budget exhausted"
            );
            return;
        };
        let peer_task = tokio::spawn(async move {
            let (result, outstanding) = match run_incoming_utp_peer(
                stream,
                peer_addr,
                peer_id,
                info_hash,
                peer_event_tx,
                peer_cmd_rx,
                upload,
                handshake.reserved.supports_extension_protocol(),
                handshake.reserved.supports_fast_extension(),
            )
            .await
            {
                Ok(exit) => (exit.result, exit.outstanding),
                Err(error) => (Err(error), Vec::new()),
            };
            if let Err(e) = result {
                debug!(
                    component = "peer",
                    operation = "run_incoming_utp",
                    peer = %peer_addr,
                    result = "ended",
                    error = %e,
                    "incoming uTP peer ended"
                );
            }
            let _ = send_peer_disconnect_event(
                &peer_disconnect_tx,
                PeerEvent::Disconnected {
                    peer: peer_addr,
                    id: peer_id,
                    outstanding,
                },
            )
            .await;
        });
        self.attach_peer_abort(peer_addr, peer_task.abort_handle());
    }

    pub(super) fn upload_context(&self, peer_addr: SocketAddr) -> Option<UploadContext> {
        let peer = self.active_peers.get(&peer_addr)?;
        let upload_control = peer.upload_control.clone();
        let shutdown_control = peer.shutdown_control.clone();
        let piece_count = self.picker.piece_count();
        // Reserve the upload-side dense availability map before constructing
        // it. A torrent with a large piece count must not make every peer
        // connection allocate a bitmap that the governor would reject.
        let bitmap_memory_lease = self.resources.try_acquire(
            MemoryClass::PeerBuffer,
            PieceBitmap::memory_bytes_for_len(piece_count),
        )?;
        let have_pieces = if self.super_seeding && self.picker.is_complete() {
            super_seed_visible_pieces(&self.picker, piece_count, peer_addr)
        } else {
            let mut have_pieces = PieceBitmap::new(piece_count);
            for piece in 0..piece_count {
                if self.picker.have_piece(piece) {
                    have_pieces.set(piece, true);
                }
            }
            have_pieces
        };
        Some(UploadContext {
            save_root: self.save_root.clone(),
            piece_map: self.piece_map.clone(),
            storage: self.storage.clone(),
            resources: self.resources.clone(),
            have_pieces,
            _bitmap_memory_lease: Some(bitmap_memory_lease),
            metadata: self.metadata.clone(),
            is_private: self.meta.private,
            pex_enabled: self.pex_enabled,
            upload_limit_bytes_per_sec: self.upload_limit_bytes_per_sec,
            upload_control,
            shutdown_control,
            global_download: self.network_budget.download(),
            global_upload: self.network_budget.upload(),
        })
    }

    pub(super) fn register_peer(
        &mut self,
        addr: SocketAddr,
        peer_permit: OwnedSemaphorePermit,
    ) -> Option<(PeerId, mpsc::Receiver<PeerCommand>)> {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let id = PeerId::new();
        // Both dense availability maps are retained for the peer lifetime.
        // Acquire their combined budget before allocating either map so a
        // rejected peer never creates a large temporary bitmap.
        let bitmap_bytes =
            PieceBitmap::memory_bytes_for_len(self.meta.pieces.len()).saturating_mul(2);
        let bitmap_memory_lease = self
            .resources
            .try_acquire(MemoryClass::PeerBuffer, bitmap_bytes)?;
        let peer_has = PieceBitmap::new(self.meta.pieces.len());
        let pending_have = PieceBitmap::new(self.meta.pieces.len());
        self.active_peers.insert(
            addr,
            PeerHandle {
                id,
                cmd_tx,
                upload_control: RateLimitCancellation::default(),
                shutdown_control: RateLimitCancellation::default(),
                abort: None,
                peer_has,
                choked: true,
                upload_choked: true,
                interested: false,
                downloaded: 0,
                uploaded: 0,
                download_rate: 0.0,
                upload_rate: 0.0,
                download_rate_window: 0,
                upload_rate_window: 0,
                download_rate_window_started: Instant::now(),
                upload_rate_window_started: Instant::now(),
                outstanding: 0,
                requested: Vec::new(),
                ut_metadata_id: None,
                ut_pex_id: None,
                metadata_size: None,
                _peer_permit: peer_permit,
                _bitmap_memory_lease: bitmap_memory_lease,
                pending_have,
                pending_upload_limit: None,
            },
        );
        Some((id, cmd_rx))
    }

    pub(super) fn attach_peer_abort(&mut self, addr: SocketAddr, abort: tokio::task::AbortHandle) {
        if let Some(peer) = self.active_peers.get_mut(&addr) {
            peer.abort = Some(abort);
        } else {
            // The peer can finish before the engine processes its first
            // event. Do not leave the just-created task alive in that race.
            abort.abort();
        }
    }

    pub(super) fn peer_snapshots_limited(
        &self,
        max_entries: usize,
    ) -> Result<Vec<EnginePeerSnapshot>, String> {
        if max_entries > MAX_PEER_SNAPSHOT_ITEMS {
            return Err(format!(
                "requested peer snapshot limit {max_entries} exceeds {MAX_PEER_SNAPSHOT_ITEMS}"
            ));
        }
        if self.active_peers.len() > max_entries {
            return Err(format!(
                "torrent peer snapshot contains {} peers; maximum is {max_entries}",
                self.active_peers.len()
            ));
        }
        let mut snapshots = Vec::with_capacity(self.active_peers.len());
        self.active_peers
            .iter()
            .map(|(addr, peer)| {
                let pieces = peer.peer_has.count_ones();
                let pieces_total = peer.peer_has.len();
                let progress = if pieces_total == 0 {
                    0.0
                } else {
                    pieces as f64 / pieces_total as f64
                };
                EnginePeerSnapshot {
                    addr: *addr,
                    client: peer_client_label(peer),
                    choked: peer.choked,
                    upload_choked: peer.upload_choked,
                    interested: peer.interested,
                    pieces,
                    pieces_total,
                    progress,
                    download_rate: peer_rate(peer.download_rate, peer.download_rate_window_started),
                    upload_rate: peer_rate(peer.upload_rate, peer.upload_rate_window_started),
                    downloaded: peer.downloaded,
                    uploaded: peer.uploaded,
                }
            })
            .for_each(|snapshot| snapshots.push(snapshot));
        Ok(snapshots)
    }

    pub(super) fn webseed_snapshots(&self) -> Vec<EngineWebseedSnapshot> {
        let now = Instant::now();
        self.meta
            .webseeds
            .iter()
            .enumerate()
            .map(|(idx, url)| {
                let recent = self
                    .webseed_last_success
                    .get(idx)
                    .and_then(|instant| *instant)
                    .is_some_and(|instant| now.duration_since(instant) <= Duration::from_secs(10));
                EngineWebseedSnapshot {
                    url: url.clone(),
                    is_downloading: recent,
                    download_rate: if recent {
                        self.webseed_last_rates.get(idx).copied().unwrap_or(0)
                    } else {
                        0
                    },
                    failures: self.webseed_failures.get(idx).copied().unwrap_or(0),
                }
            })
            .collect()
    }

    pub(super) fn runtime_stats(&self) -> TorrentRuntimeStats {
        let outstanding_requests = self
            .active_peers
            .values()
            .map(|peer| peer.outstanding as u64)
            .sum::<u64>();
        let peer_command_queue_capacity = self
            .active_peers
            .values()
            .map(|peer| peer.cmd_tx.max_capacity() as u64)
            .sum::<u64>();
        let peer_command_queue_depth = self
            .active_peers
            .values()
            .map(|peer| {
                peer.cmd_tx
                    .max_capacity()
                    .saturating_sub(peer.cmd_tx.capacity()) as u64
            })
            .sum::<u64>();
        let peer_command_queue_bytes = self
            .active_peers
            .values()
            .map(|peer| {
                (peer.cmd_tx.max_capacity() as u64)
                    .saturating_mul(std::mem::size_of::<PeerCommand>() as u64)
                    // This gauge also includes the per-peer packed
                    // availability/control maps. They are retained for the
                    // lifetime of the command mailbox and are charged to
                    // the same peer-buffer memory class at registration.
                    .saturating_add(peer.peer_has.memory_bytes())
                    .saturating_add(peer.pending_have.memory_bytes())
                    // The peer task retains its upload-side map for the same
                    // lifetime; it is charged when upload_context is built.
                    .saturating_add(peer.peer_has.memory_bytes())
                    .saturating_add(
                        peer.requested
                            .capacity()
                            .saturating_mul(std::mem::size_of::<BlockRequest>())
                            as u64,
                    )
            })
            .sum::<u64>();
        let tracker_peer_cache_bytes = self
            ._tracker_peer_cache_memory_lease
            .as_ref()
            .map_or(0, MemoryLease::bytes);
        let (download_rate, upload_rate) = self
            .active_peers
            .values()
            .map(|peer| {
                (
                    peer_rate(peer.download_rate, peer.download_rate_window_started),
                    peer_rate(peer.upload_rate, peer.upload_rate_window_started),
                )
            })
            .fold(
                (0_i64, 0_i64),
                |(download, upload), (peer_download, peer_upload)| {
                    (
                        download.saturating_add(peer_download),
                        upload.saturating_add(peer_upload),
                    )
                },
            );
        TorrentRuntimeStats {
            connected_peers: self.active_peers.len() as u64,
            outstanding_requests,
            download_rate,
            upload_rate,
            fastresume_dirty_pieces: self.dirty_pieces_since_barrier.len(),
            completed_piece_verify_from_memory: self.completed_piece_verify_from_memory,
            completed_piece_verify_from_disk: self.completed_piece_verify_from_disk,
            piece_assembly_buffers: self.piece_assemblies.len() as u64,
            piece_assembly_bytes: self.piece_assembly_bytes as u64,
            piece_assembly_evictions: self.piece_assembly_evictions,
            peer_request_window_reductions: self.peer_request_window_reductions,
            peer_rx_buffer_bytes: outstanding_requests.saturating_mul(MAX_BLOCK_SIZE as u64),
            peer_tx_buffer_bytes: 0,
            peer_command_queue_depth,
            peer_command_queue_capacity,
            peer_command_queue_full: self.peer_command_queue_full,
            tracker_peer_cache_entries: self.known_tracker_peers.len() as u64,
            tracker_peer_cache_drops: self.tracker_peer_cache_drops,
            tracker_peer_cache_bytes,
            peer_command_queue_bytes,
            storage: self.storage.stats(),
        }
    }

    pub(super) fn remember_tracker_peers(&mut self, peers: &[SocketAddr]) {
        let cap = tracker_peer_cache_cap(self.max_peers);
        let mut dropped = 0u64;
        for &peer in peers {
            if self.known_tracker_peers.contains(&peer) {
                continue;
            }
            if self._tracker_peer_cache_memory_lease.is_none()
                || self.known_tracker_peers.len() >= cap
            {
                dropped = dropped.saturating_add(1);
                continue;
            }
            // prepare_tracker_peer_cache reserved the full capacity before
            // this task could receive peers, so this insert cannot trigger a
            // new allocation.
            self.known_tracker_peers.insert(peer);
        }
        self.tracker_peer_cache_drops = self.tracker_peer_cache_drops.saturating_add(dropped);
    }

    pub(super) async fn retry_known_tracker_peers(&mut self) {
        if self.picker.is_complete() {
            return;
        }
        if self.active_peers.is_empty() {
            self.schedule_peerless_reannounce();
        }
        if self.active_peers.len() >= self.peer_capacity() || self.known_tracker_peers.is_empty() {
            return;
        }
        info!(
            component = "peer",
            operation = "retry_known_peers",
            torrent = %self.info_hash_hex,
            known_peers = self.known_tracker_peers.len(),
            result = "scheduled",
            "retrying known peers"
        );
        let peers: Vec<SocketAddr> = self.known_tracker_peers.iter().copied().collect();
        self.connect_peers(peers, PeerSource::Tracker).await;
    }

    pub(super) async fn download_next_webseed_block(&mut self) -> Option<BlockEvent> {
        if self.picker.is_complete() {
            return None;
        }
        if self.meta.webseeds.is_empty() {
            debug!(
                component = "webseed",
                operation = "select_block",
                torrent = %self.info_hash_hex,
                reason = "no_webseeds",
                result = "skipped",
                "webseed skipped: no webseeds"
            );
            return None;
        }
        if self.meta.files.len() != 1 {
            debug!(
                component = "webseed",
                operation = "select_block",
                torrent = %self.info_hash_hex,
                files = self.meta.files.len(),
                reason = "multi_file",
                result = "skipped",
                "webseed skipped: multi-file torrent"
            );
            return None;
        }
        if !self.active_peers.is_empty() {
            debug!(
                component = "webseed",
                operation = "select_block",
                torrent = %self.info_hash_hex,
                peers = self.active_peers.len(),
                reason = "active_peers",
                result = "skipped",
                "webseed skipped: active peers available"
            );
            return None;
        }

        let Some(req) = self.picker.pick_from_seed() else {
            self.picker.reset_outstanding_requests();
            debug!(
                component = "webseed",
                operation = "select_block",
                torrent = %self.info_hash_hex,
                reason = "no_requestable_block",
                result = "skipped",
                "webseed skipped: no requestable block"
            );
            return None;
        };
        // Check the local bucket before starting the request, but do not
        // debit it yet. The HTTP operation can fail or be cancelled by a
        // lifecycle command; neither case transferred any payload bytes and
        // must not consume a full protocol block's allowance.
        if !self.download_tokens_available(req.length) {
            self.picker.cancel_request(req.piece as usize, req.begin);
            return None;
        }
        let seed_count = self.meta.webseeds.len();
        for attempt in 0..seed_count {
            let idx = (self.webseed_next_index + attempt) % seed_count;
            if self
                .webseed_next_attempt
                .get(idx)
                .and_then(|next| *next)
                .is_some_and(|next| Instant::now() < next)
            {
                continue;
            }
            if self
                .webseed_failures
                .get(idx)
                .copied()
                .is_some_and(|failures| failures == u8::MAX)
            {
                continue;
            }
            let Some(url) = webseed_block_url(&self.meta, &self.meta.webseeds[idx]) else {
                if let Some(failures) = self.webseed_failures.get_mut(idx) {
                    // An unsupported URL is a permanent local configuration
                    // failure for this task. Do not wake it ten times per
                    // second forever while pretending it is retryable.
                    *failures = u8::MAX;
                }
                debug!(
                    torrent = %self.info_hash_hex,
                    webseed = %url_log_target(&self.meta.webseeds[idx]),
                    "webseed skipped: unsupported url"
                );
                continue;
            };
            debug!(
                torrent = %self.info_hash_hex,
                webseed = %url_log_target(&self.meta.webseeds[idx]),
                url = %url_log_target(url.as_str()),
                piece = req.piece,
                offset = req.begin,
                length = req.length,
                "fetching webseed block"
            );
            let started = Instant::now();
            match self.fetch_webseed_block(&url, req).await {
                Ok(data) => {
                    let elapsed = started.elapsed().as_secs_f64().max(0.001);
                    let rate = (data.len() as f64 / elapsed).round() as i64;
                    // Charge the aggregate budget for bytes actually
                    // received. Failed seeds and short/error responses do
                    // not consume the process-wide download allowance.
                    self.network_budget
                        .download()
                        .acquire(data.len() as u64)
                        .await;
                    // `fetch_webseed_block` validates the exact block length,
                    // and the global budget wait has completed. Commit the
                    // local budget at the final handoff point so a lifecycle
                    // cancellation during either operation charges no bytes
                    // that were not delivered to the torrent task.
                    debug_assert!(self.try_consume_download_tokens(req.length));
                    // Keep the local success projection behind both budget
                    // waits. The outer actor can cancel this operation while
                    // it waits for the aggregate bucket; committing it before
                    // that handoff would rotate/reset seed state for a block
                    // that was never delivered.
                    self.webseed_next_index = (idx + 1) % seed_count;
                    if let Some(failures) = self.webseed_failures.get_mut(idx) {
                        *failures = 0;
                    }
                    if let Some(next_attempt) = self.webseed_next_attempt.get_mut(idx) {
                        *next_attempt = None;
                    }
                    if let Some(last_rate) = self.webseed_last_rates.get_mut(idx) {
                        *last_rate = rate.max(0);
                    }
                    if let Some(last_success) = self.webseed_last_success.get_mut(idx) {
                        *last_success = Some(Instant::now());
                    }
                    return Some(BlockEvent {
                        piece: req.piece,
                        offset: req.begin,
                        data,
                    });
                }
                Err(e) => {
                    let err = webseed_error_for_log(&e);
                    let failures = if let Some(failures) = self.webseed_failures.get_mut(idx) {
                        if err.contains("HTTP 404") || err.contains("HTTP 410") {
                            *failures = (*failures).max(2);
                        } else {
                            *failures = failures.saturating_add(1);
                        }
                        *failures
                    } else {
                        1
                    };
                    if let Some(next_attempt) = self.webseed_next_attempt.get_mut(idx) {
                        *next_attempt = Some(Instant::now() + webseed_retry_delay(failures));
                    }
                    warn!(
                        component = "webseed",
                        operation = "fetch_block",
                        torrent = %self.info_hash_hex,
                        webseed = %url_log_target(&self.meta.webseeds[idx]),
                        piece = req.piece,
                        offset = req.begin,
                        result = "error",
                        error = %err,
                        "webseed block fetch failed"
                    );
                }
            }
        }

        self.picker.cancel_request(req.piece as usize, req.begin);
        None
    }

    pub(super) async fn fetch_webseed_block(
        &self,
        url: &Url,
        req: BlockRequest,
    ) -> anyhow::Result<bytes::Bytes> {
        let user_agent = crate::peer_id::user_agent();
        let client = self
            .egress_policy
            .http_client(
                OutboundTargetKind::Webseed,
                url,
                self.http_timeout,
                &user_agent,
            )
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        let _lease = reserve_webseed_body_bytes(&self.resources, req.length)?;
        let (start, end) =
            webseed_byte_range(req.piece, self.meta.piece_length, req.begin, req.length)?;
        let response = client
            .get(url.clone())
            .header(RANGE, format!("bytes={start}-{end}"))
            .send()
            .await
            .map_err(|error| anyhow::anyhow!(error.without_url()))?;
        validate_webseed_range_response(response.status(), response.headers(), start, end)?;
        let bytes = bounded_response_body(response, req.length as usize)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        if bytes.len() != req.length as usize {
            anyhow::bail!(
                "expected {} bytes, received {} bytes",
                req.length,
                bytes.len()
            );
        }
        Ok(bytes.into())
    }

    pub(super) fn schedule_peerless_reannounce(&mut self) {
        let now = Instant::now();
        if self
            .last_peerless_reannounce
            .is_some_and(|last| now.duration_since(last) < Duration::from_secs(120))
        {
            return;
        }
        self.last_peerless_reannounce = Some(now);
        self.tracker_event = TrackerEvent::Empty;
        self.schedule_active_tracker_tier_now();
        info!(
            torrent = %self.info_hash_hex,
            "scheduled tracker reannounce after losing all peers"
        );
    }

    pub(super) fn peer_source_allowed(&self, peer: SocketAddr) -> bool {
        private_peer_source_allowed(self.meta.private, &self.known_tracker_peers, peer)
    }
}
