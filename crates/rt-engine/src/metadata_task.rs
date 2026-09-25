use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::mem::size_of;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use futures::{stream::FuturesUnordered, SinkExt, StreamExt};
use rt_bencode::decode_with_allocation_reservation;
use rt_hash::{hash_pair, merkle_root, V2_BLOCK_SIZE};
use rt_metainfo::{
    v2_piece_layer_requirements_with_allocation_reservation, V2PieceLayerRequirement,
};
use rt_metrics::{MemoryClass, MemoryLease, ResourceGovernor};
use rt_peer_wire::{
    codec::PeerCodec,
    extension::{ExtensionHandshake, UtMetadataMessage, EXT_HANDSHAKE_ID},
    handshake::{ExtensionFlags, Handshake},
    message::Message,
};
use rt_session::SessionRegistry;
use rt_tracker::{
    udp::{UdpAnnounceRequest, UdpAnnounceResponse, UdpConnectRequest, UdpConnectResponse},
    AnnounceRequest, AnnounceResponse, InfoHash, TrackerError, TrackerEvent, MAX_TRACKER_PEERS,
};
use rt_utp::UtpStream;
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::{Digest as Sha2Digest, Sha256};
use tokio::net::TcpStream;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, OwnedSemaphorePermit, RwLock};
use tokio::time::{interval, timeout, Interval};
use tokio_util::codec::Framed;
use tracing::{debug, warn};
use url::Url;

use crate::command::EngineCmd;
use crate::egress_policy::{OutboundEgressPolicy, OutboundTargetKind};
use crate::network_budget::GlobalNetworkBudget;
use crate::torrent_task::{TorrentCmd, UtpFrameDecoder, UtpWireBuffer};
use crate::tracker_runtime::{is_udp_tracker_url, protocol_numwant, url_log_target};

const METADATA_PIECE_SIZE: usize = 16 * 1024;
const MAX_METADATA_SIZE: u32 = 16 * 1024 * 1024;
const LOCAL_UT_METADATA_ID: u8 = 1;
const MAX_METADATA_FETCH_CONCURRENCY: usize = 8;
const METADATA_PEER_RETRY_AFTER: Duration = Duration::from_secs(15);
const METADATA_PEER_ATTEMPT_CACHE_MIN: usize = 256;
const METADATA_PEER_ATTEMPT_CACHE_MULTIPLIER: usize = 4;
// A std HashMap bucket includes the key/value, hash-table control bytes, and
// allocator slack. Reserve a deliberately conservative per-slot estimate so
// the retry history cannot bypass the metadata governor even though it grows
// lazily while peers arrive.
const METADATA_PEER_ATTEMPT_SLOT_BYTES: usize = 256;
const METADATA_TASK_BASE_MEMORY_BYTES: usize = 8 * 1024;
const MAX_METADATA_DIRECT_PEERS: usize = 256;
// A tracker announce response is parsed into `Peer` values, then its unique
// socket addresses are retained in both the result vector and a dedup set.
// Reserve one conservative aggregate allowance before either collection can
// grow outside the tracker-peers governor.
const METADATA_TRACKER_PEER_COLLECTION_BYTES_PER_ENTRY: u64 = 256;
const METADATA_PEER_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const METADATA_PEER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const METADATA_PEER_FETCH_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const METADATA_TRACKER_ANNOUNCE_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct FetchedMetadata {
    bytes: Vec<u8>,
    piece_layers: Vec<([u8; 32], Vec<[u8; 32]>)>,
    _lease: MemoryLease,
}

/// The identity used while a magnet is still waiting for its info
/// dictionary. The peer wire and tracker protocols use the first 20 bytes of
/// a v2 SHA-256 infohash, while completion must compare the full digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataInfoHash {
    V1([u8; 20]),
    V2([u8; 32]),
    Hybrid { v1: [u8; 20], v2: [u8; 32] },
}

impl MetadataInfoHash {
    pub(crate) fn wire_hash(self) -> [u8; 20] {
        match self {
            Self::V1(hash) => hash,
            Self::V2(hash) => hash[..20].try_into().expect("v2 hash has 20-byte prefix"),
            Self::Hybrid { v1, .. } => v1,
        }
    }

    fn is_v2(self) -> bool {
        matches!(self, Self::V2(_))
    }
}

/// Parse the durable magnet identity accepted by the engine. A v2 metadata
/// placeholder is keyed by the full 64-character SHA-256 digest, even though
/// its initial peer/tracker wire identity is truncated to 20 bytes.
pub(crate) fn parse_metadata_info_hash_hex(value: &str) -> Result<MetadataInfoHash, ()> {
    match value.len() {
        40 => {
            let bytes = hex::decode(value).map_err(|_| ())?;
            Ok(MetadataInfoHash::V1(bytes.try_into().map_err(|_| ())?))
        }
        64 => {
            let bytes = hex::decode(value).map_err(|_| ())?;
            Ok(MetadataInfoHash::V2(bytes.try_into().map_err(|_| ())?))
        }
        _ => Err(()),
    }
}

/// Restore the full expected identity for a metadata-pending magnet. Hybrid
/// magnets use the v1 hash as their durable row key but must retain and verify
/// the v2 digest supplied by the same magnet.
pub(crate) fn metadata_info_hash_with_v2(
    value: &str,
    expected_v2_hash: Option<[u8; 32]>,
) -> Result<MetadataInfoHash, ()> {
    match (parse_metadata_info_hash_hex(value)?, expected_v2_hash) {
        (MetadataInfoHash::V1(v1), Some(v2)) => Ok(MetadataInfoHash::Hybrid { v1, v2 }),
        (MetadataInfoHash::V2(_), Some(_)) => Err(()),
        (MetadataInfoHash::Hybrid { .. }, _) => Err(()),
        (identity, None) => Ok(identity),
    }
}

/// Memory retained by one metadata-pending torrent for its tracker URLs,
/// bounded peer retry history, and the bounded tracker prefix copied into the
/// raw metainfo during completion. The lease is kept with the task for its
/// whole lifetime; metadata fetch response leases are separate, short-lived
/// leases.
#[derive(Debug)]
pub(crate) struct MetadataTaskMemory {
    leases: Vec<MemoryLease>,
    reserved_bytes: u64,
    direct_peers: Vec<SocketAddr>,
}

/// Compact caller-owned tracker vectors before admission. API callers and
/// restored database rows can carry excess Vec/String capacity even when the
/// logical URL content is within the protocol limits.
pub(crate) fn compact_metadata_task_trackers(mut trackers: Vec<String>) -> Vec<String> {
    for tracker in &mut trackers {
        tracker.shrink_to_fit();
    }
    trackers.shrink_to_fit();
    trackers
}

fn compact_metadata_task_peers(mut peers: Vec<SocketAddr>) -> Vec<SocketAddr> {
    peers.sort_unstable();
    peers.dedup();
    peers.shrink_to_fit();
    peers
}

fn metadata_task_memory_bytes(
    trackers: &[String],
    tracker_capacity: usize,
    direct_peer_capacity: usize,
    max_peers: usize,
) -> usize {
    let tracker_vec_bytes = tracker_capacity.saturating_mul(size_of::<String>());
    let tracker_string_bytes = trackers
        .iter()
        .map(String::capacity)
        .fold(0usize, usize::saturating_add);
    let peer_attempt_bytes =
        metadata_peer_attempt_cache_cap(max_peers).saturating_mul(METADATA_PEER_ATTEMPT_SLOT_BYTES);
    let completion_tracker_bytes = metadata_completion_tracker_bytes(trackers);
    // Keep room for the retained x.pe vector and the bounded working copy
    // consumed by the first/retry fetch. SocketAddr is inline, so capacity is
    // the only allocation component.
    let direct_peer_bytes = direct_peer_capacity
        .saturating_mul(size_of::<SocketAddr>())
        .saturating_mul(2);
    METADATA_TASK_BASE_MEMORY_BYTES
        .saturating_add(tracker_vec_bytes)
        .saturating_add(tracker_string_bytes)
        .saturating_add(peer_attempt_bytes)
        .saturating_add(completion_tracker_bytes)
        .saturating_add(direct_peer_bytes)
}

/// Admit the retained state before a metadata task is published into the
/// engine runtime maps. Failure leaves the caller free to reject a new magnet
/// or keep a restored placeholder dormant without silently losing its tracker
/// override.
pub(crate) fn prepare_metadata_task_memory(
    resources: &ResourceGovernor,
    trackers: Vec<String>,
    max_peers: usize,
) -> Result<(Vec<String>, MetadataTaskMemory), String> {
    prepare_metadata_task_memory_with_peers(resources, trackers, Vec::new(), max_peers)
}

/// Admit metadata task state, including bounded BEP 9 `x.pe` direct peers.
/// Direct peers are hints only; tracker and DHT discovery remain independent
/// fallback paths.
pub(crate) fn prepare_metadata_task_memory_with_peers(
    resources: &ResourceGovernor,
    trackers: Vec<String>,
    direct_peers: Vec<SocketAddr>,
    max_peers: usize,
) -> Result<(Vec<String>, MetadataTaskMemory), String> {
    let trackers = compact_metadata_task_trackers(trackers);
    let direct_peers = compact_metadata_task_peers(direct_peers);
    if direct_peers.len() > MAX_METADATA_DIRECT_PEERS {
        return Err(format!(
            "metadata task has {} direct peers, maximum is {MAX_METADATA_DIRECT_PEERS}",
            direct_peers.len()
        ));
    }
    let bytes = u64::try_from(metadata_task_memory_bytes(
        &trackers,
        trackers.capacity(),
        direct_peers.capacity(),
        max_peers,
    ))
    .map_err(|_| "metadata task memory estimate does not fit in u64".to_owned())?;
    let lease = resources
        .try_acquire(MemoryClass::Metadata, bytes)
        .ok_or_else(|| {
            format!(
                "metadata task allocation of {bytes} bytes denied for {} trackers",
                trackers.len()
            )
        })?;
    Ok((
        trackers,
        MetadataTaskMemory {
            leases: vec![lease],
            reserved_bytes: bytes,
            direct_peers,
        },
    ))
}

