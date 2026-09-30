//! Peer source selection, connection admission, and replacement policy.
//! State changes remain owned by the per-torrent task.

use super::*;

impl TorrentTask {
    pub(super) async fn connect_peers(&mut self, addrs: Vec<SocketAddr>, source: PeerSource) {
        for addr in addrs {
            if self.active_peers.len() >= self.peer_capacity() {
                break;
            }
            if let Err(error) = self.egress_policy.validate_peer_addr(addr) {
                debug!(
                    component = "peer",
                    operation = "connect_outgoing",
                    torrent = %self.info_hash_hex,
                    peer = %addr,
                    result = "rejected",
                    reason = "egress_address_policy",
                    error = %error,
                    "skipping outgoing peer denied by address policy"
                );
                continue;
            }
            if self.registry.read().await.is_peer_banned(addr) {
                debug!(
                    component = "peer",
                    operation = "connect_outgoing",
                    torrent = %self.info_hash_hex,
                    peer = %addr,
                    result = "rejected",
                    reason = "peer_banned",
                    "skipping banned outgoing peer"
                );
                continue;
            }
            if !self.peer_source_allowed(addr) {
                debug!(
                    torrent = %self.info_hash_hex,
                    peer = %addr,
                    "skipping peer not returned by private tracker"
                );
                continue;
            }
            if self.active_peers.contains_key(&addr) {
                continue;
            }
            let Ok(peer_permit) = self.network_budget.try_acquire_peer() else {
                debug!(
                    torrent = %self.info_hash_hex,
                    peer = %addr,
                    "global peer connection budget exhausted"
                );
                break;
            };
            let info_hash = self.meta.info_hash;
            let Some((peer_id, peer_cmd_rx)) = self.register_peer(addr, peer_permit) else {
                debug!(
                    component = "peer",
                    operation = "register_outgoing",
                    torrent = %self.info_hash_hex,
                    peer = %addr,
                    result = "rejected",
                    reason = "peer_state_memory_budget",
                    "peer state memory budget exhausted"
                );
                break;
            };
            let peer_event_tx = self.peer_event_tx.clone();
            let peer_disconnect_tx = self.peer_disconnect_tx.clone();
            let Some(upload) = self.upload_context(addr) else {
                self.active_peers.remove(&addr);
                debug!(
                    component = "peer",
                    operation = "allocate_outgoing_state",
                    torrent = %self.info_hash_hex,
                    peer = %addr,
                    result = "rejected",
                    reason = "peer_state_memory_budget",
                    "peer state memory budget exhausted"
                );
                break;
            };
            let transport_policy = outgoing_transport_policy_for_peer(
                outgoing_transport_policy_configured(),
                source,
                self.meta.private,
            );
            let peer_task = tokio::spawn(async move {
                let (result, outstanding) = match run_outgoing_peer_with_policy(
                    addr,
                    peer_id,
                    info_hash,
                    peer_event_tx,
                    peer_cmd_rx,
                    upload,
                    transport_policy,
                )
                .await
                {
                    Ok(exit) => (exit.result, exit.outstanding),
                    Err(error) => (Err(error), Vec::new()),
                };
                if let Err(e) = result {
                    debug!(
                        component = "peer",
                        operation = "run_outgoing",
                        peer = %addr,
                        result = "ended",
                        error = %e,
                        "peer ended"
                    );
                }
                let _ = send_peer_disconnect_event(
                    &peer_disconnect_tx,
                    PeerEvent::Disconnected {
                        peer: addr,
                        id: peer_id,
                        outstanding,
                    },
                )
                .await;
            });
            self.attach_peer_abort(addr, peer_task.abort_handle());
        }
    }

    pub(super) async fn connect_priority_peers(&mut self, addrs: Vec<SocketAddr>) {
        let mut allowed_addrs = Vec::with_capacity(addrs.len());
        for addr in addrs {
            if let Err(error) = self.egress_policy.validate_peer_addr(addr) {
                debug!(
                    component = "peer",
                    operation = "connect_priority",
                    torrent = %self.info_hash_hex,
                    peer = %addr,
                    result = "rejected",
                    reason = "egress_address_policy",
                    error = %error,
                    "skipping priority peer denied by address policy"
                );
                continue;
            }
            allowed_addrs.push(addr);
        }
        let preferred: HashSet<SocketAddr> = allowed_addrs.iter().copied().collect();
        for addr in allowed_addrs {
            if self.active_peers.contains_key(&addr) {
                continue;
            }
            if self.active_peers.len() >= self.peer_capacity() {
                self.drop_replaceable_peer(&preferred).await;
            }
            if self.active_peers.len() >= self.peer_capacity() {
                break;
            }
            self.connect_peers(vec![addr], PeerSource::Manual).await;
        }
    }

    pub(super) async fn drop_replaceable_peer(&mut self, preferred: &HashSet<SocketAddr>) {
        let victim = self
            .active_peers
            .iter()
            .find(|(addr, peer)| !preferred.contains(addr) && peer.choked && peer.outstanding == 0)
            .map(|(addr, _)| *addr)
            .or_else(|| {
                self.active_peers
                    .iter()
                    .find(|(addr, peer)| !preferred.contains(addr) && peer.outstanding == 0)
                    .map(|(addr, _)| *addr)
            })
            .or_else(|| {
                self.active_peers
                    .keys()
                    .find(|addr| !preferred.contains(addr))
                    .copied()
            });

        let Some(victim) = victim else {
            return;
        };
        if self.evict_peer(victim) {
            debug!(
                torrent = %self.info_hash_hex,
                peer = %victim,
                "dropped peer to connect priority peer"
            );
        }
    }

    /// Remove a live peer and all scheduler state associated with it. This is
    /// synchronous because callers already own the torrent actor; the peer
    /// task receives a best-effort shutdown command and its permit is released
    /// when the handle is dropped.
    pub(super) fn evict_peer(&mut self, peer: SocketAddr) -> bool {
        let Some(handle) = self.active_peers.remove(&peer) else {
            return false;
        };
        remove_peer_availability(&mut self.picker.availability, &handle.peer_has);
        for req in handle.requested {
            self.picker.cancel_request(req.piece as usize, req.begin);
        }
        handle.upload_control.cancel();
        handle.shutdown_control.cancel();
        let _ = handle.cmd_tx.try_send(PeerCommand::Shutdown);
        if let Some(abort) = handle.abort {
            abort.abort();
        }
        true
    }
}