impl MetadataTaskMemory {
    fn refresh_trackers(
        &mut self,
        resources: &ResourceGovernor,
        trackers: &[String],
        tracker_capacity: usize,
        max_peers: usize,
    ) -> Result<(), String> {
        let required = u64::try_from(metadata_task_memory_bytes(
            trackers,
            tracker_capacity,
            self.direct_peers.capacity(),
            max_peers,
        ))
        .map_err(|_| "metadata task memory estimate does not fit in u64".to_owned())?;
        if required <= self.reserved_bytes {
            return Ok(());
        }
        let additional = required.saturating_sub(self.reserved_bytes);
        let lease = resources
            .try_acquire(MemoryClass::Metadata, additional)
            .ok_or_else(|| {
                format!(
                    "metadata task tracker update allocation of {additional} bytes denied for {} trackers",
                    trackers.len()
                )
            })?;
        self.leases.push(lease);
        self.reserved_bytes = required;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataTransportPolicy {
    TcpOnly,
    PreferUtp,
    UtpOnly,
}

fn metadata_outgoing_transport_policy() -> MetadataTransportPolicy {
    if let Ok(value) = std::env::var("TNG_UTP_METADATA") {
        return parse_metadata_transport_policy(&value);
    }
    if let Ok(value) = std::env::var("TNG_UTP_OUTGOING") {
        return parse_metadata_transport_policy(&value);
    }
    if std::env::var_os("TNG_ENABLE_UTP_OUTGOING").is_some() {
        return MetadataTransportPolicy::PreferUtp;
    }
    MetadataTransportPolicy::TcpOnly
}

fn parse_metadata_transport_policy(value: &str) -> MetadataTransportPolicy {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "prefer" | "prefer-utp" | "utp-prefer" => {
            MetadataTransportPolicy::PreferUtp
        }
        "only" | "utp" | "utp-only" => MetadataTransportPolicy::UtpOnly,
        _ => MetadataTransportPolicy::TcpOnly,
    }
}

fn current_listen_port(network_budget: &GlobalNetworkBudget, fallback: u16) -> u16 {
    match network_budget.listen_port() {
        0 => fallback,
        port => port,
    }
}

// Metadata acquisition currently has separate transport, persistence, and
// admission dependencies. Keep those boundaries explicit until the task
// context object is introduced as part of the engine seam refactor.
#[allow(clippy::too_many_arguments)]
pub async fn run_metadata_task(
    info_hash: MetadataInfoHash,
    info_hash_hex: String,
    mut trackers: Vec<String>,
    mut task_memory: MetadataTaskMemory,
    mut cmd_rx: mpsc::Receiver<TorrentCmd>,
    engine_tx: mpsc::Sender<EngineCmd>,
    task_tx: mpsc::Sender<TorrentCmd>,
    registry: Arc<RwLock<SessionRegistry>>,
    resources: ResourceGovernor,
    listen_port: u16,
    max_peers: usize,
    http_timeout_secs: u64,
    udp_timeout_secs: u64,
    paused: bool,
    egress_policy: OutboundEgressPolicy,
    network_budget: GlobalNetworkBudget,
) {
    let mut paused = paused;
    let mut peer_attempts = HashMap::with_capacity(metadata_peer_attempt_cache_cap(max_peers));
    let mut tracker_tick = interval(Duration::from_secs(60));
    let http_timeout = Duration::from_secs(http_timeout_secs);
    let udp_timeout = Duration::from_secs(udp_timeout_secs);
    let mut tracker_event = TrackerEvent::Started;
    let mut pending_command = None;
    let direct_peers = task_memory.direct_peers.clone();
    let mut direct_peer_retry = !paused && !direct_peers.is_empty();
    loop {
        // BEP 9 direct peers should be tried immediately, including for a
        // trackerless magnet. Keep the operation outside the main select so
        // it cannot borrow the retry map concurrently with a tracker tick;
        // lifecycle commands still interrupt it through the same bounded
        // operation wrapper used by tracker and inbound-peer work.
        if direct_peer_retry && !paused && pending_command.is_none() {
            direct_peer_retry = false;
            match await_metadata_operation(
                &mut cmd_rx,
                try_fetch_from_peers(
                    info_hash,
                    &info_hash_hex,
                    &trackers,
                    direct_peers.clone(),
                    max_peers,
                    &egress_policy,
                    &mut peer_attempts,
                    &engine_tx,
                    &task_tx,
                    &registry,
                    &resources,
                    &network_budget,
                ),
            )
            .await
            {
                MetadataOperation::Completed(true) => {
                    wait_for_metadata_completion(&mut cmd_rx, paused).await;
                    return;
                }
                MetadataOperation::Completed(false) => {}
                MetadataOperation::Command(command) => {
                    pending_command = Some(command);
                    direct_peer_retry = true;
                }
                MetadataOperation::Closed => return,
            }
            continue;
        }
        tokio::select! {
            command = receive_metadata_command(&mut cmd_rx, &mut pending_command) => {
                let Some(cmd) = command else {
                    // The owning engine disappeared. Metadata work has no
                    // independent lifecycle and must not remain alive on its
                    // tracker timer after its command channel is gone.
                    warn!(
                        component = "metadata",
                        operation = "run",
                        torrent = %info_hash_hex,
                        result = "command_channel_closed",
                        "metadata command channel closed; shutting down"
                    );
                    return;
                };
                match cmd {
                    TorrentCmd::NewPeers(peers) | TorrentCmd::PriorityPeers(peers) => {
                        if paused {
                            continue;
                        }
                        match await_metadata_operation(
                            &mut cmd_rx,
                            try_fetch_from_peers(
                                info_hash,
                                &info_hash_hex,
                                &trackers,
                                peers,
                                max_peers,
                                &egress_policy,
                                &mut peer_attempts,
                                &engine_tx,
                                &task_tx,
                                &registry,
                                &resources,
                                &network_budget,
                            ),
                        )
                        .await
                        {
                            MetadataOperation::Completed(true) => {
                                wait_for_metadata_completion(&mut cmd_rx, paused).await;
                                return;
                            }
                            MetadataOperation::Completed(false) => {}
                            MetadataOperation::Command(command) => {
                                pending_command = Some(command);
                            }
                            MetadataOperation::Closed => return,
                        }
                    }
                    TorrentCmd::AcceptPeer {
                        stream,
                        peer_addr,
                        handshake,
                        peer_permit,
                    } => if paused {
                        drop(stream);
                    } else {
                        let operation = async {
                            let info = fetch_from_incoming_peer(
                                stream,
                                peer_addr,
                                info_hash,
                                handshake,
                                resources.clone(),
                                peer_permit,
                            )
                            .await?;
                            Ok::<bool, anyhow::Error>(
                                complete_metadata(
                                    &engine_tx,
                                    &task_tx,
                                    &info_hash_hex,
                                    &trackers,
                                    info,
                                )
                                .await,
                            )
                        };
                        match await_metadata_operation(&mut cmd_rx, operation).await {
                            MetadataOperation::Completed(Ok(true)) => {
                                wait_for_metadata_completion(&mut cmd_rx, paused).await;
                                return;
                            }
                            MetadataOperation::Completed(Ok(false)) => {}
                            MetadataOperation::Completed(Err(e)) => {
                                debug!(
                                    component = "metadata",
                                    operation = "fetch_incoming_peer",
                                    torrent = %info_hash_hex,
                                    peer = %peer_addr,
                                    result = "error",
                                    error = %e,
                                    "incoming metadata fetch failed"
                                )
                            }
                            MetadataOperation::Command(command) => {
                                pending_command = Some(command);
                            }
                            MetadataOperation::Closed => return,
                        }
                    },
                    TorrentCmd::AcceptUtpPeer {
                        stream,
                        peer_addr,
                        handshake,
                        peer_permit,
                    } => if paused {
                        drop(stream);
                    } else {
                        let operation = async {
                            let info = fetch_from_incoming_utp_peer(
                                *stream,
                                peer_addr,
                                info_hash,
                                handshake,
                                resources.clone(),
                                peer_permit,
                            )
                            .await?;
                            Ok::<bool, anyhow::Error>(
                                complete_metadata(
                                    &engine_tx,
                                    &task_tx,
                                    &info_hash_hex,
                                    &trackers,
                                    info,
                                )
                                .await,
                            )
                        };
                        match await_metadata_operation(&mut cmd_rx, operation).await {
                            MetadataOperation::Completed(Ok(true)) => {
                                wait_for_metadata_completion(&mut cmd_rx, paused).await;
                                return;
                            }
                            MetadataOperation::Completed(Ok(false)) => {}
                            MetadataOperation::Completed(Err(e)) => {
                                debug!(
                                    component = "metadata",
                                    operation = "fetch_incoming_utp_peer",
                                    torrent = %info_hash_hex,
                                    peer = %peer_addr,
                                    result = "error",
                                    error = %e,
                                    "incoming uTP metadata fetch failed"
                                )
                            }
                            MetadataOperation::Command(command) => {
                                pending_command = Some(command);
                            }
                            MetadataOperation::Closed => return,
                        }
                    },
                    TorrentCmd::Shutdown => {
                        if !paused {
                            let stopped_announce = announce_trackers(
                                info_hash,
                                &info_hash_hex,
                                &trackers,
                                current_listen_port(&network_budget, listen_port),
                                max_peers,
                                http_timeout,
                                udp_timeout,
                                TrackerEvent::Stopped,
                                &egress_policy,
                                &resources,
                            );
                            // Stopped is best-effort housekeeping. Do not let
                            // it strand a later lifecycle command behind a
                            // dead tracker; dropping the announce future
                            // cancels its in-flight network request.
                            if let MetadataOperation::Command(command) =
                                await_metadata_operation(&mut cmd_rx, stopped_announce).await
                            {
                                reject_metadata_command_during_shutdown(command);
                            }
                        }
                        return;
                    }
                    TorrentCmd::Pause { reply } => {
                        let was_paused = paused;
                        paused = true;
                        // The durable placeholder state is committed by the
                        // engine before this control command is delivered.
                        // A stopped announce is only best-effort follow-up and
                        // must not make the already-safe pause wait for a
                        // tracker timeout.
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(()));
                        }
                        if !was_paused {
                            let stopped_announce = announce_trackers(
                                info_hash,
                                &info_hash_hex,
                                &trackers,
                                current_listen_port(&network_budget, listen_port),
                                max_peers,
                                http_timeout,
                                udp_timeout,
                                TrackerEvent::Stopped,
                                &egress_policy,
                                &resources,
                            );
                            match await_metadata_operation(&mut cmd_rx, stopped_announce).await {
                                MetadataOperation::Completed(_) => {}
                                MetadataOperation::Command(command) => {
                                    pending_command = Some(command);
                                }
                                MetadataOperation::Closed => return,
                            }
                        }
                    }
                    TorrentCmd::Resume { reply } => {
                        paused = false;
                        tracker_event = TrackerEvent::Started;
                        tracker_tick.reset_immediately();
                        direct_peer_retry = !direct_peers.is_empty();
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(()));
                        }
                    }
                    TorrentCmd::Reannounce => {
                        request_metadata_reannounce(
                            paused,
                            &mut tracker_event,
                            &mut tracker_tick,
                        );
                        if !paused {
                            direct_peer_retry = !direct_peers.is_empty();
                        }
                    }
                    TorrentCmd::GetPeers { reply } => {
                        let _ = reply.send(Vec::new());
                    }
                    TorrentCmd::GetPeerSnapshotCount { reply } => {
                        let _ = reply.send(0);
                    }
                    TorrentCmd::GetPeerSnapshots { reply, .. } => {
                        let _ = reply.send(Ok(Vec::new()));
                    }
                    TorrentCmd::GetWebseeds { reply } => {
                        let _ = reply.send(Vec::new());
                    }
                    TorrentCmd::GetRuntimeStats { reply } => {
                        let _ = reply.send(Default::default());
                    }
                    TorrentCmd::QuiesceForStorageMove { reply } => {
                        // No metadata (and therefore no files) exist yet for
                        // a torrent still in this pre-metadata state, so
                        // there is no disk work to drain. The task still has
                        // to become quiescent: otherwise tracker ticks and
                        // incoming peers can continue metadata work while the
                        // storage plan is running.
                        let was_paused = quiesce_metadata_task(&mut paused);
                        let _ = reply.send(Ok(was_paused));
                    }
                    TorrentCmd::ResumeAfterStorageMove {
                        resume_paused,
                        reply,
                        ..
                    } => {
                        paused = resume_paused;
                        if !resume_paused {
                            tracker_event = TrackerEvent::Started;
                            tracker_tick.reset_immediately();
                            direct_peer_retry = !direct_peers.is_empty();
                        }
                        let _ = reply.send(Ok(()));
                    }
                    TorrentCmd::Recheck { .. }
                    | TorrentCmd::CancelJob { .. }
                    | TorrentCmd::UpdatePeerExchange(_)
                    | TorrentCmd::BanPeer(_)
                    | TorrentCmd::EvictBannedPeers => {}
                    TorrentCmd::ReloadFilePolicy { reply } => {
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(()));
                        }
                    }
                    TorrentCmd::UpdateLimits { reply, .. } => {
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(()));
                        }
                    }
                    TorrentCmd::UpdateTrackers { trackers: updated, reply } => {
                        let updated = compact_metadata_task_trackers(updated);
                        match task_memory.refresh_trackers(
                            &resources,
                            &updated,
                            updated.capacity(),
                            max_peers,
                        ) {
                            Ok(()) => {
                                trackers = updated;
                                peer_attempts.clear();
                                tracker_event = TrackerEvent::Empty;
                                tracker_tick.reset_immediately();
                                if let Some(reply) = reply {
                                    let _ = reply.send(Ok(()));
                                }
                            }
                            Err(error) => {
                                if let Some(reply) = reply {
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                    }
                }
            }
            _ = tracker_tick.tick(), if !paused && (!trackers.is_empty() || !direct_peers.is_empty()) => {
                let operation = async {
                    // Retry bounded BEP 9 x.pe hints even when the magnet
                    // has no tracker. The per-peer retry cache prevents an
                    // immediate duplicate after the first attempt while the
                    // interval provides eventual recovery for a peer that
                    // was temporarily offline.
                    if !direct_peers.is_empty()
                        && try_fetch_from_peers(
                            info_hash,
                            &info_hash_hex,
                            &trackers,
                            direct_peers.clone(),
                            max_peers,
                            &egress_policy,
                            &mut peer_attempts,
                            &engine_tx,
                            &task_tx,
                            &registry,
                            &resources,
                            &network_budget,
                        )
                        .await
                    {
                        return true;
                    }
                    if trackers.is_empty() {
                        return false;
                    }
                    let peers = announce_trackers(
                        info_hash,
                        &info_hash_hex,
                        &trackers,
                        current_listen_port(&network_budget, listen_port),
                        max_peers,
                        http_timeout,
                        udp_timeout,
                        tracker_event,
                        &egress_policy,
                        &resources,
                    )
                    .await;
                    try_fetch_from_peers(
                        info_hash,
                        &info_hash_hex,
                        &trackers,
                        peers,
                        max_peers,
                        &egress_policy,
                        &mut peer_attempts,
                        &engine_tx,
                        &task_tx,
                        &registry,
                        &resources,
                        &network_budget,
                    )
                    .await
                };
                match await_metadata_operation(&mut cmd_rx, operation).await {
                    MetadataOperation::Completed(found) => {
                        if tracker_event == TrackerEvent::Started {
                            tracker_event = TrackerEvent::Empty;
                        }
                        if found {
                            wait_for_metadata_completion(&mut cmd_rx, paused).await;
                            return;
                        }
                    }
                    MetadataOperation::Command(command) => {
                        pending_command = Some(command);
                    }
                    MetadataOperation::Closed => return,
                }
            }
            else => {
                if !paused {
                    let stopped_announce = announce_trackers(
                        info_hash,
                        &info_hash_hex,
                        &trackers,
                        current_listen_port(&network_budget, listen_port),
                        max_peers,
                        http_timeout,
                        udp_timeout,
                        TrackerEvent::Stopped,
                        &egress_policy,
                        &resources,
                    );
                    match await_metadata_operation(&mut cmd_rx, stopped_announce).await {
                        MetadataOperation::Completed(_) => {}
                        MetadataOperation::Command(command) => {
                            pending_command = Some(command);
                            continue;
                        }
                        MetadataOperation::Closed => return,
                    }
                }
                return;
            },
        }
    }
}

fn request_metadata_reannounce(
    paused: bool,
    tracker_event: &mut TrackerEvent,
    tracker_tick: &mut Interval,
) {
    if paused {
        return;
    }
    *tracker_event = TrackerEvent::Empty;
    tracker_tick.reset_immediately();
}

fn quiesce_metadata_task(paused: &mut bool) -> bool {
    let was_paused = *paused;
    *paused = true;
    was_paused
}

enum MetadataOperation<T> {
    Completed(T),
    Command(TorrentCmd),
    Closed,
}

async fn receive_metadata_command(
    cmd_rx: &mut mpsc::Receiver<TorrentCmd>,
    pending_command: &mut Option<TorrentCmd>,
) -> Option<TorrentCmd> {
    if pending_command.is_some() {
        pending_command.take()
    } else {
        cmd_rx.recv().await
    }
}

/// Keep metadata network operations interruptible by lifecycle commands. A
/// peer fetch has its own network deadline, but waiting for that deadline
/// before accepting Pause or Shutdown leaves the task unresponsive for up to
/// two minutes and strands incoming peer permits during that interval.
async fn await_metadata_operation<T, F>(
    cmd_rx: &mut mpsc::Receiver<TorrentCmd>,
    operation: F,
) -> MetadataOperation<T>
where
    F: Future<Output = T>,
{
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => MetadataOperation::Completed(result),
        command = cmd_rx.recv() => match command {
            Some(command) => MetadataOperation::Command(command),
            None => MetadataOperation::Closed,
        },
    }
}

fn reject_metadata_command_during_shutdown(command: TorrentCmd) {
    const ERROR: &str = "metadata task is shutting down";
    match command {
        TorrentCmd::Pause { reply } | TorrentCmd::Resume { reply } => {
            if let Some(reply) = reply {
                let _ = reply.send(Err(ERROR.to_owned()));
            }
        }
        TorrentCmd::ReloadFilePolicy { reply } | TorrentCmd::UpdateLimits { reply, .. } => {
            if let Some(reply) = reply {
                let _ = reply.send(Err(ERROR.to_owned()));
            }
        }
        TorrentCmd::UpdateTrackers { reply, .. } => {
            if let Some(reply) = reply {
                let _ = reply.send(Err(ERROR.to_owned()));
            }
        }
        TorrentCmd::QuiesceForStorageMove { reply } => {
            let _ = reply.send(Err(ERROR.to_owned()));
        }
        TorrentCmd::ResumeAfterStorageMove { reply, .. } => {
            let _ = reply.send(Err(ERROR.to_owned()));
        }
        TorrentCmd::GetPeers { reply } => {
            let _ = reply.send(Vec::new());
        }
        TorrentCmd::GetPeerSnapshotCount { reply } => {
            let _ = reply.send(0);
        }
        TorrentCmd::GetPeerSnapshots { reply, .. } => {
            let _ = reply.send(Err(ERROR.to_owned()));
        }
        TorrentCmd::GetWebseeds { reply } => {
            let _ = reply.send(Vec::new());
        }
        TorrentCmd::GetRuntimeStats { reply } => {
            let _ = reply.send(Default::default());
        }
        TorrentCmd::AcceptPeer { .. }
        | TorrentCmd::AcceptUtpPeer { .. }
        | TorrentCmd::Shutdown
        | TorrentCmd::Recheck { .. }
        | TorrentCmd::CancelJob { .. }
        | TorrentCmd::Reannounce
        | TorrentCmd::NewPeers(_)
        | TorrentCmd::PriorityPeers(_)
        | TorrentCmd::UpdatePeerExchange(_)
        | TorrentCmd::BanPeer(_)
        | TorrentCmd::EvictBannedPeers => {}
    }
}

/// Keep a metadata task alive after it has queued a successful completion.
/// The engine reaps finished torrent handles before processing its next
/// command; returning immediately here can therefore make the reaper remove
/// this task and its source channel before `CompleteMagnet` is handled. The
/// engine replaces or aborts this task after the staged metadata is durable.
async fn wait_for_metadata_completion(cmd_rx: &mut mpsc::Receiver<TorrentCmd>, mut paused: bool) {
    while let Some(command) = cmd_rx.recv().await {
        match command {
            TorrentCmd::Shutdown => return,
            TorrentCmd::Pause { reply } => {
                paused = true;
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
            TorrentCmd::Resume { reply } => {
                paused = false;
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
            TorrentCmd::QuiesceForStorageMove { reply } => {
                let was_paused = paused;
                paused = true;
                let _ = reply.send(Ok(was_paused));
            }
            TorrentCmd::ResumeAfterStorageMove {
                resume_paused,
                reply,
                ..
            } => {
                paused = resume_paused;
                let _ = reply.send(Ok(()));
            }
            TorrentCmd::ReloadFilePolicy { reply } => {
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
            TorrentCmd::UpdateLimits { reply, .. } => {
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
            TorrentCmd::UpdateTrackers { reply, .. } => {
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
            TorrentCmd::GetPeers { reply } => {
                let _ = reply.send(Vec::new());
            }
            TorrentCmd::GetPeerSnapshotCount { reply } => {
                let _ = reply.send(0);
            }
            TorrentCmd::GetPeerSnapshots { reply, .. } => {
                let _ = reply.send(Ok(Vec::new()));
            }
            TorrentCmd::GetWebseeds { reply } => {
                let _ = reply.send(Vec::new());
            }
            TorrentCmd::GetRuntimeStats { reply } => {
                let _ = reply.send(Default::default());
            }
            TorrentCmd::AcceptPeer { .. } | TorrentCmd::AcceptUtpPeer { .. } => {}
            TorrentCmd::Recheck { .. }
            | TorrentCmd::CancelJob { .. }
            | TorrentCmd::Reannounce
            | TorrentCmd::NewPeers(_)
            | TorrentCmd::PriorityPeers(_)
            | TorrentCmd::UpdatePeerExchange(_)
            | TorrentCmd::BanPeer(_)
            | TorrentCmd::EvictBannedPeers => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn try_fetch_from_peers(
    info_hash: MetadataInfoHash,
    info_hash_hex: &str,
    trackers: &[String],
    peers: Vec<SocketAddr>,
    max_peers: usize,
    egress_policy: &OutboundEgressPolicy,
    peer_attempts: &mut HashMap<SocketAddr, Instant>,
    engine_tx: &mpsc::Sender<EngineCmd>,
    task_tx: &mpsc::Sender<TorrentCmd>,
    registry: &Arc<RwLock<SessionRegistry>>,
    resources: &ResourceGovernor,
    network_budget: &GlobalNetworkBudget,
) -> bool {
    let mut candidates = {
        let registry = registry.read().await;
        metadata_fetch_candidates(
            peers,
            egress_policy,
            &registry,
            peer_attempts,
            Instant::now(),
            metadata_peer_attempt_cache_cap(max_peers),
            metadata_peer_candidate_cap(max_peers),
        )
    };
    let mut in_flight = FuturesUnordered::new();

    while in_flight.len() < MAX_METADATA_FETCH_CONCURRENCY {
        let Some(peer) = candidates.pop_front() else {
            break;
        };
        in_flight.push(metadata_fetch_attempt(
            peer,
            info_hash,
            resources.clone(),
            network_budget.clone(),
            Arc::clone(registry),
            *egress_policy,
        ));
    }

    while let Some((peer, result)) = in_flight.next().await {
        match result {
            Ok(info) => {
                if complete_metadata(engine_tx, task_tx, info_hash_hex, trackers, info).await {
                    return true;
                }
            }
            Err(e) => {
                debug!(
                    component = "metadata",
                    operation = "fetch_peer",
                    torrent = %info_hash_hex,
                    peer = %peer,
                    result = "error",
                    error = %e,
                    "metadata fetch failed"
                )
            }
        }

        if let Some(peer) = candidates.pop_front() {
            in_flight.push(metadata_fetch_attempt(
                peer,
                info_hash,
                resources.clone(),
                network_budget.clone(),
                Arc::clone(registry),
                *egress_policy,
            ));
        }
    }

    false
}

fn should_retry_peer(
    peer_attempts: &mut HashMap<SocketAddr, Instant>,
    peer: SocketAddr,
    now: Instant,
) -> bool {
    if peer_attempts
        .get(&peer)
        .is_some_and(|last| now.duration_since(*last) < METADATA_PEER_RETRY_AFTER)
    {
        return false;
    }
    peer_attempts.insert(peer, now);
    true
}

fn metadata_peer_attempt_cache_cap(max_peers: usize) -> usize {
    max_peers
        .saturating_mul(METADATA_PEER_ATTEMPT_CACHE_MULTIPLIER)
        .clamp(METADATA_PEER_ATTEMPT_CACHE_MIN, MAX_TRACKER_PEERS)
}

fn metadata_peer_candidate_cap(max_peers: usize) -> usize {
    max_peers.clamp(MAX_METADATA_FETCH_CONCURRENCY, MAX_TRACKER_PEERS)
}

fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn bencoded_bytes_len(payload_len: usize) -> usize {
    decimal_digits(payload_len)
        .saturating_add(1)
        .saturating_add(payload_len)
}

fn metadata_completion_tracker_bytes(trackers: &[String]) -> usize {
    let Some(first) = trackers.first() else {
        return 0;
    };

    let mut total = bencoded_bytes_len(b"announce".len())
        .saturating_add(bencoded_bytes_len(first.len()))
        .saturating_add(bencoded_bytes_len(b"announce-list".len()))
        .saturating_add(1); // announce-list value: outer list start
    for tracker in trackers {
        total = total
            .saturating_add(1) // tier list start
            .saturating_add(bencoded_bytes_len(tracker.len()))
            .saturating_add(1); // tier list end
    }
    total.saturating_add(1) // announce-list value: outer list end
}

fn metadata_tracker_peer_collection_bytes(peer_cap: usize) -> u64 {
    u64::try_from(peer_cap)
        .unwrap_or(u64::MAX)
        .saturating_mul(METADATA_TRACKER_PEER_COLLECTION_BYTES_PER_ENTRY)
}

fn metadata_fetch_candidates(
    peers: Vec<SocketAddr>,
    egress_policy: &OutboundEgressPolicy,
    registry: &SessionRegistry,
    peer_attempts: &mut HashMap<SocketAddr, Instant>,
    now: Instant,
    attempt_cap: usize,
    candidate_cap: usize,
) -> VecDeque<SocketAddr> {
    peer_attempts.retain(|_, last| now.duration_since(*last) < METADATA_PEER_RETRY_AFTER);
    let mut candidates = VecDeque::new();
    for peer in peers {
        if candidates.len() >= candidate_cap {
            break;
        }
        if registry.is_peer_banned(peer) {
            debug!(
                component = "metadata",
                operation = "fetch_peer",
                peer = %peer,
                result = "rejected",
                reason = "peer_banned",
                "skipping banned metadata peer"
            );
            continue;
        }
        if let Err(error) = egress_policy.validate_peer_addr(peer) {
            debug!(
                component = "metadata",
                operation = "fetch_peer",
                peer = %peer,
                result = "rejected",
                reason = "egress_address_policy",
                error = %error,
                "skipping metadata peer denied by address policy"
            );
            continue;
        }
        if !should_retry_peer(peer_attempts, peer, now) {
            continue;
        }
        prune_metadata_peer_attempts(peer_attempts, attempt_cap, peer);
        candidates.push_back(peer);
    }
    candidates
}

fn prune_metadata_peer_attempts(
    peer_attempts: &mut HashMap<SocketAddr, Instant>,
    attempt_cap: usize,
    protected_peer: SocketAddr,
) {
    while peer_attempts.len() > attempt_cap {
        let Some(oldest_peer) = peer_attempts
            .iter()
            .filter(|(peer, _)| **peer != protected_peer)
            .min_by_key(|(_, attempted_at)| *attempted_at)
            .map(|(peer, _)| *peer)
        else {
            break;
        };
        peer_attempts.remove(&oldest_peer);
    }
}

async fn metadata_fetch_attempt(
    peer: SocketAddr,
    info_hash: MetadataInfoHash,
    resources: ResourceGovernor,
    network_budget: GlobalNetworkBudget,
    registry: Arc<RwLock<SessionRegistry>>,
    egress_policy: OutboundEgressPolicy,
) -> (SocketAddr, anyhow::Result<FetchedMetadata>) {
    if let Err(error) = egress_policy.validate_peer_addr(peer) {
        debug!(
            component = "metadata",
            operation = "connect_peer",
            peer = %peer,
            result = "rejected",
            reason = "egress_address_policy",
            error = %error,
            "denying metadata peer before opening an outbound transport"
        );
        return (
            peer,
            Err(anyhow::anyhow!(
                "metadata peer denied by egress policy: {error}"
            )),
        );
    }
    let peer_banned = registry.read().await.is_peer_banned(peer);
    if peer_banned {
        debug!(
            component = "metadata",
            operation = "connect_peer",
            peer = %peer,
            result = "rejected",
            reason = "peer_banned",
            "denying banned metadata peer before opening an outbound transport"
        );
        return (peer, Err(anyhow::anyhow!("metadata peer is banned")));
    }
    let result = match network_budget.try_acquire_peer() {
        Ok(_peer_permit) => match metadata_outgoing_transport_policy() {
            MetadataTransportPolicy::TcpOnly => {
                fetch_from_outgoing_peer(peer, info_hash, resources).await
            }
            MetadataTransportPolicy::UtpOnly => {
                fetch_from_outgoing_utp_peer(peer, info_hash, resources).await
            }
            MetadataTransportPolicy::PreferUtp => {
                match fetch_from_outgoing_utp_peer(peer, info_hash, resources.clone()).await {
                    Ok(info) => Ok(info),
                    Err(e) => {
                        debug!(
                            component = "metadata",
                            operation = "fetch_outgoing_utp_peer",
                            peer = %peer,
                            result = "fallback",
                            error = %e,
                            "uTP metadata fetch failed; falling back to TCP"
                        );
                        fetch_from_outgoing_peer(peer, info_hash, resources).await
                    }
                }
            }
        },
        Err(_) => Err(anyhow::anyhow!("global peer connection budget exhausted")),
    };
    (peer, result)
}

async fn complete_metadata(
    engine_tx: &mpsc::Sender<EngineCmd>,
    task_tx: &mpsc::Sender<TorrentCmd>,
    info_hash_hex: &str,
    trackers: &[String],
    fetched: FetchedMetadata,
) -> bool {
    let FetchedMetadata {
        bytes: info,
        piece_layers,
        _lease: metadata_memory_lease,
    } = fetched;
    let raw = match build_torrent_from_info(&info, trackers, &piece_layers) {
        Ok(raw) => raw,
        Err(error) => {
            warn!(
                component = "metadata",
                operation = "complete",
                torrent = %info_hash_hex,
                result = "allocation_failed",
                error = %error,
                "metadata completion raw metainfo allocation failed"
            );
            return false;
        }
    };
    let delivered = crate::engine::send_engine_command_until_delivered(
        engine_tx.clone(),
        EngineCmd::CompleteMagnet {
            info_hash: info_hash_hex.to_owned(),
            raw,
            metadata_memory_lease,
            source: task_tx.clone(),
        },
        "metadata_completion",
    )
    .await;
    delivered
}

#[allow(clippy::too_many_arguments)]
async fn announce_trackers(
    info_hash: MetadataInfoHash,
    info_hash_hex: &str,
    trackers: &[String],
    listen_port: u16,
    max_peers: usize,
    http_timeout: Duration,
    udp_timeout: Duration,
    event: TrackerEvent,
    egress_policy: &OutboundEgressPolicy,
    resources: &ResourceGovernor,
) -> Vec<SocketAddr> {
    let peer_cap = metadata_peer_candidate_cap(max_peers);
    let collection_bytes = metadata_tracker_peer_collection_bytes(peer_cap);
    let Some(_collection_lease) =
        resources.try_acquire(MemoryClass::TrackerPeers, collection_bytes)
    else {
        warn!(
            component = "metadata",
            operation = "tracker_announce",
            torrent = %info_hash_hex,
            result = "memory_denied",
            requested_bytes = collection_bytes,
            "metadata tracker peer collection memory budget exhausted"
        );
        return Vec::new();
    };

    let mut peers = Vec::new();
    if peers.try_reserve(peer_cap).is_err() {
        warn!(
            component = "metadata",
            operation = "tracker_announce",
            torrent = %info_hash_hex,
            result = "allocation_failed",
            peer_cap,
            "metadata tracker peer result allocation failed"
        );
        return Vec::new();
    }
    let mut seen = HashSet::new();
    if seen.try_reserve(peer_cap).is_err() {
        warn!(
            component = "metadata",
            operation = "tracker_announce",
            torrent = %info_hash_hex,
            result = "allocation_failed",
            peer_cap,
            "metadata tracker peer dedup allocation failed"
        );
        return Vec::new();
    }
    let deadline = Instant::now() + METADATA_TRACKER_ANNOUNCE_DEADLINE;
    for (tracker_index, tracker) in trackers.iter().enumerate() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            warn!(
                component = "metadata",
                operation = "tracker_announce",
                torrent = %info_hash_hex,
                result = "deadline_exceeded",
                completed = tracker_index,
                deadline_secs = METADATA_TRACKER_ANNOUNCE_DEADLINE.as_secs(),
                "metadata tracker announce deadline exceeded"
            );
            break;
        };
        if remaining.is_zero() {
            warn!(
                component = "metadata",
                operation = "tracker_announce",
                torrent = %info_hash_hex,
                result = "deadline_exceeded",
                completed = tracker_index,
                deadline_secs = METADATA_TRACKER_ANNOUNCE_DEADLINE.as_secs(),
                "metadata tracker announce deadline exceeded"
            );
            break;
        }

        match timeout(
            remaining,
            announce_tracker(
                tracker,
                info_hash,
                listen_port,
                max_peers,
                http_timeout,
                udp_timeout,
                event,
                egress_policy,
                resources,
            ),
        )
        .await
        {
            Err(_) => {
                warn!(
                    component = "metadata",
                    operation = "tracker_announce",
                    torrent = %info_hash_hex,
                    result = "deadline_exceeded",
                    completed = tracker_index,
                    deadline_secs = METADATA_TRACKER_ANNOUNCE_DEADLINE.as_secs(),
                    "metadata tracker announce deadline exceeded"
                );
                break;
            }
            Ok(result) => match result {
                Ok(resp) => {
                    for peer in resp.peers.into_iter().map(|peer| peer.addr) {
                        if peers.len() >= peer_cap {
                            return peers;
                        }
                        if seen.insert(peer) {
                            peers.push(peer);
                        }
                    }
                }
                Err(err) => {
                    warn!(
                        component = "metadata",
                        operation = "tracker_announce",
                        torrent = %info_hash_hex,
                        tracker = %url_log_target(tracker),
                        result = "error",
                        error = %err,
                        "metadata tracker announce failed"
                    );
                }
            },
        }
    }
    peers
}

// TNG-005: egress_policy is the 8th parameter, added so every tracker
// announce path (this one and torrent_task's) routes through one
// server-owned outbound policy rather than each having its own client.
// Single call site (below) -- a context struct would be pure ceremony here.
#[allow(clippy::too_many_arguments)]
async fn announce_tracker(
    tracker_url: &str,
    info_hash: MetadataInfoHash,
    listen_port: u16,
    max_peers: usize,
    http_timeout: Duration,
    udp_timeout: Duration,
    event: TrackerEvent,
    egress_policy: &OutboundEgressPolicy,
    resources: &ResourceGovernor,
) -> Result<AnnounceResponse, TrackerError> {
    if is_udp_tracker_url(tracker_url) {
        announce_udp(
            tracker_url,
            info_hash,
            listen_port,
            max_peers,
            udp_timeout,
            event,
            egress_policy,
            resources,
        )
        .await
    } else {
        announce_http(
            tracker_url,
            info_hash,
            listen_port,
            max_peers,
            http_timeout,
            event,
            egress_policy,
            resources,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn announce_http(
    tracker_url: &str,
    info_hash: MetadataInfoHash,
    listen_port: u16,
    max_peers: usize,
    http_timeout: Duration,
    event: TrackerEvent,
    egress_policy: &OutboundEgressPolicy,
    resources: &ResourceGovernor,
) -> Result<AnnounceResponse, TrackerError> {
    let tracker =
        Url::parse(tracker_url).map_err(|error| TrackerError::InvalidUrl(error.to_string()))?;
    let user_agent = crate::peer_id::user_agent();
    let client = egress_policy
        .http_client(
            OutboundTargetKind::Tracker,
            &tracker,
            http_timeout,
            &user_agent,
        )
        .await
        .map_err(|error| TrackerError::Network(error.to_string()))?;
    let req = metadata_announce_request(info_hash, listen_port, max_peers, event);
    let url = req.to_http_query(tracker_url)?;
    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            TrackerError::Timeout
        } else {
            TrackerError::Network(e.without_url().to_string())
        }
    })?;
    if !response.status().is_success() {
        return Err(TrackerError::Http {
            status: response.status().as_u16(),
        });
    }
    let body = crate::tracker_runtime::bounded_response_body_with_memory_and_extra(
        response,
        4 * 1024 * 1024,
        crate::tracker_runtime::tracker_response_output_memory_bytes(
            max_peers.min(MAX_TRACKER_PEERS),
        ),
        resources,
    )
    .await?;
    AnnounceResponse::parse_with_peer_limit(&body.bytes, max_peers.min(MAX_TRACKER_PEERS))
}

#[allow(clippy::too_many_arguments)]
async fn announce_udp(
    tracker_url: &str,
    info_hash: MetadataInfoHash,
    listen_port: u16,
    max_peers: usize,
    udp_timeout: Duration,
    event: TrackerEvent,
    egress_policy: &OutboundEgressPolicy,
    resources: &ResourceGovernor,
) -> Result<AnnounceResponse, TrackerError> {
    let url = Url::parse(tracker_url).map_err(|e| TrackerError::InvalidUrl(e.to_string()))?;
    let mut addrs = egress_policy
        .resolve_and_validate(OutboundTargetKind::Tracker, &url, udp_timeout)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    let tracker_addr = addrs
        .drain(..)
        .next()
        .ok_or_else(|| TrackerError::Network("no validated tracker address".to_owned()))?;

    let bind_addr = if tracker_addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind_addr)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    socket
        .connect(tracker_addr)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;

    let connect = UdpConnectRequest::new();
    socket
        .send(&connect.encode())
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;

    let response_lease = crate::tracker_runtime::reserve_tracker_response_bytes(
        resources,
        crate::tracker_runtime::MAX_TRACKER_UDP_RESPONSE_BYTES.saturating_add(
            crate::tracker_runtime::tracker_response_output_memory_bytes(
                max_peers.min(MAX_TRACKER_PEERS),
            ),
        ),
    )?;
    let mut buf = vec![0u8; crate::tracker_runtime::MAX_TRACKER_UDP_RESPONSE_BYTES];
    let n = tokio::time::timeout(udp_timeout, socket.recv(&mut buf))
        .await
        .map_err(|_| TrackerError::Timeout)?
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    let connect_resp = UdpConnectResponse::parse(&buf[..n])?;
    if connect_resp.transaction_id != connect.transaction_id {
        return Err(TrackerError::Udp("connect transaction id mismatch".into()));
    }

    let announce = UdpAnnounceRequest::new(
        connect_resp.connection_id,
        metadata_announce_request(info_hash, listen_port, max_peers, event),
    );
    let encoded = announce.encode()?;
    socket
        .send(&encoded)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;

    let n = tokio::time::timeout(udp_timeout, socket.recv(&mut buf))
        .await
        .map_err(|_| TrackerError::Timeout)?
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    let announce_resp =
        UdpAnnounceResponse::parse_with_peer_limit(&buf[..n], max_peers.min(MAX_TRACKER_PEERS))?;
    if announce_resp.transaction_id != announce.transaction_id {
        return Err(TrackerError::Udp("announce transaction id mismatch".into()));
    }

    drop(response_lease);
    Ok(AnnounceResponse {
        interval: announce_resp.interval,
        min_interval: None,
        peers: announce_resp.peers,
        tracker_id: None,
        warning_message: None,
        complete: Some(announce_resp.seeders),
        incomplete: Some(announce_resp.leechers),
    })
}

fn metadata_announce_request(
    info_hash: MetadataInfoHash,
    listen_port: u16,
    max_peers: usize,
    event: TrackerEvent,
) -> AnnounceRequest {
    AnnounceRequest {
        // BEP 52 tracker announces carry the truncated 20-byte v2 hash. The
        // tracker codec's V1 variant is the wire representation here; the
        // durable identity remains the full SHA-256 digest.
        info_hash: InfoHash::V1(info_hash.wire_hash()),
        peer_id: crate::peer_id::our_peer_id(),
        port: listen_port,
        uploaded: 0,
        downloaded: 0,
        left: 0,
        event,
        compact: true,
        numwant: Some(protocol_numwant(max_peers)),
    }
}

async fn fetch_from_outgoing_peer(
    addr: SocketAddr,
    info_hash: MetadataInfoHash,
    resources: ResourceGovernor,
) -> anyhow::Result<FetchedMetadata> {
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await??;
    stream.set_nodelay(true)?;
    let mut framed = Framed::with_capacity(stream, PeerCodec::with_resources(resources.clone()), 1);
    write_handshake(&mut framed, info_hash).await?;

    let remote_hs = read_handshake(&mut framed).await?;
    if remote_hs.info_hash != info_hash.wire_hash() {
        anyhow::bail!("info_hash mismatch from {addr}");
    }
    fetch_metadata(
        addr,
        framed,
        remote_hs.reserved.supports_extension_protocol(),
        remote_hs.reserved.supports_v2(),
        info_hash,
        resources,
    )
    .await
}

async fn fetch_from_incoming_peer(
    stream: TcpStream,
    addr: SocketAddr,
    info_hash: MetadataInfoHash,
    remote_hs: Handshake,
    resources: ResourceGovernor,
    _peer_permit: OwnedSemaphorePermit,
) -> anyhow::Result<FetchedMetadata> {
    stream.set_nodelay(true)?;
    let mut framed = Framed::with_capacity(stream, PeerCodec::with_resources(resources.clone()), 1);
    if remote_hs.info_hash != info_hash.wire_hash() {
        anyhow::bail!("info_hash mismatch from {addr}");
    }
    write_handshake(&mut framed, info_hash).await?;
    fetch_metadata(
        addr,
        framed,
        remote_hs.reserved.supports_extension_protocol(),
        remote_hs.reserved.supports_v2(),
        info_hash,
        resources,
    )
    .await
}

async fn fetch_from_outgoing_utp_peer(
    addr: SocketAddr,
    info_hash: MetadataInfoHash,
    resources: ResourceGovernor,
) -> anyhow::Result<FetchedMetadata> {
    let mut stream = UtpStream::connect(addr).await?;
    write_utp_handshake(&mut stream, info_hash).await?;

    let remote_hs = read_utp_handshake(&mut stream).await?;
    if remote_hs.info_hash != info_hash.wire_hash() {
        anyhow::bail!("info_hash mismatch from {addr}");
    }
    fetch_metadata_over_io(
        addr,
        MetadataPeerIo::Utp {
            stream: Box::new(stream),
            decoder: UtpFrameDecoder::new(resources.clone()),
            write_buffer: UtpWireBuffer::default(),
        },
        remote_hs.reserved.supports_extension_protocol(),
        remote_hs.reserved.supports_v2(),
        info_hash,
        resources,
    )
    .await
}

async fn fetch_from_incoming_utp_peer(
    mut stream: UtpStream,
    addr: SocketAddr,
    info_hash: MetadataInfoHash,
    remote_hs: Handshake,
    resources: ResourceGovernor,
    _peer_permit: OwnedSemaphorePermit,
) -> anyhow::Result<FetchedMetadata> {
    if remote_hs.info_hash != info_hash.wire_hash() {
        anyhow::bail!("info_hash mismatch from {addr}");
    }
    write_utp_handshake(&mut stream, info_hash).await?;
    fetch_metadata_over_io(
        addr,
        MetadataPeerIo::Utp {
            stream: Box::new(stream),
            decoder: UtpFrameDecoder::new(resources.clone()),
            write_buffer: UtpWireBuffer::default(),
        },
        remote_hs.reserved.supports_extension_protocol(),
        remote_hs.reserved.supports_v2(),
        info_hash,
        resources,
    )
    .await
}

async fn write_handshake(
    framed: &mut Framed<TcpStream, PeerCodec>,
    info_hash: MetadataInfoHash,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let hs = Handshake {
        info_hash: info_hash.wire_hash(),
        peer_id: crate::peer_id::our_peer_id(),
        // Advertise v2 capability even for a v1 magnet: the peer may return
        // a hybrid info dictionary whose BEP 52 piece layers then need the
        // hash-exchange extension.
        reserved: ExtensionFlags::with_v2_support(),
    };
    timeout(
        METADATA_PEER_WRITE_TIMEOUT,
        framed.get_mut().write_all(&hs.encode()),
    )
    .await
    .map_err(|_| anyhow::anyhow!("metadata peer handshake write timed out"))??;
    Ok(())
}

async fn read_handshake(framed: &mut Framed<TcpStream, PeerCodec>) -> anyhow::Result<Handshake> {
    use tokio::io::AsyncReadExt;
    let mut hs_buf = [0u8; 68];
    tokio::time::timeout(
        METADATA_PEER_HANDSHAKE_TIMEOUT,
        framed.get_mut().read_exact(&mut hs_buf),
    )
    .await??;
    Ok(Handshake::parse(&hs_buf)?)
}

async fn write_utp_handshake(
    stream: &mut UtpStream,
    info_hash: MetadataInfoHash,
) -> anyhow::Result<()> {
    let hs = Handshake {
        info_hash: info_hash.wire_hash(),
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_v2_support(),
    };
    timeout(METADATA_PEER_WRITE_TIMEOUT, stream.write_all(&hs.encode()))
        .await
        .map_err(|_| anyhow::anyhow!("metadata peer handshake write timed out"))??;
    Ok(())
}

async fn read_utp_handshake(stream: &mut UtpStream) -> anyhow::Result<Handshake> {
    read_utp_handshake_with_timeout(stream, METADATA_PEER_HANDSHAKE_TIMEOUT).await
}

async fn read_utp_handshake_with_timeout(
    stream: &mut UtpStream,
    handshake_timeout: Duration,
) -> anyhow::Result<Handshake> {
    let mut hs_buf = [0u8; 68];
    tokio::time::timeout(handshake_timeout, stream.read_exact(&mut hs_buf)).await??;
    Ok(Handshake::parse(&hs_buf)?)
}

async fn fetch_metadata(
    addr: SocketAddr,
    framed: Framed<TcpStream, PeerCodec>,
    remote_supports_extension: bool,
    remote_supports_v2: bool,
    expected_info_hash: MetadataInfoHash,
    resources: ResourceGovernor,
) -> anyhow::Result<FetchedMetadata> {
    fetch_metadata_over_io(
        addr,
        MetadataPeerIo::Tcp(framed),
        remote_supports_extension,
        remote_supports_v2,
        expected_info_hash,
        resources,
    )
    .await
}

async fn fetch_metadata_over_io(
    addr: SocketAddr,
    peer_io: MetadataPeerIo,
    remote_supports_extension: bool,
    remote_supports_v2: bool,
    expected_info_hash: MetadataInfoHash,
    resources: ResourceGovernor,
) -> anyhow::Result<FetchedMetadata> {
    fetch_metadata_over_io_with_timeout(
        addr,
        peer_io,
        remote_supports_extension,
        remote_supports_v2,
        expected_info_hash,
        resources,
        METADATA_PEER_FETCH_TIMEOUT,
    )
    .await
}

async fn fetch_metadata_over_io_with_timeout(
    addr: SocketAddr,
    peer_io: MetadataPeerIo,
    remote_supports_extension: bool,
    remote_supports_v2: bool,
    expected_info_hash: MetadataInfoHash,
    resources: ResourceGovernor,
    fetch_timeout: Duration,
) -> anyhow::Result<FetchedMetadata> {
    timeout(
        fetch_timeout,
        fetch_metadata_over_io_inner(
            addr,
            peer_io,
            remote_supports_extension,
            remote_supports_v2,
            expected_info_hash,
            resources,
        ),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "metadata fetch from {addr} timed out after {} seconds",
            fetch_timeout.as_secs()
        )
    })?
}

async fn fetch_metadata_over_io_inner(
    addr: SocketAddr,
    mut peer_io: MetadataPeerIo,
    remote_supports_extension: bool,
    remote_supports_v2: bool,
    expected_info_hash: MetadataInfoHash,
    resources: ResourceGovernor,
) -> anyhow::Result<FetchedMetadata> {
    if !remote_supports_extension {
        anyhow::bail!("peer does not support BEP 10");
    }
    peer_io
        .send(Message::Extended {
            ext_id: EXT_HANDSHAKE_ID,
            payload: ExtensionHandshake::new(None)
                .with_ut_metadata(LOCAL_UT_METADATA_ID)
                .encode(),
        })
        .await?;
    peer_io.send(Message::Interested).await?;

    let (remote_ext_id, metadata_size) = read_remote_metadata_handshake(addr, &mut peer_io).await?;
    let mut lease = reserve_metadata_fetch_bytes(&resources, metadata_size)?;
    let piece_count = metadata_size.div_ceil(METADATA_PIECE_SIZE as u32);
    // Requests are issued and validated in piece order, so retain only the
    // final metadata buffer and the one response currently being copied into
    // it. A BTreeMap here kept every piece allocation alive while a second
    // full-size buffer was assembled, exceeding the payload-only reservation.
    let mut metadata = Vec::new();
    metadata
        .try_reserve_exact(metadata_size as usize)
        .map_err(|error| anyhow::anyhow!("metadata allocation failed: {error}"))?;

    for piece in 0..piece_count {
        peer_io
            .send(Message::Extended {
                ext_id: remote_ext_id,
                payload: UtMetadataMessage::Request { piece }.encode(),
            })
            .await?;
        match read_metadata_piece(
            addr,
            &mut peer_io,
            LOCAL_UT_METADATA_ID,
            piece,
            metadata_size,
        )
        .await?
        {
            Some(data) => {
                metadata.extend_from_slice(&data);
            }
            None => anyhow::bail!("peer rejected metadata piece {piece}"),
        }
    }

    metadata.truncate(metadata_size as usize);
    let requirements = {
        let mut reserve_parser_memory = |additional| {
            u64::try_from(additional)
                .map(|bytes| lease.try_grow(bytes))
                .unwrap_or(false)
        };
        decode_with_allocation_reservation(&metadata, &mut reserve_parser_memory)
            .context("fetched metadata is not valid bencode")?;
        validate_metadata_info_hash(&metadata, expected_info_hash)?;
        if expected_info_hash.is_v2() && !remote_supports_v2 {
            anyhow::bail!("pure-v2 metadata peer does not advertise BEP 52 support");
        }

        v2_piece_layer_requirements_with_allocation_reservation(
            &metadata,
            &mut reserve_parser_memory,
        )
        .context("fetched metadata has an invalid v2 file tree")?
    };
    let mut piece_layers = Vec::new();
    if let Some(requirements) = requirements {
        if !requirements.files.is_empty() && !remote_supports_v2 {
            anyhow::bail!("peer does not advertise BEP 52 support for piece layers");
        }
        let layer_bytes = v2_piece_layer_bytes(&requirements.files)?;
        let additional_memory = layer_bytes
            .checked_mul(2)
            .ok_or_else(|| anyhow::anyhow!("v2 piece-layer memory estimate overflowed"))?;
        if !lease.try_grow(additional_memory) {
            anyhow::bail!("v2 piece-layer allocation of {additional_memory} bytes denied");
        }
        piece_layers = fetch_v2_piece_layers(
            addr,
            &mut peer_io,
            requirements.piece_length,
            &requirements.files,
        )
        .await?;
    }
    Ok(FetchedMetadata {
        bytes: metadata,
        piece_layers,
        _lease: lease,
    })
}

enum MetadataPeerIo {
    Tcp(Framed<TcpStream, PeerCodec>),
    Utp {
        stream: Box<UtpStream>,
        decoder: UtpFrameDecoder,
        write_buffer: UtpWireBuffer,
    },
}

impl MetadataPeerIo {
    async fn send(&mut self, msg: Message) -> anyhow::Result<()> {
        timeout(METADATA_PEER_WRITE_TIMEOUT, async {
            match self {
                MetadataPeerIo::Tcp(framed) => framed.send(msg).await.map_err(Into::into),
                MetadataPeerIo::Utp {
                    stream,
                    decoder,
                    write_buffer,
                } => {
                    let encoded = write_buffer.encode(decoder, &msg)?;
                    stream.write_all(encoded).await?;
                    Ok(())
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("metadata peer socket write timed out"))?
    }

    async fn next(&mut self) -> anyhow::Result<Option<Message>> {
        match self {
            MetadataPeerIo::Tcp(framed) => match framed.next().await {
                Some(result) => result.map(Some).map_err(Into::into),
                None => Ok(None),
            },
            MetadataPeerIo::Utp {
                stream, decoder, ..
            } => decoder.next_message(stream).await,
        }
    }
}

fn reserve_metadata_fetch_bytes(
    resources: &ResourceGovernor,
    metadata_size: u32,
) -> anyhow::Result<MemoryLease> {
    let bytes = u64::from(metadata_size).saturating_mul(2);
    resources
        .try_acquire(MemoryClass::Metadata, bytes)
        .ok_or_else(|| anyhow::anyhow!("metadata allocation of {bytes} bytes denied"))
}

fn validate_metadata_info_hash(
    metadata: &[u8],
    expected_info_hash: MetadataInfoHash,
) -> anyhow::Result<()> {
    match expected_info_hash {
        MetadataInfoHash::V1(expected) => {
            validate_metadata_v1_hash(metadata, expected)?;
        }
        MetadataInfoHash::V2(expected) => {
            validate_metadata_v2_hash(metadata, expected)?;
        }
        MetadataInfoHash::Hybrid { v1, v2 } => {
            validate_metadata_v1_hash(metadata, v1)?;
            validate_metadata_v2_hash(metadata, v2)?;
        }
    }
    Ok(())
}

fn validate_metadata_v1_hash(metadata: &[u8], expected: [u8; 20]) -> anyhow::Result<()> {
    let mut hasher = Sha1::new();
    hasher.update(metadata);
    let actual: [u8; 20] = hasher.finalize().into();
    if actual != expected {
        anyhow::bail!(
            "fetched metadata v1 infohash {} does not match expected {}",
            hex::encode(actual),
            hex::encode(expected)
        );
    }
    Ok(())
}

fn validate_metadata_v2_hash(metadata: &[u8], expected: [u8; 32]) -> anyhow::Result<()> {
    let mut hasher = Sha256::new();
    hasher.update(metadata);
    let actual: [u8; 32] = hasher.finalize().into();
    if actual != expected {
        anyhow::bail!(
            "fetched metadata v2 infohash {} does not match expected {}",
            hex::encode(actual),
            hex::encode(expected)
        );
    }
    Ok(())
}

async fn read_remote_metadata_handshake(
    addr: SocketAddr,
    peer_io: &mut MetadataPeerIo,
) -> anyhow::Result<(u8, u32)> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), peer_io.next())
            .await??
            .ok_or_else(|| anyhow::anyhow!("peer closed before extension handshake"))?;
        if let Message::Extended {
            ext_id: EXT_HANDSHAKE_ID,
            payload,
        } = msg
        {
            let handshake = ExtensionHandshake::parse(&payload)?;
            let remote_ext_id = handshake
                .ut_metadata_id()
                .ok_or_else(|| anyhow::anyhow!("peer does not advertise ut_metadata"))?;
            let metadata_size = handshake
                .metadata_size
                .ok_or_else(|| anyhow::anyhow!("peer did not send metadata_size"))?;
            if metadata_size == 0 || metadata_size > MAX_METADATA_SIZE {
                anyhow::bail!("metadata_size {metadata_size} from {addr} is invalid");
            }
            return Ok((remote_ext_id, metadata_size));
        }
    }
}

async fn read_metadata_piece(
    _addr: SocketAddr,
    peer_io: &mut MetadataPeerIo,
    local_ext_id: u8,
    expected_piece: u32,
    expected_total_size: u32,
) -> anyhow::Result<Option<Vec<u8>>> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), peer_io.next())
            .await??
            .ok_or_else(|| anyhow::anyhow!("peer closed during metadata transfer"))?;
        let Message::Extended { ext_id, payload } = msg else {
            continue;
        };
        if ext_id == EXT_HANDSHAKE_ID {
            continue;
        }
        if ext_id != local_ext_id {
            continue;
        }
        match UtMetadataMessage::parse(&payload)? {
            UtMetadataMessage::Data {
                piece,
                total_size,
                data,
            } if piece == expected_piece => {
                validate_metadata_piece(
                    expected_piece,
                    expected_total_size,
                    total_size,
                    data.len(),
                )?;
                return Ok(Some(data));
            }
            UtMetadataMessage::Reject { piece } if piece == expected_piece => return Ok(None),
            _ => {}
        }
    }
}

fn validate_metadata_piece(
    piece: u32,
    expected_total_size: u32,
    total_size: u32,
    data_len: usize,
) -> anyhow::Result<()> {
    if total_size != expected_total_size {
        anyhow::bail!(
            "metadata piece {piece} total_size {total_size} does not match expected {expected_total_size}"
        );
    }
    let expected_total_size = usize::try_from(expected_total_size)
        .map_err(|_| anyhow::anyhow!("metadata size does not fit this platform"))?;
    let start = (piece as usize)
        .checked_mul(METADATA_PIECE_SIZE)
        .ok_or_else(|| anyhow::anyhow!("metadata piece offset overflow"))?;
    if start >= expected_total_size {
        anyhow::bail!("metadata piece {piece} starts past metadata size");
    }
    let expected_len = (expected_total_size - start).min(METADATA_PIECE_SIZE);
    if data_len != expected_len {
        anyhow::bail!(
            "metadata piece {piece} length {data_len} does not match expected {expected_len}"
        );
    }
    Ok(())
}

const MAX_METADATA_HASH_REQUEST_LENGTH: u32 = 512;

fn v2_piece_layer_bytes(requirements: &[V2PieceLayerRequirement]) -> anyhow::Result<u64> {
    let mut bytes = 0u64;
    for requirement in requirements {
        if requirement.hash_count < 2 {
            anyhow::bail!("v2 layered file has fewer than two piece-layer hashes");
        }
        let requirement_bytes = u64::try_from(requirement.hash_count)
            .ok()
            .and_then(|count| count.checked_mul(32))
            .ok_or_else(|| anyhow::anyhow!("v2 piece-layer byte count overflowed"))?;
        bytes = bytes
            .checked_add(requirement_bytes)
            .ok_or_else(|| anyhow::anyhow!("v2 piece-layer byte count overflowed"))?;
    }
    // A completed magnet is persisted as a normal metainfo blob, whose
    // parser has the same 64 MiB raw-torrent ceiling. Refuse an impossible
    // layer set before opening a sequence of network requests for it.
    if bytes > rt_metainfo::MAX_TORRENT_BYTES as u64 {
        anyhow::bail!(
            "v2 piece layers require {bytes} bytes, above the {}-byte metainfo limit",
            rt_metainfo::MAX_TORRENT_BYTES
        );
    }
    Ok(bytes)
}

fn v2_piece_layer_shape(
    requirement: &V2PieceLayerRequirement,
    piece_length: u64,
) -> anyhow::Result<(u32, u32, u64)> {
    if piece_length < V2_BLOCK_SIZE as u64
        || !piece_length.is_power_of_two()
        || !piece_length.is_multiple_of(V2_BLOCK_SIZE as u64)
    {
        anyhow::bail!("invalid v2 piece length {piece_length} for hash exchange");
    }
    let blocks_per_piece = piece_length / V2_BLOCK_SIZE as u64;
    let base_layer = blocks_per_piece.trailing_zeros();
    let leaf_count = requirement
        .file_length
        .checked_add(V2_BLOCK_SIZE as u64 - 1)
        .ok_or_else(|| anyhow::anyhow!("v2 leaf count overflowed"))?
        / V2_BLOCK_SIZE as u64;
    let leaf_count = leaf_count.max(1);
    let padded_leaf_count = leaf_count
        .checked_next_power_of_two()
        .ok_or_else(|| anyhow::anyhow!("v2 Merkle tree is too large"))?;
    let tree_height = padded_leaf_count.trailing_zeros();
    if base_layer >= tree_height {
        anyhow::bail!("v2 piece layer base {base_layer} is not below tree height {tree_height}");
    }
    let layer_len = padded_leaf_count
        .checked_shr(base_layer)
        .ok_or_else(|| anyhow::anyhow!("v2 piece-layer width overflowed"))?;
    let hash_count = u64::try_from(requirement.hash_count)
        .map_err(|_| anyhow::anyhow!("v2 piece-layer count does not fit in u64"))?;
    if hash_count == 0 || hash_count > layer_len {
        anyhow::bail!("v2 piece-layer count {hash_count} exceeds padded layer width {layer_len}");
    }
    let proof_layers = tree_height
        .checked_sub(base_layer + 1)
        .ok_or_else(|| anyhow::anyhow!("v2 proof-layer count underflowed"))?;
    Ok((base_layer, proof_layers, layer_len))
}

fn v2_hash_request_length(remaining: usize) -> anyhow::Result<u32> {
    let length = if remaining > MAX_METADATA_HASH_REQUEST_LENGTH as usize {
        MAX_METADATA_HASH_REQUEST_LENGTH
    } else {
        u32::try_from(remaining.next_power_of_two().max(2))
            .map_err(|_| anyhow::anyhow!("v2 piece-layer request length overflowed"))?
    };
    Ok(length)
}

async fn fetch_v2_piece_layers(
    addr: SocketAddr,
    peer_io: &mut MetadataPeerIo,
    piece_length: u64,
    requirements: &[V2PieceLayerRequirement],
) -> anyhow::Result<Vec<([u8; 32], Vec<[u8; 32]>)>> {
    let mut layers = Vec::new();
    layers
        .try_reserve_exact(requirements.len())
        .map_err(|error| anyhow::anyhow!("v2 piece-layer result allocation failed: {error}"))?;
    for requirement in requirements {
        let layer = fetch_v2_piece_layer(addr, peer_io, piece_length, requirement).await?;
        layers.push((requirement.pieces_root, layer));
    }
    Ok(layers)
}

async fn fetch_v2_piece_layer(
    addr: SocketAddr,
    peer_io: &mut MetadataPeerIo,
    piece_length: u64,
    requirement: &V2PieceLayerRequirement,
) -> anyhow::Result<Vec<[u8; 32]>> {
    let (base_layer, proof_layers, layer_len) = v2_piece_layer_shape(requirement, piece_length)?;
    let mut layer = Vec::new();
    layer
        .try_reserve_exact(requirement.hash_count)
        .map_err(|error| anyhow::anyhow!("v2 piece-layer allocation failed: {error}"))?;
    let mut cursor = 0usize;
    while cursor < requirement.hash_count {
        let remaining = requirement.hash_count - cursor;
        let request_length = v2_hash_request_length(remaining)?;
        let request_index = u32::try_from(cursor)
            .map_err(|_| anyhow::anyhow!("v2 piece-layer request index overflowed"))?;
        if request_index % request_length != 0
            || u64::from(request_index) + u64::from(request_length) > layer_len
        {
            anyhow::bail!(
                "v2 piece-layer request index {request_index} and length {request_length} exceed layer width {layer_len}"
            );
        }
        let request = Message::HashRequest {
            pieces_root: requirement.pieces_root,
            base_layer,
            index: request_index,
            length: request_length,
            proof_layers,
        };
        peer_io.send(request).await?;
        let hashes = read_v2_hashes_response(
            addr,
            peer_io,
            requirement.pieces_root,
            base_layer,
            request_index,
            request_length,
            proof_layers,
            requirement.file_length,
            piece_length,
        )
        .await?;
        let take = remaining.min(request_length as usize);
        layer.extend_from_slice(&hashes[..take]);
        cursor += take;
        if take < request_length as usize {
            break;
        }
    }
    if layer.len() != requirement.hash_count || merkle_root(&layer) != requirement.pieces_root {
        anyhow::bail!(
            "v2 piece layer for {} does not reconstruct its pieces root",
            hex::encode(requirement.pieces_root)
        );
    }
    Ok(layer)
}

#[allow(clippy::too_many_arguments)]
async fn read_v2_hashes_response(
    addr: SocketAddr,
    peer_io: &mut MetadataPeerIo,
    pieces_root: [u8; 32],
    base_layer: u32,
    index: u32,
    length: u32,
    proof_layers: u32,
    file_length: u64,
    piece_length: u64,
) -> anyhow::Result<Vec<[u8; 32]>> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), peer_io.next())
            .await??
            .ok_or_else(|| anyhow::anyhow!("peer closed during v2 piece-layer transfer"))?;
        match msg {
            Message::Hashes {
                pieces_root: response_root,
                base_layer: response_base,
                index: response_index,
                length: response_length,
                proof_layers: response_proof,
                hashes,
            } => {
                if (
                    response_root,
                    response_base,
                    response_index,
                    response_length,
                    response_proof,
                ) != (pieces_root, base_layer, index, length, proof_layers)
                {
                    anyhow::bail!("peer returned a mismatched v2 hashes response from {addr}");
                }
                validate_v2_hashes_response(
                    pieces_root,
                    base_layer,
                    index,
                    length,
                    proof_layers,
                    file_length,
                    piece_length,
                    &hashes,
                )?;
                return Ok(hashes);
            }
            Message::HashReject {
                pieces_root: response_root,
                base_layer: response_base,
                index: response_index,
                length: response_length,
                proof_layers: response_proof,
            } if (
                response_root,
                response_base,
                response_index,
                response_length,
                response_proof,
            ) == (pieces_root, base_layer, index, length, proof_layers) =>
            {
                anyhow::bail!("peer rejected v2 piece-layer request from {addr}");
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_v2_hashes_response(
    pieces_root: [u8; 32],
    base_layer: u32,
    index: u32,
    length: u32,
    proof_layers: u32,
    file_length: u64,
    piece_length: u64,
    hashes: &[[u8; 32]],
) -> anyhow::Result<()> {
    if length < 2
        || !length.is_power_of_two()
        || !index.is_multiple_of(length)
        || length > MAX_METADATA_HASH_REQUEST_LENGTH
    {
        anyhow::bail!("peer returned invalid v2 hash request coordinates");
    }
    let blocks_per_piece = piece_length
        .checked_div(V2_BLOCK_SIZE as u64)
        .filter(|blocks| blocks.is_power_of_two())
        .ok_or_else(|| anyhow::anyhow!("invalid v2 piece length in hash response"))?;
    let expected_base = blocks_per_piece.trailing_zeros();
    if base_layer != expected_base {
        anyhow::bail!("peer returned an unexpected v2 hash base layer");
    }
    let leaf_count = file_length
        .checked_add(V2_BLOCK_SIZE as u64 - 1)
        .ok_or_else(|| anyhow::anyhow!("v2 leaf count overflowed"))?
        / V2_BLOCK_SIZE as u64;
    let padded_leaf_count = leaf_count
        .max(1)
        .checked_next_power_of_two()
        .ok_or_else(|| anyhow::anyhow!("v2 Merkle tree is too large"))?;
    let tree_height = padded_leaf_count.trailing_zeros();
    let layer_len = padded_leaf_count
        .checked_shr(base_layer)
        .ok_or_else(|| anyhow::anyhow!("v2 hash base layer is too large"))?;
    let end = u64::from(index)
        .checked_add(u64::from(length))
        .ok_or_else(|| anyhow::anyhow!("v2 hash range overflowed"))?;
    if base_layer >= tree_height || end > layer_len {
        anyhow::bail!("peer returned v2 hash coordinates outside the file tree");
    }
    let expected_proof_layers = tree_height
        .checked_sub(base_layer + 1)
        .ok_or_else(|| anyhow::anyhow!("v2 proof-layer count underflowed"))?;
    if proof_layers != expected_proof_layers {
        anyhow::bail!("peer returned an incomplete v2 hash proof");
    }
    let omitted_layers = length.trailing_zeros();
    let proof_hash_count = if omitted_layers <= proof_layers {
        usize::try_from(proof_layers - omitted_layers + 1)
            .map_err(|_| anyhow::anyhow!("v2 proof hash count does not fit usize"))?
    } else {
        0
    };
    let requested_hash_count = usize::try_from(length)
        .map_err(|_| anyhow::anyhow!("v2 requested hash count does not fit usize"))?;
    let expected_hash_count = requested_hash_count
        .checked_add(proof_hash_count)
        .ok_or_else(|| anyhow::anyhow!("v2 response hash count overflowed"))?;
    if hashes.len() != expected_hash_count {
        anyhow::bail!(
            "v2 hashes response contains {}, expected {expected_hash_count}",
            hashes.len()
        );
    }

    let mut nodes = hashes[..requested_hash_count].to_vec();
    while nodes.len() > 1 {
        let mut next = Vec::with_capacity(nodes.len() / 2);
        for pair in nodes.as_chunks::<2>().0 {
            next.push(hash_pair(pair[0], pair[1]));
        }
        nodes = next;
    }
    let mut current = nodes[0];
    for (offset, sibling) in hashes[requested_hash_count..].iter().enumerate() {
        let depth = omitted_layers
            .checked_add(
                u32::try_from(offset).map_err(|_| anyhow::anyhow!("v2 proof offset overflowed"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("v2 proof depth overflowed"))?;
        let node_index = u64::from(index) >> depth;
        current = if node_index & 1 == 0 {
            hash_pair(current, *sibling)
        } else {
            hash_pair(*sibling, current)
        };
    }
    if current != pieces_root {
        anyhow::bail!("v2 hashes response proof does not match pieces root");
    }
    Ok(())
}

fn build_torrent_from_info(
    info: &[u8],
    trackers: &[String],
    piece_layers: &[([u8; 32], Vec<[u8; 32]>)],
) -> Result<Vec<u8>, String> {
    let mut sorted_layers = piece_layers.iter().collect::<Vec<_>>();
    sorted_layers.sort_by_key(|(root, _)| *root);
    for pair in sorted_layers.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err("duplicate v2 piece-layer root".to_owned());
        }
    }
    let piece_layers_capacity = if sorted_layers.is_empty() {
        0
    } else {
        let entries = sorted_layers
            .iter()
            .try_fold(0usize, |total, (root, hashes)| {
                let hash_bytes = hashes
                    .len()
                    .checked_mul(32)
                    .ok_or_else(|| "v2 piece-layer byte count overflowed".to_owned())?;
                total
                    .checked_add(bencoded_bytes_len(root.len()))
                    .and_then(|value| value.checked_add(bencoded_bytes_len(hash_bytes)))
                    .ok_or_else(|| "v2 piece-layer encoding size overflowed".to_owned())
            })?;
        bencoded_bytes_len(b"piece layers".len())
            .checked_add(1)
            .and_then(|value| value.checked_add(entries))
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "v2 piece-layer encoding size overflowed".to_owned())?
    };
    let capacity = 1usize
        .saturating_add(metadata_completion_tracker_bytes(trackers))
        .saturating_add(bencoded_bytes_len(b"info".len()))
        .saturating_add(info.len())
        .saturating_add(piece_layers_capacity)
        .saturating_add(1);
    if capacity > rt_metainfo::MAX_TORRENT_BYTES {
        return Err(format!(
            "raw metainfo requires {capacity} bytes, above the {}-byte limit",
            rt_metainfo::MAX_TORRENT_BYTES
        ));
    }
    let mut out = Vec::new();
    out.try_reserve(capacity)
        .map_err(|error| format!("raw metainfo allocation of {capacity} bytes failed: {error}"))?;
    out.push(b'd');
    if let Some(first) = trackers.first() {
        write_bytes_key(&mut out, b"announce");
        write_bytes(&mut out, first.as_bytes());
        write_bytes_key(&mut out, b"announce-list");
        out.push(b'l');
        for tracker in trackers {
            out.push(b'l');
            write_bytes(&mut out, tracker.as_bytes());
            out.push(b'e');
        }
        out.push(b'e');
    }
    write_bytes_key(&mut out, b"info");
    out.extend_from_slice(info);
    if !sorted_layers.is_empty() {
        write_bytes_key(&mut out, b"piece layers");
        out.push(b'd');
        for (root, hashes) in sorted_layers {
            write_bytes(&mut out, root);
            let hash_bytes = hashes
                .len()
                .checked_mul(32)
                .ok_or_else(|| "v2 piece-layer byte count overflowed".to_owned())?;
            write_bytes_len(&mut out, hash_bytes);
            for hash in hashes {
                out.extend_from_slice(hash);
            }
        }
        out.push(b'e');
    }
    out.push(b'e');
    debug_assert_eq!(out.len(), capacity);
    Ok(out)
}

fn write_bytes_key(out: &mut Vec<u8>, key: &[u8]) {
    write_bytes(out, key);
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_bytes_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn write_bytes_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(len.to_string().as_bytes());
    out.push(b':');
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_bencode::{encode, BValue};
    use rt_utp::{UtpListener, UtpTransportConfig};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn empty_registry() -> Arc<RwLock<SessionRegistry>> {
        Arc::new(RwLock::new(SessionRegistry::new()))
    }

    #[test]
    fn validates_metadata_piece_lengths() {
        validate_metadata_piece(0, 20_000, 20_000, METADATA_PIECE_SIZE).unwrap();
        validate_metadata_piece(1, 20_000, 20_000, 20_000 - METADATA_PIECE_SIZE).unwrap();

        assert!(validate_metadata_piece(0, 20_000, 19_999, METADATA_PIECE_SIZE).is_err());
        assert!(validate_metadata_piece(1, 20_000, 20_000, METADATA_PIECE_SIZE).is_err());
        assert!(validate_metadata_piece(2, 20_000, 20_000, 1).is_err());
        assert!(validate_metadata_piece(u32::MAX, 20_000, 20_000, 1).is_err());
    }

    #[test]
    fn validates_metadata_info_hash() {
        let info = b"d4:name4:teste";
        let mut hasher = Sha1::new();
        hasher.update(info);
        let expected: [u8; 20] = hasher.finalize().into();

        validate_metadata_info_hash(info, MetadataInfoHash::V1(expected)).unwrap();
        assert!(validate_metadata_info_hash(info, MetadataInfoHash::V1([0; 20])).is_err());
    }

    #[test]
    fn validates_both_hashes_for_hybrid_metadata() {
        let info = b"d4:name4:teste";
        let mut sha1 = Sha1::new();
        sha1.update(info);
        let v1: [u8; 20] = sha1.finalize().into();
        let mut sha256 = Sha256::new();
        sha256.update(info);
        let v2: [u8; 32] = sha256.finalize().into();
        let identity = MetadataInfoHash::Hybrid { v1, v2 };

        validate_metadata_info_hash(info, identity).unwrap();
        assert!(
            validate_metadata_info_hash(info, MetadataInfoHash::Hybrid { v1: [0; 20], v2 })
                .is_err()
        );
        assert!(
            validate_metadata_info_hash(info, MetadataInfoHash::Hybrid { v1, v2: [0; 32] })
                .is_err()
        );
    }

    #[test]
    fn restores_hybrid_identity_from_v1_key_and_persisted_v2_hash() {
        let v1 = [0x31; 20];
        let v2 = [0x72; 32];
        let identity = metadata_info_hash_with_v2(&hex::encode(v1), Some(v2)).unwrap();
        assert_eq!(identity, MetadataInfoHash::Hybrid { v1, v2 });
        assert_eq!(identity.wire_hash(), v1);
        assert!(metadata_info_hash_with_v2(&hex::encode(v2), Some([0; 32])).is_err());
    }

    #[test]
    fn metadata_peer_retry_has_cooldown() {
        let peer: SocketAddr = "127.0.0.1:6881".parse().unwrap();
        let now = Instant::now();
        let mut attempts = HashMap::new();

        assert!(should_retry_peer(&mut attempts, peer, now));
        assert!(!should_retry_peer(
            &mut attempts,
            peer,
            now + Duration::from_secs(10)
        ));
        assert!(should_retry_peer(
            &mut attempts,
            peer,
            now + METADATA_PEER_RETRY_AFTER + Duration::from_secs(1)
        ));
    }

    #[tokio::test]
    async fn paused_metadata_reannounce_does_not_resume_tracker_work() {
        let paused = true;
        let mut tracker_event = TrackerEvent::Started;
        let mut tracker_tick = interval(Duration::from_secs(60));

        request_metadata_reannounce(paused, &mut tracker_event, &mut tracker_tick);

        assert!(paused);
        assert_eq!(tracker_event, TrackerEvent::Started);
    }

    #[test]
    fn metadata_storage_quiesce_sets_paused_and_reports_previous_state() {
        let mut paused = false;

        assert!(!quiesce_metadata_task(&mut paused));
        assert!(paused);
        assert!(quiesce_metadata_task(&mut paused));
    }

    #[test]
    fn metadata_fetch_candidates_are_bounded_and_prune_retry_history() {
        let now = Instant::now();
        let egress_policy = OutboundEgressPolicy {
            allow_loopback: true,
            ..OutboundEgressPolicy::default()
        };
        let mut attempts = HashMap::from([
            (
                "127.0.0.1:6881".parse().unwrap(),
                now - METADATA_PEER_RETRY_AFTER - Duration::from_secs(1),
            ),
            ("127.0.0.2:6881".parse().unwrap(), now),
        ]);
        let peers = vec![
            "127.0.0.1:6881".parse().unwrap(),
            "127.0.0.2:6881".parse().unwrap(),
            "127.0.0.3:6881".parse().unwrap(),
            "127.0.0.4:6881".parse().unwrap(),
        ];

        let registry = SessionRegistry::new();
        let candidates =
            metadata_fetch_candidates(peers, &egress_policy, &registry, &mut attempts, now, 2, 2);

        assert_eq!(candidates.len(), 2);
        assert!(candidates.contains(&"127.0.0.1:6881".parse().unwrap()));
        assert!(candidates.contains(&"127.0.0.3:6881".parse().unwrap()));
        assert_eq!(attempts.len(), 2);
        assert!(attempts.contains_key(&"127.0.0.3:6881".parse().unwrap()));
    }

    #[test]
    fn metadata_fetch_candidates_skip_denied_addresses_without_retry_state() {
        let now = Instant::now();
        let policy = OutboundEgressPolicy::default();
        let loopback = "127.0.0.1:6881".parse().unwrap();
        let private = "10.0.0.2:6881".parse().unwrap();
        let public = "8.8.8.8:6881".parse().unwrap();
        let mut attempts = HashMap::new();
        let registry = SessionRegistry::new();

        let candidates = metadata_fetch_candidates(
            vec![loopback, private, public],
            &policy,
            &registry,
            &mut attempts,
            now,
            8,
            8,
        );

        assert_eq!(candidates.into_iter().collect::<Vec<_>>(), vec![public]);
        assert_eq!(attempts.len(), 1);
        assert!(attempts.contains_key(&public));
        assert!(!attempts.contains_key(&loopback));
        assert!(!attempts.contains_key(&private));
    }

    #[test]
    fn metadata_fetch_candidates_skip_banned_peers_without_retry_state() {
        let now = Instant::now();
        let policy = OutboundEgressPolicy::default();
        let banned = "8.8.8.8:6881".parse().unwrap();
        let allowed = "8.8.4.4:6881".parse().unwrap();
        let mut registry = SessionRegistry::new();
        assert_eq!(registry.ban_peers([banned]), 1);
        let mut attempts = HashMap::new();

        let candidates = metadata_fetch_candidates(
            vec![banned, allowed],
            &policy,
            &registry,
            &mut attempts,
            now,
            8,
            8,
        );

        assert_eq!(candidates.into_iter().collect::<Vec<_>>(), vec![allowed]);
        assert_eq!(attempts.len(), 1);
        assert!(attempts.contains_key(&allowed));
        assert!(!attempts.contains_key(&banned));
    }

    #[tokio::test]
    async fn metadata_fetch_attempt_rejects_loopback_before_connecting() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = listener.local_addr().unwrap();
        let resources = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default());

        let (attempted_peer, result) = timeout(
            Duration::from_millis(100),
            metadata_fetch_attempt(
                peer,
                MetadataInfoHash::V1([0; 20]),
                resources,
                GlobalNetworkBudget::unlimited(),
                empty_registry(),
                OutboundEgressPolicy::default(),
            ),
        )
        .await
        .expect("policy rejection should not wait on a peer handshake");

        assert_eq!(attempted_peer, peer);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("metadata peer denied by egress policy"));
        assert!(timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn metadata_fetch_attempt_rejects_banned_peer_before_connecting() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = listener.local_addr().unwrap();
        let registry = empty_registry();
        registry.write().await.ban_peers([peer]);
        let resources = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default());
        let policy = OutboundEgressPolicy {
            allow_loopback: true,
            ..OutboundEgressPolicy::default()
        };

        let (attempted_peer, result) = timeout(
            Duration::from_millis(100),
            metadata_fetch_attempt(
                peer,
                MetadataInfoHash::V1([0; 20]),
                resources,
                GlobalNetworkBudget::unlimited(),
                registry,
                policy,
            ),
        )
        .await
        .expect("ban rejection should not wait on a peer handshake");

        assert_eq!(attempted_peer, peer);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("metadata peer is banned"));
        assert!(timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err());
    }

    #[test]
    fn metadata_attempt_cache_cap_scales_with_peer_limit() {
        assert_eq!(
            metadata_peer_attempt_cache_cap(1),
            METADATA_PEER_ATTEMPT_CACHE_MIN
        );
        assert_eq!(metadata_peer_attempt_cache_cap(100), 400);
        assert_eq!(
            metadata_peer_attempt_cache_cap(usize::MAX),
            MAX_TRACKER_PEERS
        );
        assert_eq!(
            metadata_peer_candidate_cap(1),
            MAX_METADATA_FETCH_CONCURRENCY
        );
        assert_eq!(metadata_peer_candidate_cap(100), 100);
        assert_eq!(metadata_peer_candidate_cap(usize::MAX), MAX_TRACKER_PEERS);
    }

    #[test]
    fn metadata_tracker_peer_collection_reservation_scales_with_peer_limit() {
        let bytes = metadata_tracker_peer_collection_bytes(MAX_TRACKER_PEERS);
        assert_eq!(bytes, 4 * 1024 * 1024);
        assert_eq!(metadata_tracker_peer_collection_bytes(0), 0);

        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::TrackerPeers as usize] = bytes;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: bytes,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let lease = governor
            .try_acquire(MemoryClass::TrackerPeers, bytes)
            .expect("the bounded collection should fit its exact allowance");
        assert!(governor.try_acquire(MemoryClass::TrackerPeers, 1).is_none());
        drop(lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::TrackerPeers as usize].used_bytes,
            0
        );
    }

    #[test]
    fn metadata_completion_tracker_prefix_is_preallocated_and_accounted() {
        let trackers = vec![
            "https://tracker.example/announce".to_owned(),
            "udp://tracker.example:6969".to_owned(),
        ];
        let info = b"d4:name4:test6:lengthi1ee";
        let raw = build_torrent_from_info(info, &trackers, &[]).unwrap();
        let tracker_prefix = metadata_completion_tracker_bytes(&trackers);
        let expected_len = 1 + tracker_prefix + bencoded_bytes_len(b"info".len()) + info.len() + 1;

        assert_eq!(raw.len(), expected_len);
        assert!(raw.capacity() >= raw.len());
        assert_eq!(metadata_completion_tracker_bytes(&[]), 0);
        assert_eq!(decimal_digits(0), 1);
        assert_eq!(decimal_digits(9), 1);
        assert_eq!(decimal_digits(10), 2);
    }

    #[test]
    fn metadata_fetch_reservation_uses_metadata_governor_class() {
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::Metadata as usize] = 128;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 128,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let lease = reserve_metadata_fetch_bytes(&governor, 64).unwrap();
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            128
        );
        drop(lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            0
        );
        assert!(reserve_metadata_fetch_bytes(&governor, 65).is_err());
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].denied_allocations,
            1
        );
    }

    #[test]
    fn metadata_task_reservation_covers_tracker_and_retry_state() {
        let trackers =
            compact_metadata_task_trackers(vec!["https://tracker.example/announce".to_owned()]);
        let bytes = metadata_task_memory_bytes(&trackers, trackers.capacity(), 0, 100) as u64;
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::Metadata as usize] = bytes;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: bytes,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let (trackers, memory) = prepare_metadata_task_memory(&governor, trackers, 100).unwrap();
        assert_eq!(trackers.len(), 1);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            bytes
        );
        drop(memory);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            0
        );
    }

    #[test]
    fn direct_peer_hash_exchange_requests_never_use_invalid_length_one() {
        assert_eq!(v2_hash_request_length(1).unwrap(), 2);
        assert_eq!(v2_hash_request_length(2).unwrap(), 2);
        assert_eq!(v2_hash_request_length(3).unwrap(), 4);
        assert_eq!(
            v2_hash_request_length(MAX_METADATA_HASH_REQUEST_LENGTH as usize).unwrap(),
            MAX_METADATA_HASH_REQUEST_LENGTH
        );
        assert_eq!(
            v2_hash_request_length(MAX_METADATA_HASH_REQUEST_LENGTH as usize + 1).unwrap(),
            MAX_METADATA_HASH_REQUEST_LENGTH
        );
    }

    #[test]
    fn v2_hash_exchange_proof_is_authenticated_and_tamper_resistant() {
        let leaves = (0u8..8)
            .map(|value| rt_hash::BlockHash::of(&[value]).0)
            .collect::<Vec<_>>();
        let layer_one = leaves
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| hash_pair(pair[0], pair[1]))
            .collect::<Vec<_>>();
        let layer_two = layer_one
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| hash_pair(pair[0], pair[1]))
            .collect::<Vec<_>>();
        let pieces_root = merkle_root(&leaves);
        let response = vec![leaves[0], leaves[1], layer_one[1], layer_two[1]];
        validate_v2_hashes_response(
            pieces_root,
            0,
            0,
            2,
            2,
            8 * V2_BLOCK_SIZE as u64,
            V2_BLOCK_SIZE as u64,
            &response,
        )
        .unwrap();

        let mut tampered = response;
        tampered[3][0] ^= 1;
        assert!(validate_v2_hashes_response(
            pieces_root,
            0,
            0,
            2,
            2,
            8 * V2_BLOCK_SIZE as u64,
            V2_BLOCK_SIZE as u64,
            &tampered,
        )
        .is_err());
    }

    #[tokio::test]
    async fn metadata_task_pause_interrupts_incoming_peer_fetch() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(peer_addr).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let resources = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default());
        let (_, task_memory) = prepare_metadata_task_memory(&resources, Vec::new(), 8).unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (engine_tx, _engine_rx) = mpsc::channel(8);
        let task = tokio::spawn(run_metadata_task(
            MetadataInfoHash::V1([0; 20]),
            "00".repeat(40),
            Vec::new(),
            task_memory,
            cmd_rx,
            engine_tx,
            cmd_tx.clone(),
            empty_registry(),
            resources,
            6881,
            8,
            1,
            1,
            false,
            OutboundEgressPolicy::default(),
            GlobalNetworkBudget::unlimited(),
        ));

        cmd_tx
            .send(TorrentCmd::AcceptPeer {
                stream: server,
                peer_addr,
                handshake: Handshake {
                    info_hash: [0; 20],
                    peer_id: [b'P'; 20],
                    reserved: ExtensionFlags::with_extension_protocol(),
                },
                peer_permit: Arc::new(tokio::sync::Semaphore::new(1))
                    .acquire_owned()
                    .await
                    .unwrap(),
            })
            .await
            .unwrap();

        let mut client = client;
        let mut handshake = [0u8; 68];
        client.read_exact(&mut handshake).await.unwrap();

        let (pause_reply, pause_result) = tokio::sync::oneshot::channel();
        cmd_tx
            .send(TorrentCmd::Pause {
                reply: Some(pause_reply),
            })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), pause_result)
                .await
                .unwrap()
                .unwrap(),
            Ok(())
        );

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .expect("metadata task should stop after an interrupted fetch")
            .unwrap();
    }

    #[tokio::test]
    async fn metadata_task_stops_fetching_when_global_peer_is_banned() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let resources = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default());
        let (_, task_memory) =
            prepare_metadata_task_memory_with_peers(&resources, Vec::new(), vec![peer_addr], 8)
                .unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (engine_tx, _engine_rx) = mpsc::channel(8);
        let registry = empty_registry();
        let task = tokio::spawn(run_metadata_task(
            MetadataInfoHash::V1([0; 20]),
            "00".repeat(40),
            Vec::new(),
            task_memory,
            cmd_rx,
            engine_tx,
            cmd_tx.clone(),
            Arc::clone(&registry),
            resources,
            6881,
            8,
            1,
            1,
            false,
            OutboundEgressPolicy {
                allow_loopback: true,
                ..OutboundEgressPolicy::default()
            },
            GlobalNetworkBudget::unlimited(),
        ));

        let (mut peer_stream, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("metadata task should try the permitted direct peer")
            .unwrap();
        let mut handshake = [0u8; 68];
        timeout(
            Duration::from_secs(1),
            peer_stream.read_exact(&mut handshake),
        )
        .await
        .expect("metadata task should send its peer handshake")
        .unwrap();

        registry.write().await.ban_peers([peer_addr]);
        cmd_tx.send(TorrentCmd::EvictBannedPeers).await.unwrap();
        let mut trailing = [0u8; 1];
        assert_eq!(
            timeout(Duration::from_secs(1), peer_stream.read(&mut trailing))
                .await
                .expect("ban eviction should cancel the active metadata fetch")
                .unwrap(),
            0
        );
        assert!(timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err());

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .expect("metadata task should stop after shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn metadata_task_pause_acknowledges_before_slow_stopped_announce() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tracker_addr = listener.local_addr().unwrap();
        let (tracker_started_tx, tracker_started_rx) = tokio::sync::oneshot::channel();
        let tracker = tokio::spawn(async move {
            let mut tracker_started_tx = Some(tracker_started_tx);
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                if let Some(started) = tracker_started_tx.take() {
                    let _ = started.send(());
                }
                let mut request = [0_u8; 1];
                let _ = stream.read(&mut request).await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        let tracker_url = format!("http://{tracker_addr}/announce");
        let resources = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default());
        let (trackers, task_memory) =
            prepare_metadata_task_memory(&resources, vec![tracker_url], 8).unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (engine_tx, _engine_rx) = mpsc::channel(8);
        let task = tokio::spawn(run_metadata_task(
            MetadataInfoHash::V1([0; 20]),
            "00".repeat(40),
            trackers,
            task_memory,
            cmd_rx,
            engine_tx,
            cmd_tx.clone(),
            empty_registry(),
            resources,
            6881,
            8,
            60,
            1,
            false,
            OutboundEgressPolicy {
                allow_loopback: true,
                ..OutboundEgressPolicy::default()
            },
            GlobalNetworkBudget::unlimited(),
        ));

        timeout(Duration::from_secs(1), tracker_started_rx)
            .await
            .expect("metadata task should start the tracker announce")
            .expect("tracker test server should signal its first request");

        let (pause_reply, pause_result) = tokio::sync::oneshot::channel();
        cmd_tx
            .send(TorrentCmd::Pause {
                reply: Some(pause_reply),
            })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), pause_result)
                .await
                .expect("pause should not wait for the stopped tracker announce")
                .unwrap(),
            Ok(())
        );

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown should interrupt the stopped tracker announce")
            .unwrap();
        tracker.abort();
    }

    #[tokio::test]
    async fn metadata_shutdown_rejects_command_interrupting_stopped_announce() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tracker_addr = listener.local_addr().unwrap();
        let (tracker_started_tx, mut tracker_started_rx) = mpsc::channel(2);
        let tracker = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let _ = tracker_started_tx.send(()).await;
                let mut request = [0_u8; 1];
                let _ = stream.read(&mut request).await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        let tracker_url = format!("http://{tracker_addr}/announce");
        let resources = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default());
        let (trackers, task_memory) =
            prepare_metadata_task_memory(&resources, vec![tracker_url], 8).unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (engine_tx, _engine_rx) = mpsc::channel(8);
        let task = tokio::spawn(run_metadata_task(
            MetadataInfoHash::V1([0; 20]),
            "00".repeat(40),
            trackers,
            task_memory,
            cmd_rx,
            engine_tx,
            cmd_tx.clone(),
            empty_registry(),
            resources,
            6881,
            8,
            60,
            1,
            false,
            OutboundEgressPolicy {
                allow_loopback: true,
                ..OutboundEgressPolicy::default()
            },
            GlobalNetworkBudget::unlimited(),
        ));

        timeout(Duration::from_secs(1), tracker_started_rx.recv())
            .await
            .unwrap()
            .expect("metadata task should start the initial tracker announce");
        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        tokio::task::yield_now().await;
        let (pause_reply, pause_result) = tokio::sync::oneshot::channel();
        cmd_tx
            .send(TorrentCmd::Pause {
                reply: Some(pause_reply),
            })
            .await
            .unwrap();

        assert_eq!(
            timeout(Duration::from_secs(1), pause_result)
                .await
                .unwrap()
                .unwrap(),
            Err("metadata task is shutting down".to_owned())
        );
        timeout(Duration::from_secs(1), task)
            .await
            .expect("metadata task should stop after rejecting the queued command")
            .unwrap();
        tracker.abort();
    }

    #[tokio::test]
    async fn dht_only_peer_candidates_can_complete_magnet_metadata() {
        let info =
            b"d6:lengthi4e4:name4:test12:piece lengthi16384e6:pieces20:abcdefghijklmnopqrste";
        let mut hasher = Sha1::new();
        hasher.update(info);
        let info_hash: [u8; 20] = hasher.finalize().into();
        let info_hash_hex = hex::encode(info_hash);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let info_for_peer = info.to_vec();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut handshake = [0_u8; 68];
            stream.read_exact(&mut handshake).await.unwrap();
            let remote = Handshake::parse(&handshake).unwrap();
            let response = Handshake {
                info_hash: remote.info_hash,
                peer_id: [b'P'; 20],
                reserved: ExtensionFlags::with_extension_protocol(),
            };
            stream.write_all(&response.encode()).await.unwrap();

            let mut framed = Framed::new(stream, PeerCodec::default());
            framed
                .send(Message::Extended {
                    ext_id: EXT_HANDSHAKE_ID,
                    payload: ExtensionHandshake::new(Some(info_for_peer.len() as u32))
                        .with_ut_metadata(7)
                        .encode(),
                })
                .await
                .unwrap();
            while let Some(message) = framed.next().await {
                let Message::Extended { ext_id: 7, payload } = message.unwrap() else {
                    continue;
                };
                let UtMetadataMessage::Request { piece } =
                    UtMetadataMessage::parse(&payload).unwrap()
                else {
                    continue;
                };
                let start = piece as usize * METADATA_PIECE_SIZE;
                let end = (start + METADATA_PIECE_SIZE).min(info_for_peer.len());
                framed
                    .send(Message::Extended {
                        ext_id: LOCAL_UT_METADATA_ID,
                        payload: UtMetadataMessage::Data {
                            piece,
                            total_size: info_for_peer.len() as u32,
                            data: info_for_peer[start..end].to_vec(),
                        }
                        .encode(),
                    })
                    .await
                    .unwrap();
                break;
            }
        });
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::Metadata as usize] = 1024 * 1024;
        caps[MemoryClass::PeerBuffer as usize] = 64 * 1024;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 1024 * 1024,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });
        let (engine_tx, mut engine_rx) = mpsc::channel(1);
        let (task_tx, _task_rx) = mpsc::channel(1);
        let registry = empty_registry();
        let mut attempts = HashMap::new();

        assert!(
            try_fetch_from_peers(
                MetadataInfoHash::V1(info_hash),
                &info_hash_hex,
                &[],
                vec![peer_addr],
                8,
                &OutboundEgressPolicy {
                    allow_loopback: true,
                    ..OutboundEgressPolicy::default()
                },
                &mut attempts,
                &engine_tx,
                &task_tx,
                &registry,
                &governor,
                &GlobalNetworkBudget::unlimited(),
            )
            .await
        );
        let cmd = engine_rx.recv().await.unwrap();
        match cmd {
            EngineCmd::CompleteMagnet { info_hash, raw, .. } => {
                assert_eq!(info_hash, info_hash_hex);
                let parsed = rt_metainfo::parse_torrent(&raw).unwrap();
                assert_eq!(parsed.name(), "test");
            }
            other => panic!("unexpected engine command: {other:?}"),
        }
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn pure_v2_magnet_fetches_and_verifies_piece_layers_from_direct_peer() {
        let piece_length = 16 * 1024i64;
        let file_length = piece_length * 3;
        let piece_hashes = (0u8..3)
            .map(|value| rt_hash::BlockHash::of(&[value]).0)
            .collect::<Vec<_>>();
        let pieces_root = rt_hash::merkle_root(&piece_hashes);
        let leaf = BValue::Dict(vec![
            (b"length".as_slice(), BValue::Int(file_length)),
            (b"pieces root".as_slice(), BValue::Bytes(&pieces_root)),
        ]);
        let file_node = BValue::Dict(vec![(b"".as_slice(), leaf)]);
        let file_tree = BValue::Dict(vec![(b"payload.bin".as_slice(), file_node)]);
        let info = encode(&BValue::Dict(vec![
            (b"file tree".as_slice(), file_tree),
            (b"meta version".as_slice(), BValue::Int(2)),
            (b"name".as_slice(), BValue::Bytes(b"v2-test")),
            (b"piece length".as_slice(), BValue::Int(piece_length)),
        ]));
        let mut hasher = Sha256::new();
        hasher.update(&info);
        let expected_hash: [u8; 32] = hasher.finalize().into();
        let info_hash_hex = hex::encode(expected_hash);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let peer_info = info.clone();
        let peer_hashes = piece_hashes.clone();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut handshake = [0u8; 68];
            stream.read_exact(&mut handshake).await.unwrap();
            let remote = Handshake::parse(&handshake).unwrap();
            assert_eq!(remote.info_hash, expected_hash[..20]);
            assert!(remote.reserved.supports_v2());
            let response = Handshake {
                info_hash: remote.info_hash,
                peer_id: [b'P'; 20],
                reserved: ExtensionFlags::with_v2_support(),
            };
            stream.write_all(&response.encode()).await.unwrap();

            let mut framed = Framed::new(stream, PeerCodec::default());
            framed
                .send(Message::Extended {
                    ext_id: EXT_HANDSHAKE_ID,
                    payload: ExtensionHandshake::new(Some(peer_info.len() as u32))
                        .with_ut_metadata(7)
                        .encode(),
                })
                .await
                .unwrap();
            while let Some(message) = framed.next().await {
                match message.unwrap() {
                    Message::Extended { ext_id: 7, payload } => {
                        let UtMetadataMessage::Request { piece } =
                            UtMetadataMessage::parse(&payload).unwrap()
                        else {
                            continue;
                        };
                        let start = piece as usize * METADATA_PIECE_SIZE;
                        let end = (start + METADATA_PIECE_SIZE).min(peer_info.len());
                        framed
                            .send(Message::Extended {
                                ext_id: LOCAL_UT_METADATA_ID,
                                payload: UtMetadataMessage::Data {
                                    piece,
                                    total_size: peer_info.len() as u32,
                                    data: peer_info[start..end].to_vec(),
                                }
                                .encode(),
                            })
                            .await
                            .unwrap();
                    }
                    Message::HashRequest {
                        pieces_root: requested_root,
                        base_layer,
                        index,
                        length,
                        proof_layers,
                    } => {
                        assert_eq!(requested_root, pieces_root);
                        assert_eq!(base_layer, 0);
                        assert_eq!(index, 0);
                        assert_eq!(length, 4);
                        assert_eq!(proof_layers, 1);
                        let mut hashes = peer_hashes.clone();
                        hashes.push([0; 32]);
                        framed
                            .send(Message::Hashes {
                                pieces_root,
                                base_layer,
                                index,
                                length,
                                proof_layers,
                                hashes,
                            })
                            .await
                            .unwrap();
                        break;
                    }
                    _ => {}
                }
            }
        });

        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::Metadata as usize] = 1024 * 1024;
        caps[MemoryClass::PeerBuffer as usize] = 64 * 1024;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 1024 * 1024,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });
        let (engine_tx, mut engine_rx) = mpsc::channel(1);
        let (task_tx, task_rx) = mpsc::channel(8);
        let (_, task_memory) =
            prepare_metadata_task_memory_with_peers(&governor, Vec::new(), vec![peer_addr], 8)
                .unwrap();
        let task = tokio::spawn(run_metadata_task(
            MetadataInfoHash::V2(expected_hash),
            info_hash_hex.clone(),
            Vec::new(),
            task_memory,
            task_rx,
            engine_tx,
            task_tx.clone(),
            empty_registry(),
            governor,
            6881,
            8,
            1,
            1,
            false,
            OutboundEgressPolicy {
                allow_loopback: true,
                ..OutboundEgressPolicy::default()
            },
            GlobalNetworkBudget::unlimited(),
        ));

        let command = timeout(Duration::from_secs(2), engine_rx.recv())
            .await
            .expect("direct x.pe peer should complete metadata")
            .expect("metadata task should emit completion");
        let EngineCmd::CompleteMagnet { raw, .. } = command else {
            panic!("unexpected engine command")
        };
        let rt_metainfo::TorrentMeta::V2(meta) = rt_metainfo::parse_torrent(&raw).unwrap() else {
            panic!("expected pure-v2 metainfo")
        };
        assert_eq!(meta.info_hash_v2, expected_hash);
        assert_eq!(meta.piece_layers.get(&pieces_root), Some(&piece_hashes));
        task_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .expect("metadata task should stop after completion shutdown")
            .unwrap();
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn metadata_completion_waits_for_a_full_engine_mailbox() {
        let (engine_tx, mut engine_rx) = mpsc::channel(1);
        let (shutdown_reply, _shutdown_result) = tokio::sync::oneshot::channel();
        engine_tx
            .try_send(EngineCmd::Shutdown {
                reply: shutdown_reply,
            })
            .unwrap();

        let engine_tx_for_task = engine_tx.clone();
        let (task_tx, _task_rx) = mpsc::channel(1);
        let info_hash = "a".repeat(40);
        let trackers = Vec::new();
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default());
        let info = b"d4:name4:test6:lengthi1ee".to_vec();
        let lease = reserve_metadata_fetch_bytes(&governor, info.len() as u32).unwrap();
        let mut completion = tokio::spawn(async move {
            complete_metadata(
                &engine_tx_for_task,
                &task_tx,
                &info_hash,
                &trackers,
                FetchedMetadata {
                    bytes: info,
                    piece_layers: Vec::new(),
                    _lease: lease,
                },
            )
            .await
        });
        assert!(timeout(Duration::from_millis(50), &mut completion)
            .await
            .is_err());
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            2 * b"d4:name4:test6:lengthi1ee".len() as u64
        );
        assert!(matches!(
            engine_rx.recv().await,
            Some(EngineCmd::Shutdown { .. })
        ));
        assert!(timeout(Duration::from_secs(1), &mut completion)
            .await
            .unwrap()
            .unwrap());
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            2 * b"d4:name4:test6:lengthi1ee".len() as u64
        );
        let completion_command = engine_rx.recv().await;
        assert!(matches!(
            completion_command,
            Some(EngineCmd::CompleteMagnet { .. })
        ));
        drop(completion_command);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            0
        );
    }

    #[tokio::test]
    async fn metadata_fetch_has_a_total_deadline_when_peer_sends_irrelevant_messages() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(stream, PeerCodec::default());
            framed
                .send(Message::Extended {
                    ext_id: EXT_HANDSHAKE_ID,
                    payload: ExtensionHandshake::new(Some(METADATA_PIECE_SIZE as u32))
                        .with_ut_metadata(7)
                        .encode(),
                })
                .await
                .unwrap();
            loop {
                if framed.send(Message::Interested).await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        });
        let stream = TcpStream::connect(peer_addr).await.unwrap();
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::Metadata as usize] = 1024 * 1024;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 1024 * 1024,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let result = fetch_metadata_over_io_with_timeout(
            peer_addr,
            MetadataPeerIo::Tcp(Framed::new(stream, PeerCodec::default())),
            true,
            false,
            MetadataInfoHash::V1([0; 20]),
            governor,
            Duration::from_millis(50),
        )
        .await;

        let error = result.expect_err("irrelevant peer messages must not extend the fetch forever");
        assert!(error.to_string().contains("timed out"), "{error}");
        timeout(Duration::from_secs(1), peer)
            .await
            .expect("metadata test peer should stop after the client disconnects")
            .expect("metadata test peer task");
    }

    #[test]
    fn parses_metadata_transport_policy() {
        assert_eq!(
            parse_metadata_transport_policy("prefer"),
            MetadataTransportPolicy::PreferUtp
        );
        assert_eq!(
            parse_metadata_transport_policy("utp-only"),
            MetadataTransportPolicy::UtpOnly
        );
        assert_eq!(
            parse_metadata_transport_policy("off"),
            MetadataTransportPolicy::TcpOnly
        );
    }

    #[tokio::test]
    async fn silent_metadata_peer_handshake_is_bounded() {
        let config = UtpTransportConfig {
            handshake_timeout: Duration::from_secs(1),
            io_timeout: Duration::from_secs(1),
            max_datagram_len: 2048,
            max_retransmits: 1,
        };
        let listener = UtpListener::bind_with_config("127.0.0.1:0".parse().unwrap(), config)
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap() });
        let client = UtpStream::connect_with_config(addr, config).await.unwrap();
        let mut server = accept.await.unwrap();

        let error = read_utp_handshake_with_timeout(&mut server, Duration::from_millis(20))
            .await
            .expect_err("a silent metadata peer must not hold the fetch forever");

        assert!(
            error.to_string().contains("deadline has elapsed"),
            "{error}"
        );
        drop(client);
    }
}
