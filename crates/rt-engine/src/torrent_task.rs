/// Per-torrent async task.
///
/// One tokio task per torrent owns: tracker announce loop, peer connection
/// management, piece picker, and storage writes.
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use reqwest::header::RANGE;
use rt_bencode::{decode, BValue};
use rusqlite::Connection;
use tokio::net::TcpStream;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::interval;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};
use url::Url;

use std::sync::Arc;

use rt_fastresume::{
    FastresumeState, FastresumeStore, FileHint, ImportPolicy, PartialPieceState, PieceState,
};
use rt_metainfo::{torrent_info_bytes, TorrentMeta, TorrentMetaV1};
use rt_metrics::{MemoryClass, MemoryLease, ResourceGovernor};
use rt_path::{StorageProfile, StorageRootId};
use rt_peer_manager::{
    ChokeDecision, ChokeState, Choker, PeerId, PeerSnapshot, DEFAULT_MAX_UNCHOKED,
};
use rt_peer_wire::{
    codec::PeerCodec,
    extension::{ExtensionHandshake, UtMetadataMessage, EXT_HANDSHAKE_ID},
    handshake::{ExtensionFlags, Handshake},
    message::Message,
};
use rt_piece_map::{FileSpan, PieceMap};
use rt_piece_picker::{Availability, BlockRequest, PiecePicker, MAX_BLOCK_SIZE};
use rt_session::{SessionRegistry, TorrentState};
use rt_storage::{
    scheduler::{scheduled_read_owned, scheduled_write},
    IoClass, MountScheduler, PieceVerifier, SchedulerConfig, StorageIoConfig, VerifyResult,
};
use rt_tracker::{
    to_http_scrape_url,
    udp::{UdpAnnounceRequest, UdpAnnounceResponse, UdpConnectRequest, UdpConnectResponse},
    AnnounceRequest, AnnounceResponse, InfoHash, ScrapeStats, TrackerError, TrackerEvent,
    TrackerState, TrackerStatus,
};

use crate::peer_id::OUR_PEER_ID;
use crate::{EnginePeerSnapshot, TorrentRuntimeStats};

const LOCAL_UT_METADATA_ID: u8 = 1;
const LOCAL_UT_PEX_ID: u8 = 2;
const METADATA_PIECE_SIZE: usize = 16 * 1024;

/// Messages from the engine to a running torrent task.
#[derive(Debug)]
pub enum TorrentCmd {
    Pause,
    Resume,
    Recheck {
        job_id: Option<String>,
    },
    CancelJob {
        job_id: String,
    },
    Reannounce,
    ReloadFilePolicy,
    Shutdown,
    /// Peers discovered by DHT, tracker, or peer exchange.
    NewPeers(Vec<SocketAddr>),
    /// Peers explicitly added through a client API.
    PriorityPeers(Vec<SocketAddr>),
    GetPeers {
        reply: oneshot::Sender<Vec<EnginePeerSnapshot>>,
    },
    GetRuntimeStats {
        reply: oneshot::Sender<TorrentRuntimeStats>,
    },
    /// An inbound TCP peer whose handshake already matched this torrent.
    AcceptPeer {
        stream: TcpStream,
        peer_addr: SocketAddr,
        handshake: Handshake,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecheckOutcome {
    Complete,
    Paused,
    Cancelled,
    Shutdown,
}

const JOB_STATE_RUNNING: &str = "running";
const JOB_STATE_PAUSED: &str = "paused";
const JOB_STATE_CANCELLED: &str = "cancelled";
const JOB_STATE_COMPLETED: &str = "completed";
const MAX_IN_MEMORY_PIECE_ASSEMBLIES: usize = 64;
const MAX_IN_MEMORY_PIECE_ASSEMBLY_BYTES_PER_TORRENT: usize = 64 * 1024 * 1024;
const PEER_REQUEST_PIPELINE_NORMAL: usize = 32;
const PEER_REQUEST_PIPELINE_CONSTRAINED: usize = 8;
const TRACKER_PEER_CACHE_MIN: usize = 256;
const TRACKER_PEER_CACHE_MULTIPLIER: usize = 4;

fn effective_piece_assembly_soft_cap(configured_bytes: usize) -> usize {
    configured_bytes.min(MAX_IN_MEMORY_PIECE_ASSEMBLY_BYTES_PER_TORRENT)
}

fn memory_aware_request_pipeline(piece_assembly_bytes: usize, soft_cap_bytes: usize) -> usize {
    if soft_cap_bytes == 0 {
        return 0;
    }
    if piece_assembly_bytes.saturating_mul(4) >= soft_cap_bytes.saturating_mul(3) {
        PEER_REQUEST_PIPELINE_CONSTRAINED
    } else {
        PEER_REQUEST_PIPELINE_NORMAL
    }
}

fn tracker_peer_cache_cap(max_peers: usize) -> usize {
    max_peers
        .saturating_mul(TRACKER_PEER_CACHE_MULTIPLIER)
        .max(TRACKER_PEER_CACHE_MIN)
}

fn reserve_webseed_body_bytes(
    resources: &ResourceGovernor,
    bytes: u32,
) -> anyhow::Result<MemoryLease> {
    resources
        .try_acquire(MemoryClass::WebseedBody, u64::from(bytes))
        .ok_or_else(|| anyhow::anyhow!("webseed body allocation of {bytes} bytes denied"))
}

fn reserve_peer_upload_bytes(
    resources: &ResourceGovernor,
    bytes: u32,
) -> anyhow::Result<MemoryLease> {
    resources
        .try_acquire(MemoryClass::PeerBuffer, u64::from(bytes))
        .ok_or_else(|| anyhow::anyhow!("peer upload buffer allocation of {bytes} bytes denied"))
}

fn remember_tracker_peers_bounded(
    known: &mut HashSet<SocketAddr>,
    allowed_private: &mut HashSet<SocketAddr>,
    peers: &[SocketAddr],
    private: bool,
    cap: usize,
) -> u64 {
    let mut dropped = 0u64;
    for &peer in peers {
        if known.contains(&peer) {
            if private {
                allowed_private.insert(peer);
            }
            continue;
        }
        if known.len() >= cap {
            dropped = dropped.saturating_add(1);
            continue;
        }
        known.insert(peer);
        if private {
            allowed_private.insert(peer);
        }
    }
    dropped
}

/// A block received from a peer.
#[derive(Debug)]
pub struct BlockEvent {
    pub piece: u32,
    pub offset: u32,
    pub data: bytes::Bytes,
}

#[derive(Debug)]
struct PieceAssembly {
    data: Vec<u8>,
    received: Vec<bool>,
    last_used: Instant,
}

impl PieceAssembly {
    fn new(len: usize) -> Self {
        Self {
            data: vec![0; len],
            received: vec![false; len.div_ceil(MAX_BLOCK_SIZE as usize)],
            last_used: Instant::now(),
        }
    }

    fn insert(&mut self, offset: u32, block: &[u8]) -> anyhow::Result<()> {
        self.last_used = Instant::now();
        let start = offset as usize;
        let end = start
            .checked_add(block.len())
            .ok_or_else(|| anyhow::anyhow!("piece block offset overflow"))?;
        if end > self.data.len() {
            anyhow::bail!(
                "piece block range {}..{} exceeds piece length {}",
                start,
                end,
                self.data.len()
            );
        }
        let block_idx = start / MAX_BLOCK_SIZE as usize;
        let Some(received) = self.received.get_mut(block_idx) else {
            anyhow::bail!("piece block index {block_idx} out of range");
        };
        if *received {
            if self.data[start..end] == *block {
                return Ok(());
            }
            anyhow::bail!("conflicting duplicate block at offset {offset}");
        }
        self.data[start..end].copy_from_slice(block);
        *received = true;
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.received.iter().all(|received| *received)
    }

    fn len(&self) -> usize {
        self.data.len()
    }
}

fn evict_piece_assemblies_to_budget(
    assemblies: &mut HashMap<u32, PieceAssembly>,
    assembly_bytes: &mut usize,
    current_piece: u32,
    max_assemblies: usize,
    max_bytes: usize,
) -> u64 {
    let mut evictions = 0u64;
    while assemblies.len() > max_assemblies || *assembly_bytes > max_bytes {
        let Some(evict_piece) = assemblies
            .iter()
            .filter(|(piece, _)| **piece != current_piece)
            .min_by_key(|(_, assembly)| assembly.last_used)
            .map(|(piece, _)| *piece)
        else {
            break;
        };

        if let Some(assembly) = assemblies.remove(&evict_piece) {
            *assembly_bytes = assembly_bytes.saturating_sub(assembly.len());
            evictions = evictions.saturating_add(1);
        }
    }
    evictions
}

#[derive(Debug)]
enum PeerEvent {
    Bitfield {
        peer: SocketAddr,
        pieces: Vec<bool>,
    },
    Have {
        peer: SocketAddr,
        piece: u32,
    },
    Unchoked {
        peer: SocketAddr,
    },
    Choked {
        peer: SocketAddr,
        outstanding: Vec<BlockRequest>,
    },
    Interested {
        peer: SocketAddr,
    },
    NotInterested {
        peer: SocketAddr,
    },
    Piece {
        peer: SocketAddr,
        block: BlockEvent,
    },
    Uploaded {
        peer: SocketAddr,
        bytes: u64,
    },
    Disconnected {
        peer: SocketAddr,
        outstanding: Vec<BlockRequest>,
    },
    RequestTimedOut {
        peer: SocketAddr,
        timed_out: Vec<BlockRequest>,
    },
    ExtendedHandshake {
        peer: SocketAddr,
        ut_metadata_id: Option<u8>,
        ut_pex_id: Option<u8>,
        metadata_size: Option<u32>,
    },
    PeerExchange {
        peer: SocketAddr,
        peers: Vec<SocketAddr>,
    },
}

#[derive(Debug)]
struct PeerHandle {
    id: PeerId,
    cmd_tx: mpsc::Sender<PeerCommand>,
    peer_has: Vec<bool>,
    choked: bool,
    upload_choked: bool,
    interested: bool,
    upload_rate: f64,
    outstanding: usize,
    requested: Vec<BlockRequest>,
    ut_metadata_id: Option<u8>,
    ut_pex_id: Option<u8>,
    metadata_size: Option<u32>,
}

#[derive(Debug)]
enum PeerCommand {
    Request(BlockRequest),
    Have(u32),
    Choke,
    Unchoke,
    Shutdown,
}

#[derive(Clone)]
struct UploadContext {
    save_root: PathBuf,
    piece_map: PieceMap,
    storage: MountScheduler,
    resources: ResourceGovernor,
    have_pieces: Vec<bool>,
    metadata: Option<Arc<Vec<u8>>>,
    is_private: bool,
}

struct LeasedUploadBlock {
    data: bytes::Bytes,
    _lease: MemoryLease,
}

pub struct TorrentTask {
    info_hash_hex: String,
    meta: TorrentMetaV1,
    save_root: PathBuf,
    piece_map: PieceMap,
    storage: MountScheduler,
    fastresume: FastresumeStore,
    tracker_tiers: Vec<Vec<TrackerState>>,
    active_tracker_tier: usize,
    tracker_event: TrackerEvent,
    stopped_announced: bool,
    listen_port: u16,
    http_timeout: Duration,
    udp_timeout: Duration,
    min_announce_interval: Option<Duration>,
    registry: Arc<RwLock<SessionRegistry>>,
    db: Arc<Mutex<Connection>>,
    resources: ResourceGovernor,
    cmd_rx: mpsc::Receiver<TorrentCmd>,
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_event_rx: mpsc::Receiver<PeerEvent>,
    picker: PiecePicker,
    choker: Choker,
    /// active peer addresses
    active_peers: HashMap<SocketAddr, PeerHandle>,
    known_tracker_peers: HashSet<SocketAddr>,
    allowed_private_peers: HashSet<SocketAddr>,
    last_peerless_reannounce: Option<Instant>,
    webseed_client: reqwest::Client,
    webseed_next_index: usize,
    webseed_failures: Vec<u8>,
    last_progress_persist: Option<Instant>,
    piece_assemblies: HashMap<u32, PieceAssembly>,
    piece_assembly_bytes: usize,
    piece_assembly_soft_cap_bytes: usize,
    piece_assembly_evictions: u64,
    peer_request_window_reductions: u64,
    peer_command_queue_full: u64,
    tracker_peer_cache_drops: u64,
    dirty_pieces_since_barrier: HashSet<u32>,
    completed_piece_verify_from_memory: u64,
    completed_piece_verify_from_disk: u64,
    prepared_files: Mutex<HashSet<u32>>,
    paused: bool,
    max_peers: usize,
}

impl TorrentTask {
    pub fn new(
        meta: TorrentMetaV1,
        save_root: PathBuf,
        paused: bool,
        registry: Arc<RwLock<SessionRegistry>>,
        db: Arc<Mutex<Connection>>,
        resources: ResourceGovernor,
        cmd_rx: mpsc::Receiver<TorrentCmd>,
        fastresume_dir: PathBuf,
        max_peers: usize,
        listen_port: u16,
        http_timeout_secs: u64,
        udp_timeout_secs: u64,
        min_interval_secs: u64,
        piece_assembly_cap_bytes: usize,
        storage_io: StorageIoConfig,
    ) -> Self {
        let (peer_event_tx, peer_event_rx) = mpsc::channel(512);
        let total = meta.total_length();
        let last_piece_len = if total % meta.piece_length == 0 {
            meta.piece_length
        } else {
            total % meta.piece_length
        };
        let piece_count = meta.pieces.len();
        let webseed_failures = vec![0; meta.webseeds.len()];
        let picker = PiecePicker::new(piece_count, meta.piece_length as u32, last_piece_len as u32);
        let info_hash_hex = meta.info_hash.iter().map(|b| format!("{b:02x}")).collect();
        let piece_map = PieceMap::new(
            meta.piece_length,
            meta.files
                .iter()
                .map(|file| FileSpan {
                    file_index: file.index,
                    path: file.path.clone(),
                    content_offset: file.offset,
                    length: file.length,
                })
                .collect(),
        )
        .expect("metainfo parser rejects invalid piece maps");
        let storage = MountScheduler::new_for_path(
            StorageRootId::new(),
            &save_root,
            &SchedulerConfig {
                profile: StorageProfile::Unknown,
                resources: Some(resources.clone()),
                storage_io,
                ..Default::default()
            },
        );
        let tracker_tiers = tracker_tiers_from_meta(&meta);
        let mut task = TorrentTask {
            info_hash_hex,
            meta,
            save_root,
            piece_map,
            storage,
            fastresume: FastresumeStore::new(fastresume_dir),
            tracker_tiers,
            active_tracker_tier: 0,
            tracker_event: TrackerEvent::Started,
            stopped_announced: paused,
            listen_port,
            http_timeout: Duration::from_secs(http_timeout_secs.max(1)),
            udp_timeout: Duration::from_secs(udp_timeout_secs.max(1)),
            min_announce_interval: (min_interval_secs > 0)
                .then(|| Duration::from_secs(min_interval_secs)),
            registry,
            db,
            resources,
            cmd_rx,
            peer_event_tx,
            peer_event_rx,
            picker,
            choker: Choker::new(DEFAULT_MAX_UNCHOKED),
            active_peers: HashMap::new(),
            known_tracker_peers: HashSet::new(),
            allowed_private_peers: HashSet::new(),
            last_peerless_reannounce: None,
            webseed_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(http_timeout_secs.max(1)))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            webseed_next_index: 0,
            webseed_failures,
            last_progress_persist: None,
            piece_assemblies: HashMap::new(),
            piece_assembly_bytes: 0,
            piece_assembly_soft_cap_bytes: effective_piece_assembly_soft_cap(
                piece_assembly_cap_bytes,
            ),
            piece_assembly_evictions: 0,
            peer_request_window_reductions: 0,
            peer_command_queue_full: 0,
            tracker_peer_cache_drops: 0,
            dirty_pieces_since_barrier: HashSet::new(),
            completed_piece_verify_from_memory: 0,
            completed_piece_verify_from_disk: 0,
            prepared_files: Mutex::new(HashSet::new()),
            paused,
            max_peers,
        };
        task.apply_file_policy_from_db();
        task
    }

    pub async fn run(mut self) {
        let restored = self.restore_fastresume().await;
        self.persist_tracker_state().await;
        if self.paused {
            self.persist_progress().await;
            self.set_state(TorrentState::Paused).await;
        } else if !restored {
            if matches!(self.run_recheck(None).await, RecheckOutcome::Shutdown) {
                return;
            }
        } else if self.picker.is_complete() {
            self.set_state(TorrentState::Seeding).await;
        } else {
            self.set_state(TorrentState::Downloading).await;
        }

        let mut choke_tick = interval(Duration::from_secs(10));
        let mut tracker_tick = interval(Duration::from_secs(5));
        let mut peer_retry_tick = interval(Duration::from_secs(30));
        let mut webseed_tick = interval(Duration::from_millis(100));

        loop {
            tokio::select! {
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        TorrentCmd::Shutdown => {
                            self.announce_stopped().await;
                            self.save_fastresume(false).await;
                            self.shutdown_peers().await;
                            break;
                        }
                        TorrentCmd::Pause => {
                            self.paused = true;
                            self.announce_stopped().await;
                            self.shutdown_peers().await;
                            self.save_fastresume(false).await;
                            self.set_state(TorrentState::Paused).await;
                            self.tracker_event = TrackerEvent::Started;
                        }
                        TorrentCmd::Resume => {
                            self.paused = false;
                            self.restart_tracker_session();
                            if matches!(self.run_recheck(None).await, RecheckOutcome::Shutdown) {
                                break;
                            }
                        }
                        TorrentCmd::NewPeers(addrs) => {
                            if !self.paused {
                                self.remember_tracker_peers(&addrs);
                                self.connect_peers(addrs).await;
                            }
                        }
                        TorrentCmd::PriorityPeers(addrs) => {
                            if !self.paused {
                                self.remember_tracker_peers(&addrs);
                                self.connect_priority_peers(addrs).await;
                            }
                        }
                        TorrentCmd::GetPeers { reply } => {
                            let _ = reply.send(self.peer_snapshots());
                        }
                        TorrentCmd::GetRuntimeStats { reply } => {
                            let _ = reply.send(self.runtime_stats());
                        }
                        TorrentCmd::AcceptPeer {
                            stream,
                            peer_addr,
                            handshake,
                        } => {
                            if !self.paused {
                                self.accept_peer(stream, peer_addr, handshake).await;
                            }
                        }
                        TorrentCmd::Recheck { job_id } => {
                            if matches!(self.run_recheck(job_id).await, RecheckOutcome::Shutdown) {
                                break;
                            }
                        }
                        TorrentCmd::CancelJob { .. } => {}
                        TorrentCmd::Reannounce => {
                            self.tracker_event = TrackerEvent::Empty;
                            self.schedule_active_tracker_tier_now();
                        }
                        TorrentCmd::ReloadFilePolicy => {
                            self.apply_file_policy_from_db();
                        }
                    }
                }

                Some(event) = self.peer_event_rx.recv() => {
                    self.handle_peer_event(event).await;
                }

                _ = choke_tick.tick() => {
                    self.run_choker().await;
                }

                _ = tracker_tick.tick() => {
                    if !self.paused {
                        self.announce_due_trackers().await;
                    }
                }

                _ = peer_retry_tick.tick() => {
                    if !self.paused {
                        self.retry_known_tracker_peers().await;
                    }
                }

                _ = webseed_tick.tick() => {
                    if !self.paused {
                        self.download_next_webseed_block().await;
                    }
                }
            }
        }
    }

    async fn connect_peers(&mut self, addrs: Vec<SocketAddr>) {
        for addr in addrs {
            if self.active_peers.len() >= self.max_peers {
                break;
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
            let info_hash = self.meta.info_hash;
            let peer_cmd_rx = self.register_peer(addr);
            let peer_event_tx = self.peer_event_tx.clone();
            let upload = self.upload_context();
            tokio::spawn(async move {
                let disconnect_tx = peer_event_tx.clone();
                if let Err(e) =
                    run_outgoing_peer(addr, info_hash, peer_event_tx, peer_cmd_rx, upload).await
                {
                    debug!(
                        component = "peer",
                        operation = "run_outgoing",
                        peer = %addr,
                        result = "ended",
                        error = %e,
                        "peer ended"
                    );
                    let _ = disconnect_tx
                        .send(PeerEvent::Disconnected {
                            peer: addr,
                            outstanding: Vec::new(),
                        })
                        .await;
                }
            });
        }
    }

    async fn connect_priority_peers(&mut self, addrs: Vec<SocketAddr>) {
        let preferred: HashSet<SocketAddr> = addrs.iter().copied().collect();
        for addr in addrs {
            if self.active_peers.contains_key(&addr) {
                continue;
            }
            if self.active_peers.len() >= self.max_peers {
                self.drop_replaceable_peer(&preferred).await;
            }
            if self.active_peers.len() >= self.max_peers {
                break;
            }
            self.connect_peers(vec![addr]).await;
        }
    }

    async fn drop_replaceable_peer(&mut self, preferred: &HashSet<SocketAddr>) {
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
        if let Some(handle) = self.active_peers.remove(&victim) {
            let bitfield = pieces_to_bitfield(&handle.peer_has);
            self.picker.availability.remove_bitfield(&bitfield);
            for req in handle.requested {
                self.picker.cancel_request(req.piece as usize, req.begin);
            }
            let _ = handle.cmd_tx.try_send(PeerCommand::Shutdown);
            debug!(
                torrent = %self.info_hash_hex,
                peer = %victim,
                "dropped peer to connect priority peer"
            );
        }
    }

    async fn announce_due_trackers(&mut self) {
        if self.tracker_tiers.is_empty() {
            return;
        }

        let tier_idx = self.active_tracker_tier.min(self.tracker_tiers.len() - 1);
        let tier_len = self.tracker_tiers[tier_idx].len();
        let mut any_due = false;
        let mut any_success = false;

        for idx in 0..tier_len {
            if !self.tracker_tiers[tier_idx][idx].is_due() {
                continue;
            }
            any_due = true;

            let url = self.tracker_tiers[tier_idx][idx].url.clone();
            let event = self.tracker_event;
            match self.announce_tracker(&url, event).await {
                Ok(resp) => {
                    let peers: Vec<SocketAddr> = resp.peers.iter().map(|peer| peer.addr).collect();
                    self.tracker_tiers[tier_idx][idx].on_success(&resp);
                    if let Ok(scrape) = self.scrape_tracker(&url).await {
                        self.tracker_tiers[tier_idx][idx].scrape_complete = Some(scrape.complete);
                        self.tracker_tiers[tier_idx][idx].scrape_incomplete =
                            Some(scrape.incomplete);
                        self.tracker_tiers[tier_idx][idx].scrape_downloaded =
                            Some(scrape.downloaded);
                    }
                    if let Some(min_interval) = self.min_announce_interval {
                        if self.tracker_tiers[tier_idx][idx].interval < min_interval {
                            self.tracker_tiers[tier_idx][idx].interval = min_interval;
                        }
                    }
                    any_success = true;
                    self.persist_tracker_state().await;
                    self.tracker_event = tracker_event_after_success(self.tracker_event, event);
                    if !peers.is_empty() {
                        self.remember_tracker_peers(&peers);
                        info!(
                            torrent = %self.info_hash_hex,
                            tracker = %url,
                            peers = peers.len(),
                            "tracker announce returned peers"
                        );
                        self.connect_peers(peers).await;
                    }
                }
                Err(err) => {
                    warn!(
                        component = "tracker",
                        operation = "announce",
                        torrent = %self.info_hash_hex,
                        tracker = %url,
                        result = "error",
                        error = %err,
                        "tracker announce failed"
                    );
                    self.tracker_tiers[tier_idx][idx].on_failure(err);
                    self.persist_tracker_state().await;
                }
            }
        }

        if any_due && !any_success {
            self.advance_tracker_tier();
        }
    }

    async fn announce_stopped(&mut self) {
        if !consume_stopped_announce(&mut self.stopped_announced) {
            return;
        }

        for tier_idx in 0..self.tracker_tiers.len() {
            for idx in 0..self.tracker_tiers[tier_idx].len() {
                let url = self.tracker_tiers[tier_idx][idx].url.clone();
                match self.announce_tracker(&url, TrackerEvent::Stopped).await {
                    Ok(resp) => {
                        self.tracker_tiers[tier_idx][idx].on_success(&resp);
                        self.persist_tracker_state().await;
                    }
                    Err(err) => {
                        warn!(
                            component = "tracker",
                            operation = "announce_stopped",
                            torrent = %self.info_hash_hex,
                            tracker = %url,
                            result = "error",
                            error = %err,
                            "tracker stopped announce failed"
                        );
                        self.tracker_tiers[tier_idx][idx].on_failure(err);
                        self.persist_tracker_state().await;
                    }
                }
            }
        }
    }

    fn restart_tracker_session(&mut self) {
        self.tracker_event = TrackerEvent::Started;
        self.stopped_announced = false;
    }

    async fn announce_http(
        &self,
        tracker_url: &str,
        event: TrackerEvent,
    ) -> Result<AnnounceResponse, TrackerError> {
        if !tracker_url.starts_with("http://") && !tracker_url.starts_with("https://") {
            return Err(TrackerError::Disabled);
        }

        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let req = AnnounceRequest {
            info_hash: InfoHash::V1(self.meta.info_hash),
            peer_id: OUR_PEER_ID,
            port: self.listen_port,
            uploaded,
            downloaded,
            left: self.picker.bytes_left(),
            event,
            compact: true,
            numwant: Some(self.max_peers as u32),
        };
        let url = req.to_http_query(tracker_url)?;
        let response = reqwest::Client::builder()
            .timeout(self.http_timeout)
            .user_agent(crate::peer_id::USER_AGENT)
            .build()
            .map_err(|e| TrackerError::Network(e.to_string()))?
            .get(url)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    TrackerError::Timeout
                } else {
                    TrackerError::Network(e.to_string())
                }
            })?;
        if !response.status().is_success() {
            return Err(TrackerError::Http {
                status: response.status().as_u16(),
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        AnnounceResponse::parse(&bytes)
    }

    async fn announce_tracker(
        &self,
        tracker_url: &str,
        event: TrackerEvent,
    ) -> Result<AnnounceResponse, TrackerError> {
        if tracker_url.starts_with("udp://") {
            self.announce_udp(tracker_url, event).await
        } else {
            self.announce_http(tracker_url, event).await
        }
    }

    async fn announce_udp(
        &self,
        tracker_url: &str,
        event: TrackerEvent,
    ) -> Result<AnnounceResponse, TrackerError> {
        let url = Url::parse(tracker_url).map_err(|e| TrackerError::InvalidUrl(e.to_string()))?;
        let host = url
            .host_str()
            .ok_or_else(|| TrackerError::InvalidUrl("missing UDP tracker host".into()))?;
        let port = url
            .port()
            .ok_or_else(|| TrackerError::InvalidUrl("missing UDP tracker port".into()))?;
        let mut addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        let tracker_addr = addrs
            .next()
            .ok_or_else(|| TrackerError::Network(format!("no address for {host}:{port}")))?;

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

        let mut buf = vec![0u8; 1500];
        let n = tokio::time::timeout(self.udp_timeout, socket.recv(&mut buf))
            .await
            .map_err(|_| TrackerError::Timeout)?
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        let connect_resp = UdpConnectResponse::parse(&buf[..n])?;
        if connect_resp.transaction_id != connect.transaction_id {
            return Err(TrackerError::Udp("connect transaction id mismatch".into()));
        }

        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let req = AnnounceRequest {
            info_hash: InfoHash::V1(self.meta.info_hash),
            peer_id: OUR_PEER_ID,
            port: self.listen_port,
            uploaded,
            downloaded,
            left: self.picker.bytes_left(),
            event,
            compact: true,
            numwant: Some(self.max_peers as u32),
        };
        let announce = UdpAnnounceRequest::new(connect_resp.connection_id, req);
        let encoded = announce.encode()?;
        socket
            .send(&encoded)
            .await
            .map_err(|e| TrackerError::Network(e.to_string()))?;

        let n = tokio::time::timeout(self.udp_timeout, socket.recv(&mut buf))
            .await
            .map_err(|_| TrackerError::Timeout)?
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        let announce_resp = UdpAnnounceResponse::parse(&buf[..n])?;
        if announce_resp.transaction_id != announce.transaction_id {
            return Err(TrackerError::Udp("announce transaction id mismatch".into()));
        }

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

    async fn scrape_tracker(&self, tracker_url: &str) -> Result<ScrapeStats, TrackerError> {
        if !tracker_url.starts_with("http://") && !tracker_url.starts_with("https://") {
            return Err(TrackerError::Disabled);
        }
        let url = to_http_scrape_url(tracker_url, InfoHash::V1(self.meta.info_hash))?;
        let resp = reqwest::Client::new()
            .get(url)
            .header(reqwest::header::USER_AGENT, crate::peer_id::USER_AGENT)
            .timeout(self.http_timeout)
            .send()
            .await
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(TrackerError::Http {
                status: status.as_u16(),
            });
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| TrackerError::Network(e.to_string()))?;
        ScrapeStats::parse(&body, &self.meta.info_hash)
    }

    async fn accept_peer(
        &mut self,
        stream: TcpStream,
        peer_addr: SocketAddr,
        handshake: Handshake,
    ) {
        if self.active_peers.len() >= self.max_peers || self.active_peers.contains_key(&peer_addr) {
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
        let peer_cmd_rx = self.register_peer(peer_addr);
        let peer_event_tx = self.peer_event_tx.clone();
        let upload = self.upload_context();
        tokio::spawn(async move {
            let disconnect_tx = peer_event_tx.clone();
            if let Err(e) = run_incoming_peer(
                stream,
                peer_addr,
                info_hash,
                peer_event_tx,
                peer_cmd_rx,
                upload,
                handshake.reserved.supports_extension_protocol(),
            )
            .await
            {
                debug!(
                    component = "peer",
                    operation = "run_incoming",
                    peer = %peer_addr,
                    result = "ended",
                    error = %e,
                    "incoming peer ended"
                );
                let _ = disconnect_tx
                    .send(PeerEvent::Disconnected {
                        peer: peer_addr,
                        outstanding: Vec::new(),
                    })
                    .await;
            }
        });
    }

    fn upload_context(&self) -> UploadContext {
        UploadContext {
            save_root: self.save_root.clone(),
            piece_map: self.piece_map.clone(),
            storage: self.storage.clone(),
            resources: self.resources.clone(),
            have_pieces: self.picker.have_pieces(),
            metadata: torrent_info_bytes(&self.meta.raw).ok().map(Arc::new),
            is_private: self.meta.private,
        }
    }

    fn register_peer(&mut self, addr: SocketAddr) -> mpsc::Receiver<PeerCommand> {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        self.active_peers.insert(
            addr,
            PeerHandle {
                id: PeerId::new(),
                cmd_tx,
                peer_has: vec![false; self.meta.pieces.len()],
                choked: true,
                upload_choked: true,
                interested: false,
                upload_rate: 0.0,
                outstanding: 0,
                requested: Vec::new(),
                ut_metadata_id: None,
                ut_pex_id: None,
                metadata_size: None,
            },
        );
        cmd_rx
    }

    fn peer_snapshots(&self) -> Vec<EnginePeerSnapshot> {
        self.active_peers
            .iter()
            .map(|(addr, peer)| {
                let pieces = peer.peer_has.iter().filter(|has| **has).count();
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
                    download_rate: 0,
                    upload_rate: peer.upload_rate.max(0.0).round() as i64,
                    downloaded: 0,
                    uploaded: 0,
                }
            })
            .collect()
    }

    fn runtime_stats(&self) -> TorrentRuntimeStats {
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
                    .saturating_add(peer.peer_has.capacity() as u64)
            })
            .sum::<u64>();
        let tracker_peer_cache_bytes = (self.known_tracker_peers.capacity() as u64)
            .saturating_mul(std::mem::size_of::<SocketAddr>() as u64);
        TorrentRuntimeStats {
            connected_peers: self.active_peers.len() as u64,
            outstanding_requests,
            fastresume_dirty_pieces: self.dirty_pieces_since_barrier.len() as u64,
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

    fn remember_tracker_peers(&mut self, peers: &[SocketAddr]) {
        let dropped = remember_tracker_peers_bounded(
            &mut self.known_tracker_peers,
            &mut self.allowed_private_peers,
            peers,
            self.meta.private,
            tracker_peer_cache_cap(self.max_peers),
        );
        self.tracker_peer_cache_drops = self.tracker_peer_cache_drops.saturating_add(dropped);
    }

    async fn retry_known_tracker_peers(&mut self) {
        if self.picker.is_complete() {
            return;
        }
        if self.active_peers.is_empty() {
            self.schedule_peerless_reannounce();
        }
        if self.active_peers.len() >= self.max_peers || self.known_tracker_peers.is_empty() {
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
        self.connect_peers(peers).await;
    }

    async fn download_next_webseed_block(&mut self) {
        if self.picker.is_complete() {
            return;
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
            return;
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
            return;
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
            return;
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
            return;
        };

        let seed_count = self.meta.webseeds.len();
        for attempt in 0..seed_count {
            let idx = (self.webseed_next_index + attempt) % seed_count;
            if self
                .webseed_failures
                .get(idx)
                .copied()
                .is_some_and(|failures| failures >= 3)
            {
                continue;
            }
            let Some(url) = webseed_block_url(&self.meta, &self.meta.webseeds[idx]) else {
                debug!(
                    torrent = %self.info_hash_hex,
                    webseed = %self.meta.webseeds[idx],
                    "webseed skipped: unsupported url"
                );
                continue;
            };
            debug!(
                torrent = %self.info_hash_hex,
                webseed = %self.meta.webseeds[idx],
                url = %url,
                piece = req.piece,
                offset = req.begin,
                length = req.length,
                "fetching webseed block"
            );
            match self.fetch_webseed_block(&url, req).await {
                Ok(data) => {
                    self.webseed_next_index = (idx + 1) % seed_count;
                    if let Some(failures) = self.webseed_failures.get_mut(idx) {
                        *failures = 0;
                    }
                    self.handle_block(BlockEvent {
                        piece: req.piece,
                        offset: req.begin,
                        data,
                    })
                    .await;
                    return;
                }
                Err(e) => {
                    let err = e.to_string();
                    if let Some(failures) = self.webseed_failures.get_mut(idx) {
                        if err.contains("HTTP 404") || err.contains("HTTP 410") {
                            *failures = 3;
                        } else {
                            *failures = failures.saturating_add(1);
                        }
                    }
                    warn!(
                        component = "webseed",
                        operation = "fetch_block",
                        torrent = %self.info_hash_hex,
                        webseed = %self.meta.webseeds[idx],
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
    }

    async fn fetch_webseed_block(
        &self,
        url: &Url,
        req: BlockRequest,
    ) -> anyhow::Result<bytes::Bytes> {
        let _lease = reserve_webseed_body_bytes(&self.resources, req.length)?;
        let start = req.piece as u64 * self.meta.piece_length + req.begin as u64;
        let end = start + req.length as u64 - 1;
        let response = self
            .webseed_client
            .get(url.clone())
            .header(RANGE, format!("bytes={start}-{end}"))
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("HTTP {}", response.status());
        }
        let bytes = response.bytes().await?;
        if bytes.len() != req.length as usize {
            anyhow::bail!(
                "expected {} bytes, received {} bytes",
                req.length,
                bytes.len()
            );
        }
        Ok(bytes)
    }

    fn schedule_peerless_reannounce(&mut self) {
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

    fn peer_source_allowed(&self, peer: SocketAddr) -> bool {
        private_peer_source_allowed(self.meta.private, &self.allowed_private_peers, peer)
    }

    async fn handle_peer_event(&mut self, event: PeerEvent) {
        match event {
            PeerEvent::Bitfield { peer, pieces } => {
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
            PeerEvent::Have { peer, piece } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    if let Some(has_piece) = handle.peer_has.get_mut(piece as usize) {
                        if !*has_piece {
                            *has_piece = true;
                            self.picker.availability.add_have(piece as usize);
                        }
                    }
                }
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::Unchoked { peer } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.choked = false;
                }
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::Choked { peer, outstanding } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.choked = true;
                    handle.outstanding = 0;
                    handle.requested.clear();
                }
                for req in outstanding {
                    self.picker.cancel_request(req.piece as usize, req.begin);
                }
            }
            PeerEvent::Interested { peer } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.interested = true;
                }
            }
            PeerEvent::NotInterested { peer } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.interested = false;
                }
            }
            PeerEvent::Piece { peer, block } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.outstanding = handle.outstanding.saturating_sub(1);
                    remove_requested_block(&mut handle.requested, block.piece, block.offset);
                }
                self.handle_block(block).await;
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::Uploaded { peer, bytes } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.upload_rate = 0.3 * bytes as f64 + 0.7 * handle.upload_rate;
                }
                self.record_upload(bytes).await;
            }
            PeerEvent::Disconnected { peer, outstanding } => {
                if let Some(handle) = self.active_peers.get(&peer) {
                    let bitfield = pieces_to_bitfield(&handle.peer_has);
                    self.picker.availability.remove_bitfield(&bitfield);
                }
                for req in outstanding {
                    self.picker.cancel_request(req.piece as usize, req.begin);
                }
                self.active_peers.remove(&peer);
                if self.active_peers.is_empty() {
                    self.clear_piece_assemblies();
                }
            }
            PeerEvent::RequestTimedOut { peer, timed_out } => {
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
            } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    handle.ut_metadata_id = ut_metadata_id;
                    handle.ut_pex_id = ut_pex_id;
                    handle.metadata_size = metadata_size;
                }
            }
            PeerEvent::PeerExchange { peer, peers } => {
                if self.meta.private {
                    return;
                }
                let peer_count = peers.len();
                self.remember_tracker_peers(&peers);
                self.connect_peers(peers).await;
                debug!(
                    torrent = %self.info_hash_hex,
                    peer = %peer,
                    peers = peer_count,
                    "peer exchange discovered peers"
                );
            }
        }
    }

    async fn run_choker(&mut self) {
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
            match decisions.get(&handle.id).copied() {
                Some(ChokeDecision::Unchoke) if handle.upload_choked => {
                    handle.upload_choked = false;
                    if handle.cmd_tx.try_send(PeerCommand::Unchoke).is_err() {
                        queue_full = queue_full.saturating_add(1);
                    }
                }
                Some(ChokeDecision::Choke) if !handle.upload_choked => {
                    handle.upload_choked = true;
                    if handle.cmd_tx.try_send(PeerCommand::Choke).is_err() {
                        queue_full = queue_full.saturating_add(1);
                    }
                }
                _ => {}
            }
        }
        self.peer_command_queue_full = self.peer_command_queue_full.saturating_add(queue_full);
    }

    async fn refill_peer_requests(&mut self, peer: SocketAddr) {
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
            if handle.cmd_tx.try_send(PeerCommand::Request(req)).is_err() {
                queue_full = queue_full.saturating_add(1);
                self.picker.cancel_request(req.piece as usize, req.begin);
                break;
            }
            handle.outstanding += 1;
            handle.requested.push(req);
        }
        self.peer_command_queue_full = self.peer_command_queue_full.saturating_add(queue_full);
    }

    async fn handle_block(&mut self, block: BlockEvent) {
        let piece = block.piece;
        let aggregate_piece_write = self.can_aggregate_piece_write(piece);
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
                self.remove_piece_assembly(piece);
                return;
            }
        }
        self.record_download(block.data.len() as u64).await;

        let complete = self
            .picker
            .block_received(block.piece as usize, block.offset);
        if complete {
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
                        self.set_state(TorrentState::Seeding).await;
                        info!(
                            component = "torrent",
                            operation = "complete_download",
                            torrent = %self.info_hash_hex,
                            result = "ok",
                            "download complete"
                        );
                    }
                }
                VerifyResult::Invalid => {
                    warn!(
                        piece = block.piece,
                        torrent = %self.info_hash_hex,
                        "piece verification failed"
                    );
                    self.picker.reject_piece(block.piece as usize);
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
                    self.remove_piece_assembly(block.piece);
                }
            }
        }
    }

    fn can_aggregate_piece_write(&self, piece: u32) -> bool {
        self.piece_length(piece)
            .map(|len| len as usize <= self.piece_assembly_soft_cap_bytes)
            .unwrap_or(false)
    }

    fn record_piece_block(&mut self, block: &BlockEvent) -> anyhow::Result<()> {
        let len = self.piece_length(block.piece)? as usize;
        if len > self.piece_assembly_soft_cap_bytes {
            return Ok(());
        }

        let inserted = if self.piece_assemblies.contains_key(&block.piece) {
            false
        } else {
            self.piece_assembly_bytes = self.piece_assembly_bytes.saturating_add(len);
            self.piece_assemblies
                .insert(block.piece, PieceAssembly::new(len));
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

    fn remove_piece_assembly(&mut self, piece: u32) {
        if let Some(assembly) = self.piece_assemblies.remove(&piece) {
            self.piece_assembly_bytes = self.piece_assembly_bytes.saturating_sub(assembly.len());
        }
    }

    fn clear_piece_assemblies(&mut self) {
        self.piece_assemblies.clear();
        self.piece_assembly_bytes = 0;
    }

    fn enforce_piece_assembly_budget(&mut self, current_piece: u32) {
        let evictions = evict_piece_assemblies_to_budget(
            &mut self.piece_assemblies,
            &mut self.piece_assembly_bytes,
            current_piece,
            MAX_IN_MEMORY_PIECE_ASSEMBLIES,
            self.piece_assembly_soft_cap_bytes,
        );
        self.piece_assembly_evictions = self.piece_assembly_evictions.saturating_add(evictions);
    }

    async fn send_have_to_peers(&mut self, piece: u32) {
        let peers: Vec<SocketAddr> = self.active_peers.keys().copied().collect();
        let mut queue_full = 0u64;
        for peer in peers {
            if let Some(handle) = self.active_peers.get(&peer) {
                if handle.cmd_tx.try_send(PeerCommand::Have(piece)).is_err() {
                    queue_full = queue_full.saturating_add(1);
                }
            }
        }
        self.peer_command_queue_full = self.peer_command_queue_full.saturating_add(queue_full);
    }

    async fn shutdown_peers(&mut self) {
        let handles: Vec<mpsc::Sender<PeerCommand>> = self
            .active_peers
            .values()
            .map(|peer| peer.cmd_tx.clone())
            .collect();

        for tx in handles {
            let _ = tx.send(PeerCommand::Shutdown).await;
        }
        self.active_peers.clear();
        self.clear_piece_assemblies();
    }

    async fn record_download(&self, bytes: u64) {
        self.update_transfer(bytes, false).await;
    }

    async fn record_upload(&self, bytes: u64) {
        self.update_transfer(bytes, true).await;
    }

    async fn update_transfer(&self, bytes: u64, upload: bool) {
        let mut reg = self.registry.write().await;
        let Some(entry) = reg.get_mut(&self.info_hash_hex) else {
            return;
        };
        if upload {
            entry.stats.add_upload(bytes);
        } else {
            entry.stats.add_download(bytes);
        }
        let row = crate::engine::row_from_entry(entry, &TorrentMeta::V1(self.meta.clone()));
        let db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::upsert(&db, &row) {
            warn!(
                component = "db",
                operation = "persist_transfer_stats",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %e,
                "failed to persist transfer stats"
            );
        }
    }

    async fn transfer_snapshot(&self) -> (u64, u64) {
        let reg = self.registry.read().await;
        reg.get(&self.info_hash_hex)
            .map(|entry| (entry.stats.uploaded, entry.stats.downloaded))
            .unwrap_or((0, 0))
    }

    async fn persist_tracker_state(&self) {
        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let left = self.picker.bytes_left() as i64;
        let now = Instant::now();
        let mut rows = Vec::new();
        let mut tracker_index = 0i64;
        for (tier_idx, tier) in self.tracker_tiers.iter().enumerate() {
            for tracker in tier {
                rows.push(rt_db::TorrentTrackerRow {
                    info_hash: self.info_hash_hex.clone(),
                    tracker_index,
                    tier: tier_idx as i64,
                    url: tracker.url.clone(),
                    status: tracker_status_label(&tracker.status).to_owned(),
                    last_announce_at: instant_to_unix(tracker.last_announce, now),
                    next_announce_at: instant_to_unix(tracker.next_announce, now),
                    last_success_at: instant_to_unix(tracker.last_success, now),
                    failure_reason: tracker_failure_reason(&tracker.status),
                    warning_message: tracker_warning_message(&tracker.status),
                    seeders: tracker.scrape_complete.map(|value| value as i64),
                    leechers: tracker.scrape_incomplete.map(|value| value as i64),
                    completed: tracker.scrape_downloaded.map(|value| value as i64),
                    uploaded: uploaded as i64,
                    downloaded: downloaded as i64,
                    left_bytes: left,
                });
                tracker_index += 1;
            }
        }
        let mut db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::replace_torrent_trackers(&mut db, &self.info_hash_hex, &rows) {
            warn!(
                component = "db",
                operation = "persist_tracker_state",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %e,
                "failed to persist tracker state"
            );
        }
    }

    fn schedule_trackers_now(&mut self) {
        for tier in &mut self.tracker_tiers {
            for tracker in tier {
                tracker.schedule_immediate();
            }
        }
    }

    fn schedule_active_tracker_tier_now(&mut self) {
        if self.tracker_tiers.is_empty() {
            return;
        }
        let tier_idx = self.active_tracker_tier.min(self.tracker_tiers.len() - 1);
        for tracker in &mut self.tracker_tiers[tier_idx] {
            tracker.schedule_immediate();
        }
    }

    fn apply_file_policy_from_db(&mut self) {
        let rows = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::list_torrent_files(&db, &self.info_hash_hex).unwrap_or_default()
        };
        if rows.is_empty() {
            return;
        }
        let policy: HashMap<u32, (bool, i64)> = rows
            .into_iter()
            .map(|row| (row.file_index as u32, (row.wanted, row.priority)))
            .collect();
        let mut priority_pieces = Vec::new();
        for piece in 0..self.piece_map.piece_count {
            let Ok(regions) = self.piece_map.piece_to_file_regions(piece) else {
                continue;
            };
            let mut any_wanted = false;
            let mut any_high = false;
            for region in regions {
                let (wanted, priority) =
                    policy.get(&region.file_index).copied().unwrap_or((true, 1));
                any_wanted |= wanted && priority > 0;
                any_high |= wanted && priority > 1;
            }
            self.picker.set_piece_enabled(piece as usize, any_wanted);
            if any_high {
                priority_pieces.push(piece as usize);
            }
        }
        self.picker.set_priority(priority_pieces);
    }

    fn advance_tracker_tier(&mut self) {
        if self.tracker_tiers.len() <= 1 {
            return;
        }
        let old = self.active_tracker_tier;
        self.active_tracker_tier = (self.active_tracker_tier + 1) % self.tracker_tiers.len();
        for tracker in &mut self.tracker_tiers[self.active_tracker_tier] {
            tracker.schedule_immediate();
        }
        warn!(
            torrent = %self.info_hash_hex,
            from_tier = old,
            to_tier = self.active_tracker_tier,
            "advancing tracker tier after announce failures"
        );
    }

    async fn run_recheck(&mut self, job_id: Option<String>) -> RecheckOutcome {
        self.shutdown_peers().await;
        self.set_state(TorrentState::Checking).await;

        let mut valid = 0usize;
        let mut invalid = 0usize;
        let mut invalid_pieces = Vec::new();
        let mut verified_pieces = Vec::with_capacity(self.piece_map.piece_count as usize);

        for piece in 0..self.piece_map.piece_count {
            match self.pending_recheck_control().await {
                Some(RecheckOutcome::Paused) => {
                    self.save_fastresume(false).await;
                    self.set_state(TorrentState::Paused).await;
                    if let Some(job_id) = &job_id {
                        self.persist_recheck_job_progress(
                            job_id,
                            piece,
                            valid,
                            &invalid_pieces,
                            JOB_STATE_PAUSED,
                            Some("recheck paused"),
                        );
                    }
                    return RecheckOutcome::Paused;
                }
                Some(RecheckOutcome::Cancelled) => {
                    self.save_fastresume(false).await;
                    self.set_state(TorrentState::Paused).await;
                    if let Some(job_id) = &job_id {
                        self.persist_recheck_job_progress(
                            job_id,
                            piece,
                            valid,
                            &invalid_pieces,
                            JOB_STATE_CANCELLED,
                            Some("recheck cancelled"),
                        );
                    }
                    return RecheckOutcome::Cancelled;
                }
                Some(RecheckOutcome::Shutdown) => {
                    self.save_fastresume(false).await;
                    self.set_state(TorrentState::Stopped).await;
                    if let Some(job_id) = &job_id {
                        self.persist_recheck_job_progress(
                            job_id,
                            piece,
                            valid,
                            &invalid_pieces,
                            JOB_STATE_PAUSED,
                            Some("recheck interrupted by shutdown"),
                        );
                    }
                    return RecheckOutcome::Shutdown;
                }
                Some(RecheckOutcome::Complete) | None => {}
            }

            let result = PieceVerifier::new(
                &self.save_root,
                &self.storage,
                &self.piece_map,
                &self.meta.pieces,
            )
            .verify_piece(piece)
            .await;
            match result {
                VerifyResult::Valid => {
                    verified_pieces.push((piece, true));
                    valid += 1;
                }
                VerifyResult::Invalid => {
                    verified_pieces.push((piece, false));
                    invalid_pieces.push(piece as i64);
                    invalid += 1;
                }
                VerifyResult::Missing { .. } => {
                    verified_pieces.push((piece, false));
                    invalid_pieces.push(piece as i64);
                    invalid += 1;
                }
            }

            if piece > 0 && piece % 64 == 0 {
                self.save_fastresume(false).await;
                if let Some(job_id) = &job_id {
                    self.persist_recheck_job_progress(
                        job_id,
                        piece + 1,
                        valid,
                        &invalid_pieces,
                        JOB_STATE_RUNNING,
                        Some("recheck progress"),
                    );
                }
            }
        }

        info!(
            torrent = %self.info_hash_hex,
            valid,
            invalid,
            "recheck complete"
        );
        self.commit_recheck_results(&verified_pieces);
        self.save_fastresume(true).await;
        if let Some(job_id) = &job_id {
            self.persist_recheck_job_progress(
                job_id,
                self.piece_map.piece_count,
                valid,
                &invalid_pieces,
                JOB_STATE_COMPLETED,
                Some("recheck completed"),
            );
        }

        if self.picker.is_complete() {
            self.tracker_event = TrackerEvent::Completed;
            self.schedule_trackers_now();
            self.set_state(TorrentState::Seeding).await;
        } else {
            self.persist_progress().await;
            self.set_state(TorrentState::Downloading).await;
        }
        RecheckOutcome::Complete
    }

    fn commit_recheck_results(&mut self, verified_pieces: &[(u32, bool)]) {
        for (piece, valid) in verified_pieces.iter().copied() {
            if valid {
                self.picker.mark_have(piece as usize);
            } else {
                self.picker.reject_piece(piece as usize);
            }
        }
    }

    async fn pending_recheck_control(&mut self) -> Option<RecheckOutcome> {
        loop {
            match self.cmd_rx.try_recv() {
                Ok(TorrentCmd::Pause) => {
                    self.paused = true;
                    self.shutdown_peers().await;
                    self.announce_stopped().await;
                    return Some(RecheckOutcome::Paused);
                }
                Ok(TorrentCmd::Shutdown) => {
                    self.paused = true;
                    self.shutdown_peers().await;
                    self.announce_stopped().await;
                    return Some(RecheckOutcome::Shutdown);
                }
                Ok(TorrentCmd::Resume) => {
                    self.paused = false;
                }
                Ok(TorrentCmd::Reannounce) => {
                    self.schedule_active_tracker_tier_now();
                }
                Ok(TorrentCmd::ReloadFilePolicy) => {
                    self.apply_file_policy_from_db();
                }
                Ok(TorrentCmd::Recheck { .. }) => {}
                Ok(TorrentCmd::CancelJob { job_id }) => {
                    debug!(
                        torrent = %self.info_hash_hex,
                        job_id,
                        "cancelling recheck job"
                    );
                    return Some(RecheckOutcome::Cancelled);
                }
                Ok(TorrentCmd::NewPeers(_)) => {}
                Ok(TorrentCmd::PriorityPeers(_)) => {}
                Ok(TorrentCmd::GetPeers { reply }) => {
                    let _ = reply.send(Vec::new());
                }
                Ok(TorrentCmd::GetRuntimeStats { reply }) => {
                    let _ = reply.send(self.runtime_stats());
                }
                Ok(TorrentCmd::AcceptPeer { .. }) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return None,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    return Some(RecheckOutcome::Shutdown);
                }
            }
        }
    }

    async fn restore_fastresume(&mut self) -> bool {
        let mut state = match self.fastresume.load(&self.info_hash_hex) {
            Ok(state) => state,
            Err(rt_fastresume::FastresumeError::NotFound) => {
                debug!(
                    component = "fastresume",
                    operation = "load",
                    torrent = %self.info_hash_hex,
                    result = "not_found",
                    "no fastresume state"
                );
                return false;
            }
            Err(e) => {
                warn!(
                    component = "fastresume",
                    operation = "load",
                    torrent = %self.info_hash_hex,
                    result = "error",
                    error = %e,
                    "failed to load fastresume state"
                );
                return false;
            }
        };

        if let Err(e) = state.validate(&self.meta.info_hash, self.meta.pieces.len() as u32) {
            warn!(
                component = "fastresume",
                operation = "validate",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %e,
                "discarding incompatible fastresume state"
            );
            return false;
        }

        if !state.clean_shutdown {
            match state.apply_unclean_shutdown_watermark() {
                Some(downgraded) => {
                    warn!(
                        torrent = %self.info_hash_hex,
                        downgraded,
                        "fastresume had unclean shutdown; applying bounded dirty-piece recheck watermark"
                    );
                }
                None => {
                    warn!(
                        torrent = %self.info_hash_hex,
                        "discarding unclean fastresume state without durability watermark"
                    );
                    return false;
                }
            }
        }

        match collect_file_hints(&self.save_root, &self.meta) {
            Ok(hints) => {
                let invalidated = state.apply_file_hints(hints, &self.piece_map);
                if invalidated > 0 {
                    warn!(
                        torrent = %self.info_hash_hex,
                        invalidated,
                        "fastresume file hints changed"
                    );
                }
            }
            Err(e) => {
                warn!(
                    component = "fastresume",
                    operation = "collect_file_hints",
                    torrent = %self.info_hash_hex,
                    result = "error",
                    error = %e,
                    "could not collect file hints for fastresume"
                );
                return false;
            }
        }

        for (piece, piece_state) in state.pieces.iter().copied().enumerate() {
            match piece_state {
                PieceState::Valid => self.picker.mark_have(piece),
                PieceState::Invalid | PieceState::Missing | PieceState::Unknown => {
                    self.picker.reject_piece(piece)
                }
            }
        }
        for partial in &state.partial_pieces {
            self.picker
                .restore_partial_piece(partial.piece as usize, &partial.received_blocks);
        }

        {
            let mut reg = self.registry.write().await;
            if let Some(entry) = reg.get_mut(&self.info_hash_hex) {
                entry.stats.uploaded = state.uploaded_bytes;
                entry.stats.downloaded = state.downloaded_bytes;
                entry.total_length = self.meta.total_length();
                entry.amount_left = self.picker.bytes_left();
            }
        }

        if state.file_hints.is_empty() {
            self.save_fastresume(false).await;
        }

        info!(
            torrent = %self.info_hash_hex,
            valid = state.valid_piece_count(),
            unknown = state.unknown_piece_count(),
            "fastresume restored"
        );
        true
    }

    async fn write_block(&self, block: &BlockEvent) -> anyhow::Result<()> {
        let regions =
            self.piece_map
                .validate_request(block.piece, block.offset, block.data.len() as u32)?;
        let mut data_offset = 0usize;

        for region in regions {
            let file = self
                .meta
                .files
                .iter()
                .find(|file| file.index == region.file_index)
                .ok_or_else(|| anyhow::anyhow!("file index {} out of range", region.file_index))?;
            let path = file.path.resolve(&self.save_root);
            self.prepare_file_once(file.index, &path, file.length)
                .await?;
            let end = data_offset + region.length as usize;
            let data = bytes::Bytes::copy_from_slice(&block.data[data_offset..end]);
            scheduled_write(
                &self.storage,
                IoClass::PeerWrite,
                &path,
                region.file_offset,
                data,
                true,
            )
            .await?;
            data_offset = end;
        }

        Ok(())
    }

    async fn write_completed_piece(&self, piece: u32) -> anyhow::Result<()> {
        let assembly = self
            .piece_assemblies
            .get(&piece)
            .filter(|assembly| assembly.is_complete())
            .ok_or_else(|| anyhow::anyhow!("piece {piece} is not fully assembled"))?;
        let regions = self.piece_map.piece_to_file_regions(piece)?;
        for region in regions {
            let file = self
                .meta
                .files
                .iter()
                .find(|file| file.index == region.file_index)
                .ok_or_else(|| anyhow::anyhow!("file index {} out of range", region.file_index))?;
            let path = file.path.resolve(&self.save_root);
            self.prepare_file_once(file.index, &path, file.length)
                .await?;
            let start = region.piece_offset as usize;
            let end = start + region.length as usize;
            let data = bytes::Bytes::copy_from_slice(&assembly.data[start..end]);
            scheduled_write(
                &self.storage,
                IoClass::PeerWrite,
                &path,
                region.file_offset,
                data,
                true,
            )
            .await?;
        }
        Ok(())
    }

    async fn prepare_file_once(
        &self,
        file_index: u32,
        path: &std::path::Path,
        len: u64,
    ) -> anyhow::Result<()> {
        {
            let prepared = self
                .prepared_files
                .lock()
                .expect("prepared file registry mutex poisoned");
            if prepared.contains(&file_index) {
                return Ok(());
            }
        }

        self.storage
            .prepare_file(path, len, self.storage.io_config().preallocation_mode)
            .await?;

        let mut prepared = self
            .prepared_files
            .lock()
            .expect("prepared file registry mutex poisoned");
        prepared.insert(file_index);
        Ok(())
    }

    async fn verify_piece(&self, piece: u32) -> VerifyResult {
        PieceVerifier::new(
            &self.save_root,
            &self.storage,
            &self.piece_map,
            &self.meta.pieces,
        )
        .verify_piece(piece)
        .await
    }

    async fn verify_completed_piece(&mut self, piece: u32) -> VerifyResult {
        if let Some(assembly) = self
            .piece_assemblies
            .get(&piece)
            .filter(|assembly| assembly.is_complete())
        {
            self.completed_piece_verify_from_memory =
                self.completed_piece_verify_from_memory.saturating_add(1);
            let Some(expected) = self.meta.pieces.get(piece as usize) else {
                return VerifyResult::Missing {
                    file_index: 0,
                    reason: format!("no hash for piece {piece}"),
                };
            };
            match self
                .storage
                .hash_sha1(bytes::Bytes::copy_from_slice(&assembly.data))
                .await
            {
                Ok(actual) if &actual == expected => return VerifyResult::Valid,
                Ok(_) => return VerifyResult::Invalid,
                Err(e) => {
                    return VerifyResult::Missing {
                        file_index: 0,
                        reason: e.to_string(),
                    }
                }
            }
        }
        self.completed_piece_verify_from_disk =
            self.completed_piece_verify_from_disk.saturating_add(1);
        self.verify_piece(piece).await
    }

    fn piece_length(&self, piece: u32) -> anyhow::Result<u32> {
        if piece as usize >= self.meta.pieces.len() {
            anyhow::bail!("piece {piece} out of range");
        }
        let last = self.meta.pieces.len().saturating_sub(1) as u32;
        if piece == last {
            let total = self.meta.total_length();
            let rem = total % self.meta.piece_length;
            Ok(if rem == 0 {
                self.meta.piece_length as u32
            } else {
                rem as u32
            })
        } else {
            Ok(self.meta.piece_length as u32)
        }
    }

    async fn set_state(&self, state: TorrentState) {
        let mut reg = self.registry.write().await;
        if let Some(entry) = reg.get_mut(&self.info_hash_hex) {
            entry.total_length = self.meta.total_length();
            entry.amount_left = self.picker.bytes_left();
            let _ = entry.transition(state);
            if state == TorrentState::Seeding && entry.completed_at.is_none() {
                entry.amount_left = 0;
                entry.completed_at = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
            }
            let row = crate::engine::row_from_entry(entry, &TorrentMeta::V1(self.meta.clone()));
            let db = self.db.lock().expect("database mutex poisoned");
            if let Err(e) = rt_db::upsert(&db, &row) {
                warn!(
                    component = "db",
                    operation = "persist_torrent_state",
                    torrent = %self.info_hash_hex,
                    result = "error",
                    error = %e,
                    "failed to persist torrent state"
                );
            }
        }
    }

    async fn persist_progress(&self) {
        let mut reg = self.registry.write().await;
        if let Some(entry) = reg.get_mut(&self.info_hash_hex) {
            entry.total_length = self.meta.total_length();
            entry.amount_left = self.picker.bytes_left();
            let row = crate::engine::row_from_entry(entry, &TorrentMeta::V1(self.meta.clone()));
            let db = self.db.lock().expect("database mutex poisoned");
            if let Err(e) = rt_db::upsert(&db, &row) {
                warn!(
                    component = "db",
                    operation = "persist_torrent_progress",
                    torrent = %self.info_hash_hex,
                    result = "error",
                    error = %e,
                    "failed to persist torrent progress"
                );
            }
        }
    }

    async fn persist_progress_throttled(&mut self, force: bool) {
        const PROGRESS_PERSIST_INTERVAL: Duration = Duration::from_secs(5);
        let now = Instant::now();
        if !force
            && self
                .last_progress_persist
                .is_some_and(|last| now.duration_since(last) < PROGRESS_PERSIST_INTERVAL)
        {
            return;
        }
        self.last_progress_persist = Some(now);
        self.persist_progress().await;
        self.save_fastresume(false).await;
    }

    fn persist_recheck_job_progress(
        &self,
        job_id: &str,
        next_piece: u32,
        valid_pieces: usize,
        invalid_pieces: &[i64],
        state: &str,
        message: Option<&str>,
    ) {
        let now = unix_now();
        let mut job = {
            let db = self.db.lock().expect("database mutex poisoned");
            match rt_db::get_job(&db, job_id) {
                Ok(job) => job,
                Err(e) => {
                    warn!(
                        component = "db",
                        operation = "load_recheck_job",
                        job_id,
                        result = "error",
                        error = %e,
                        "failed to load recheck job"
                    );
                    return;
                }
            }
        };
        let done = next_piece.min(self.piece_map.piece_count) as i64;
        job.state = state.to_owned();
        job.done = done;
        job.checkpoint = done;
        job.piece_index = Some(done);
        job.byte_offset = Some(self.verified_byte_offset(next_piece));
        job.verified_bytes = (valid_pieces as u64).saturating_mul(self.meta.piece_length) as i64;
        job.invalid_pieces = invalid_pieces.to_vec();
        job.updated_at = now as i64;
        if matches!(state, JOB_STATE_CANCELLED | JOB_STATE_COMPLETED) {
            job.finished_at = Some(now as i64);
        }
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.to_owned(),
            occurred_at: now as i64,
            kind: match state {
                JOB_STATE_CANCELLED => "check_cancelled",
                JOB_STATE_COMPLETED => "check_completed",
                _ => "check_progress",
            }
            .to_owned(),
            message: message.map(str::to_owned),
            payload: serde_json::json!({
                "piece_index": job.piece_index,
                "verified_bytes": job.verified_bytes,
                "invalid_pieces": job.invalid_pieces,
                "state": state,
            })
            .to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::upsert_job(&db, &job) {
            warn!(
                component = "db",
                operation = "persist_recheck_progress",
                job_id,
                state,
                result = "error",
                error = %e,
                "failed to persist recheck progress"
            );
            return;
        }
        if let Err(e) = rt_db::append_job_event(&db, &event) {
            warn!(
                component = "db",
                operation = "append_job_event",
                job_id,
                kind = %event.kind,
                result = "error",
                error = %e,
                "failed to append recheck progress event"
            );
        }
    }

    fn verified_byte_offset(&self, next_piece: u32) -> i64 {
        let bytes = (next_piece as u64).saturating_mul(self.meta.piece_length);
        bytes.min(self.meta.total_length()) as i64
    }

    async fn save_fastresume(&mut self, full_verify: bool) {
        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let mut state = FastresumeState::new_empty(
            &self.meta.info_hash,
            self.meta.pieces.len() as u32,
            ImportPolicy::RequireVerification,
        );
        state.pieces = self
            .picker
            .have_pieces()
            .into_iter()
            .map(|have| {
                if have {
                    PieceState::Valid
                } else {
                    PieceState::Unknown
                }
            })
            .collect();
        state.partial_pieces = self
            .picker
            .partial_pieces()
            .into_iter()
            .map(|(piece, received_blocks)| PartialPieceState {
                piece,
                received_blocks,
            })
            .collect();
        state.uploaded_bytes = uploaded;
        state.downloaded_bytes = downloaded;
        state.set_dirty_pieces_since_barrier(self.dirty_pieces_since_barrier.iter().copied());
        if self.sync_before_clean_fastresume().await {
            state.complete_durability_barrier();
            self.dirty_pieces_since_barrier.clear();
        } else {
            state.clean_shutdown = false;
        }
        state.file_hints = match collect_file_hints(&self.save_root, &self.meta) {
            Ok(hints) => hints,
            Err(e) => {
                warn!(
                    component = "fastresume",
                    operation = "collect_file_hints",
                    torrent = %self.info_hash_hex,
                    result = "error",
                    error = %e,
                    "failed to collect fastresume file hints"
                );
                Vec::new()
            }
        };
        if full_verify {
            state.last_full_verify = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        }

        if let Err(e) = self.fastresume.save(&state) {
            warn!(
                component = "fastresume",
                operation = "save",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %e,
                "failed to save fastresume state"
            );
        }
    }

    async fn sync_before_clean_fastresume(&self) -> bool {
        match self.storage.io_config().durability_mode {
            rt_storage::DurabilityMode::Fast => true,
            rt_storage::DurabilityMode::Checkpoint | rt_storage::DurabilityMode::Strict => {
                match self.storage.sync_all_open_files().await {
                    Ok(()) => true,
                    Err(e) => {
                        warn!(
                            component = "storage",
                            operation = "sync_before_fastresume",
                            torrent = %self.info_hash_hex,
                            result = "error",
                            error = %e,
                            "failed to sync torrent files before clean fastresume save"
                        );
                        false
                    }
                }
            }
        }
    }
}

fn collect_file_hints(
    root: &std::path::Path,
    meta: &TorrentMetaV1,
) -> anyhow::Result<Vec<FileHint>> {
    meta.files
        .iter()
        .map(|file| {
            let path = file.path.resolve(root);
            let metadata = std::fs::metadata(&path)?;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);

            #[cfg(unix)]
            let inode = {
                use std::os::unix::fs::MetadataExt;
                metadata.ino()
            };
            #[cfg(not(unix))]
            let inode = 0;

            Ok(FileHint {
                file_index: file.index,
                size: metadata.len(),
                mtime_secs: modified,
                inode,
            })
        })
        .collect()
}

fn tracker_tiers_from_meta(meta: &TorrentMetaV1) -> Vec<Vec<TrackerState>> {
    let mut seen = std::collections::HashSet::new();
    let mut tiers = Vec::new();

    if !meta.announce_list.is_empty() {
        for tier in &meta.announce_list {
            let trackers: Vec<TrackerState> = tier
                .iter()
                .filter_map(|url| {
                    if seen.insert(url.clone()) {
                        Some(TrackerState::new(url.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            if !trackers.is_empty() {
                tiers.push(trackers);
            }
        }
    }

    if let Some(url) = &meta.announce {
        if seen.insert(url.clone()) {
            tiers.insert(0, vec![TrackerState::new(url.clone())]);
        }
    }

    tiers
}

fn private_peer_source_allowed(
    is_private: bool,
    allowed_private_peers: &HashSet<SocketAddr>,
    peer: SocketAddr,
) -> bool {
    !is_private
        || allowed_private_peers.contains(&peer)
        || (private_peer_port_fallback_allowed(peer.ip())
            && allowed_private_peers
                .iter()
                .any(|allowed| allowed.ip() == peer.ip()))
}

fn private_peer_port_fallback_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn peer_client_label(peer: &PeerHandle) -> String {
    if peer.ut_metadata_id.is_some() {
        "BEP10 peer".to_owned()
    } else {
        "BitTorrent peer".to_owned()
    }
}

fn webseed_block_url(meta: &TorrentMetaV1, webseed: &str) -> Option<Url> {
    let parsed = Url::parse(webseed).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let first_file = meta.files.first()?;
    let components = first_file.path.components();
    if !webseed.ends_with('/')
        && components.len() > 1
        && components
            .first()
            .is_some_and(|component| component == &meta.name)
        && parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .is_some_and(|last| last == meta.name)
    {
        let mut url = parsed;
        {
            let mut segments = url.path_segments_mut().ok()?;
            for component in &components[1..] {
                segments.push(component);
            }
        }
        return Some(url);
    }
    if webseed.ends_with('/') {
        parsed.join(&meta.name).ok()
    } else {
        Some(parsed)
    }
}

fn parse_ut_pex_peers(payload: &[u8]) -> anyhow::Result<Vec<SocketAddr>> {
    let value = decode(payload)?;
    let BValue::Dict(pairs) = value else {
        anyhow::bail!("ut_pex payload must be a dict");
    };
    let Some(added) = pairs
        .iter()
        .find(|(key, _)| *key == b"added")
        .and_then(|(_, value)| value.as_bytes())
    else {
        return Ok(Vec::new());
    };
    if !added.len().is_multiple_of(6) {
        anyhow::bail!("ut_pex added peers length is not a multiple of 6");
    }
    Ok(added
        .chunks_exact(6)
        .filter_map(|chunk| {
            let port = u16::from_be_bytes([chunk[4], chunk[5]]);
            (port != 0).then_some(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]),
                port,
            )))
        })
        .collect())
}

fn reconcile_peer_availability(availability: &mut Availability, old: &[bool], new: &[bool]) {
    let piece_count = availability.piece_count();
    for piece in 0..piece_count {
        match (
            old.get(piece).copied().unwrap_or(false),
            new.get(piece).copied().unwrap_or(false),
        ) {
            (false, true) => availability.add_have(piece),
            (true, false) => availability.remove_have(piece),
            _ => {}
        }
    }
}

fn tracker_event_after_success(current: TrackerEvent, sent: TrackerEvent) -> TrackerEvent {
    if matches!(sent, TrackerEvent::Started | TrackerEvent::Completed) {
        TrackerEvent::Empty
    } else {
        current
    }
}

fn consume_stopped_announce(stopped_announced: &mut bool) -> bool {
    if *stopped_announced {
        false
    } else {
        *stopped_announced = true;
        true
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn instant_to_unix(instant: Option<Instant>, now: Instant) -> Option<i64> {
    let current = unix_now() as i64;
    instant.map(|instant| {
        if instant >= now {
            current.saturating_add(instant.duration_since(now).as_secs() as i64)
        } else {
            current.saturating_sub(now.duration_since(instant).as_secs() as i64)
        }
    })
}

fn tracker_status_label(status: &TrackerStatus) -> &'static str {
    match status {
        TrackerStatus::NeverAnnounced => "never_announced",
        TrackerStatus::Announcing => "announcing",
        TrackerStatus::Working => "working",
        TrackerStatus::Warning(_) => "warning",
        TrackerStatus::Error(_) => "error",
        TrackerStatus::Disabled => "disabled",
    }
}

fn tracker_failure_reason(status: &TrackerStatus) -> Option<String> {
    match status {
        TrackerStatus::Error(err) => Some(err.to_string()),
        _ => None,
    }
}

fn tracker_warning_message(status: &TrackerStatus) -> Option<String> {
    match status {
        TrackerStatus::Warning(message) => Some(message.clone()),
        _ => None,
    }
}

/// Open a TCP connection, complete BEP 3 handshake, receive Piece messages.
async fn run_outgoing_peer(
    addr: SocketAddr,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
) -> anyhow::Result<()> {
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await??;
    stream.set_nodelay(true)?;

    let mut framed = Framed::new(stream, PeerCodec);

    let our_hs = Handshake {
        info_hash,
        peer_id: OUR_PEER_ID,
        reserved: ExtensionFlags::with_extension_protocol(),
    };
    // Send our handshake as raw bytes before the codec takes over.
    {
        use tokio::io::AsyncWriteExt;
        let inner = framed.get_mut();
        inner.write_all(&our_hs.encode()).await?;
    }

    let remote_supports_extension = {
        use tokio::io::AsyncReadExt;
        let mut hs_buf = [0u8; 68];
        framed.get_mut().read_exact(&mut hs_buf).await?;
        let remote_hs = Handshake::parse(&hs_buf)?;
        if remote_hs.info_hash != info_hash {
            anyhow::bail!("info_hash mismatch from {addr}");
        }
        remote_hs.reserved.supports_extension_protocol()
    };

    send_extension_handshake(
        &mut framed,
        upload.metadata.as_ref(),
        upload.is_private,
        remote_supports_extension,
    )
    .await?;
    send_have_state(&mut framed, &upload.have_pieces).await?;
    framed.send(Message::Interested).await?;

    run_peer_loop(addr, framed, peer_event_tx, peer_cmd_rx, upload).await
}

async fn run_incoming_peer(
    stream: TcpStream,
    addr: SocketAddr,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
    remote_supports_extension: bool,
) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let mut framed = Framed::new(stream, PeerCodec);

    let our_hs = Handshake {
        info_hash,
        peer_id: OUR_PEER_ID,
        reserved: ExtensionFlags::with_extension_protocol(),
    };
    {
        use tokio::io::AsyncWriteExt;
        framed.get_mut().write_all(&our_hs.encode()).await?;
    }

    send_extension_handshake(
        &mut framed,
        upload.metadata.as_ref(),
        upload.is_private,
        remote_supports_extension,
    )
    .await?;
    send_have_state(&mut framed, &upload.have_pieces).await?;
    framed.send(Message::Interested).await?;
    run_peer_loop(addr, framed, peer_event_tx, peer_cmd_rx, upload).await
}

async fn run_peer_loop(
    addr: SocketAddr,
    mut framed: Framed<TcpStream, PeerCodec>,
    peer_event_tx: mpsc::Sender<PeerEvent>,
    mut peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    mut upload: UploadContext,
) -> anyhow::Result<()> {
    let mut outstanding = Vec::<OutstandingRequest>::new();
    let mut upload_choked = true;
    let mut timeout_tick = interval(Duration::from_secs(5));

    let result: anyhow::Result<()> = async {
        loop {
        tokio::select! {
            Some(cmd) = peer_cmd_rx.recv() => {
                match cmd {
                    PeerCommand::Request(req) => {
                        framed.send(Message::Request {
                            piece: req.piece,
                            begin: req.begin,
                            length: req.length,
                        }).await?;
                        outstanding.push(OutstandingRequest::new(req));
                    }
                    PeerCommand::Have(piece) => {
                        if let Some(has_piece) = upload.have_pieces.get_mut(piece as usize) {
                            *has_piece = true;
                        }
                        framed.send(Message::Have(piece)).await?;
                    }
                    PeerCommand::Choke => {
                        upload_choked = true;
                        framed.send(Message::Choke).await?;
                    }
                    PeerCommand::Unchoke => {
                        upload_choked = false;
                        framed.send(Message::Unchoke).await?;
                    }
                    PeerCommand::Shutdown => {
                        break;
                    }
                }
            }
            msg_result = framed.next() => {
                let Some(msg_result) = msg_result else {
                    break;
                };
                match msg_result? {
                    Message::Bitfield(bits) => {
                        let pieces = match bitfield_to_pieces(&bits, upload.have_pieces.len()) {
                            Ok(pieces) => pieces,
                            Err(e) => {
                                debug!(
                                    component = "peer",
                                    operation = "parse_bitfield",
                                    peer = %addr,
                                    result = "error",
                                    error = %e,
                                    "ignoring invalid bitfield"
                                );
                                continue;
                            }
                        };
                        if peer_event_tx.send(PeerEvent::Bitfield { peer: addr, pieces }).await.is_err() {
                            break;
                        }
                    }
                    Message::Have(piece) => {
                        if piece as usize >= upload.have_pieces.len() {
                            debug!(
                                component = "peer",
                                operation = "handle_have",
                                peer = %addr,
                                piece,
                                result = "ignored",
                                reason = "out_of_range",
                                "ignoring out-of-range have"
                            );
                            continue;
                        }
                        if peer_event_tx.send(PeerEvent::Have { peer: addr, piece }).await.is_err() {
                            break;
                        }
                    }
                    Message::Piece { piece, begin, data } => {
                        let data_len = data.len() as u32;
                        if !take_matching_outstanding(&mut outstanding, piece, begin, data_len) {
                            warn!(
                                peer = %addr,
                                piece,
                                begin,
                                length = data_len,
                                "dropping unsolicited or mismatched piece block"
                            );
                            continue;
                        }
                        if peer_event_tx
                            .send(PeerEvent::Piece {
                                peer: addr,
                                block: BlockEvent {
                                    piece,
                                    offset: begin,
                                    data: bytes::Bytes::from(data),
                                },
                            })
                            .await
                            .is_err()
                        {
                            break; // torrent task gone
                        }
                    }
                    Message::Unchoke => {
                        if peer_event_tx.send(PeerEvent::Unchoked { peer: addr }).await.is_err() {
                            break;
                        }
                    }
                    Message::Interested => {
                        if peer_event_tx.send(PeerEvent::Interested { peer: addr }).await.is_err() {
                            break;
                        }
                    }
                    Message::NotInterested => {
                        if peer_event_tx.send(PeerEvent::NotInterested { peer: addr }).await.is_err() {
                            break;
                        }
                    }
                    Message::Request { piece, begin, length } => {
                        if !upload_choked && upload.have_pieces.get(piece as usize).copied().unwrap_or(false) {
                            match read_upload_block(&upload, piece, begin, length).await {
                                Ok(block) => {
                                    let bytes = block.data.len() as u64;
                                    framed.send(Message::Piece {
                                        piece,
                                        begin,
                                        data: block.data.to_vec(),
                                    }).await?;
                                    if peer_event_tx
                                        .send(PeerEvent::Uploaded { peer: addr, bytes })
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        component = "peer",
                                        operation = "read_upload_block",
                                        peer = %addr,
                                        piece,
                                        begin,
                                        length,
                                        result = "error",
                                        error = %e,
                                        "failed to read upload block"
                                    );
                                }
                            }
                        }
                    }
                    Message::Choke => {
                        let choked_requests = drain_outstanding(&mut outstanding);
                        if peer_event_tx
                            .send(PeerEvent::Choked {
                                peer: addr,
                                outstanding: choked_requests,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                        warn!(
                            component = "peer",
                            operation = "handle_choke",
                            peer = %addr,
                            result = "choked",
                            "choked"
                        );
                    }
                    Message::KeepAlive => {}
                    Message::Extended { ext_id: EXT_HANDSHAKE_ID, payload } => {
                        match ExtensionHandshake::parse(&payload) {
                            Ok(handshake) => {
                                if peer_event_tx
                                    .send(PeerEvent::ExtendedHandshake {
                                        peer: addr,
                                        ut_metadata_id: handshake.ut_metadata_id(),
                                        ut_pex_id: handshake.ut_pex_id(),
                                        metadata_size: handshake.metadata_size,
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(e) => {
                                debug!(
                                    component = "peer",
                                    operation = "parse_extension_handshake",
                                    peer = %addr,
                                    result = "error",
                                    error = %e,
                                    "ignoring invalid extension handshake"
                                );
                            }
                        }
                    }
                    Message::Extended { ext_id: LOCAL_UT_METADATA_ID, payload } => {
                        match UtMetadataMessage::parse(&payload) {
                            Ok(UtMetadataMessage::Request { piece }) => {
                                let response = upload
                                    .metadata
                                    .as_ref()
                                    .map(|metadata| metadata_response(piece, metadata))
                                    .unwrap_or(UtMetadataMessage::Reject { piece });
                                framed.send(Message::Extended {
                                    ext_id: LOCAL_UT_METADATA_ID,
                                    payload: response.encode(),
                                }).await?;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                debug!(
                                    component = "peer",
                                    operation = "parse_ut_metadata",
                                    peer = %addr,
                                    result = "error",
                                    error = %e,
                                    "ignoring invalid ut_metadata message"
                                );
                            }
                        }
                    }
                    Message::Extended { ext_id: LOCAL_UT_PEX_ID, payload } => {
                        match parse_ut_pex_peers(&payload) {
                            Ok(peers) if !peers.is_empty() => {
                                if peer_event_tx
                                    .send(PeerEvent::PeerExchange { peer: addr, peers })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                debug!(
                                    component = "peer",
                                    operation = "parse_ut_pex",
                                    peer = %addr,
                                    result = "error",
                                    error = %e,
                                    "ignoring invalid ut_pex message"
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ = timeout_tick.tick() => {
                let timed_out = take_timed_out_requests(&mut outstanding, Duration::from_secs(60));
                if !timed_out.is_empty()
                    && peer_event_tx
                        .send(PeerEvent::RequestTimedOut {
                            peer: addr,
                            timed_out,
                        })
                        .await
                        .is_err()
                {
                    break;
                }
            }
        }
        }
        Ok(())
    }
    .await;

    let _ = peer_event_tx
        .send(PeerEvent::Disconnected {
            peer: addr,
            outstanding: drain_outstanding(&mut outstanding),
        })
        .await;
    result
}

async fn send_extension_handshake(
    framed: &mut Framed<TcpStream, PeerCodec>,
    metadata: Option<&Arc<Vec<u8>>>,
    is_private: bool,
    remote_supports_extension: bool,
) -> anyhow::Result<()> {
    if remote_supports_extension {
        let handshake = extension_handshake_for_torrent(metadata, is_private);
        framed
            .send(Message::Extended {
                ext_id: EXT_HANDSHAKE_ID,
                payload: handshake.encode(),
            })
            .await?;
    }
    Ok(())
}

fn extension_handshake_for_torrent(
    metadata: Option<&Arc<Vec<u8>>>,
    is_private: bool,
) -> ExtensionHandshake {
    let metadata_size = metadata.and_then(|bytes| u32::try_from(bytes.len()).ok());
    let mut handshake = ExtensionHandshake::new(metadata_size);
    if metadata_size.is_some() {
        handshake = handshake.with_ut_metadata(LOCAL_UT_METADATA_ID);
    }
    if !is_private {
        handshake = handshake.with_ut_pex(LOCAL_UT_PEX_ID);
    }
    handshake
}

fn metadata_response(piece: u32, metadata: &[u8]) -> UtMetadataMessage {
    let start = piece as usize * METADATA_PIECE_SIZE;
    if start >= metadata.len() {
        return UtMetadataMessage::Reject { piece };
    }
    let end = (start + METADATA_PIECE_SIZE).min(metadata.len());
    UtMetadataMessage::Data {
        piece,
        total_size: metadata.len() as u32,
        data: metadata[start..end].to_vec(),
    }
}

#[derive(Debug, Clone, Copy)]
struct OutstandingRequest {
    req: BlockRequest,
    sent_at: Instant,
}

impl OutstandingRequest {
    fn new(req: BlockRequest) -> Self {
        Self {
            req,
            sent_at: Instant::now(),
        }
    }
}

async fn send_have_state(
    framed: &mut Framed<TcpStream, PeerCodec>,
    have_pieces: &[bool],
) -> anyhow::Result<()> {
    let bitfield = pieces_to_bitfield(have_pieces);
    if bitfield.iter().any(|byte| *byte != 0) {
        framed.send(Message::Bitfield(bitfield)).await?;
    }
    Ok(())
}

async fn read_upload_block(
    upload: &UploadContext,
    piece: u32,
    begin: u32,
    length: u32,
) -> anyhow::Result<LeasedUploadBlock> {
    let lease = reserve_peer_upload_bytes(&upload.resources, length)?;
    let regions = upload.piece_map.validate_request(piece, begin, length)?;
    let mut data = Vec::with_capacity(length as usize);
    for region in regions {
        let path = region.path.resolve(&upload.save_root);
        let read = scheduled_read_owned(
            &upload.storage,
            IoClass::PeerRead,
            &path,
            region.file_offset,
            region.length as usize,
        )
        .await?;
        data.extend_from_slice(read.as_slice());
    }
    if data.len() != length as usize {
        anyhow::bail!(
            "upload block read assembled {} bytes, expected {}",
            data.len(),
            length
        );
    }
    Ok(LeasedUploadBlock {
        data: bytes::Bytes::from(data),
        _lease: lease,
    })
}

fn bitfield_to_pieces(bits: &[u8], piece_count: usize) -> anyhow::Result<Vec<bool>> {
    let expected_len = piece_count.div_ceil(8);
    if bits.len() != expected_len {
        anyhow::bail!(
            "bitfield length {} does not match expected {}",
            bits.len(),
            expected_len
        );
    }
    if piece_count % 8 != 0 && !bits.is_empty() {
        let used_bits = piece_count % 8;
        let spare_mask = (1u8 << (8 - used_bits)) - 1;
        if bits[bits.len() - 1] & spare_mask != 0 {
            anyhow::bail!("bitfield has non-zero spare bits");
        }
    }

    let mut pieces = Vec::with_capacity(piece_count);
    for byte in bits {
        for bit in (0..8).rev() {
            if pieces.len() == piece_count {
                return Ok(pieces);
            }
            pieces.push((byte & (1 << bit)) != 0);
        }
    }
    Ok(pieces)
}

fn pieces_to_bitfield(pieces: &[bool]) -> Vec<u8> {
    let mut bits = vec![0u8; pieces.len().div_ceil(8)];
    for (idx, has_piece) in pieces.iter().copied().enumerate() {
        if has_piece {
            bits[idx / 8] |= 0x80 >> (idx % 8);
        }
    }
    bits
}

fn take_matching_outstanding(
    outstanding: &mut Vec<OutstandingRequest>,
    piece: u32,
    begin: u32,
    length: u32,
) -> bool {
    let Some(pos) = outstanding.iter().position(|out| {
        out.req.piece == piece && out.req.begin == begin && out.req.length == length
    }) else {
        return false;
    };
    outstanding.swap_remove(pos);
    true
}

fn remove_requested_block(requested: &mut Vec<BlockRequest>, piece: u32, begin: u32) {
    if let Some(pos) = requested
        .iter()
        .position(|req| req.piece == piece && req.begin == begin)
    {
        requested.swap_remove(pos);
    }
}

fn take_timed_out_requests(
    outstanding: &mut Vec<OutstandingRequest>,
    timeout: Duration,
) -> Vec<BlockRequest> {
    let now = Instant::now();
    let mut timed_out = Vec::new();
    let mut idx = 0;
    while idx < outstanding.len() {
        if now.duration_since(outstanding[idx].sent_at) >= timeout {
            timed_out.push(outstanding.swap_remove(idx).req);
        } else {
            idx += 1;
        }
    }
    timed_out
}

fn drain_outstanding(outstanding: &mut Vec<OutstandingRequest>) -> Vec<BlockRequest> {
    outstanding.drain(..).map(|out| out.req).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_peer_wire_bitfield_msb_first() {
        assert_eq!(
            bitfield_to_pieces(&[0b1010_0000], 8).unwrap(),
            vec![true, false, true, false, false, false, false, false]
        );
    }

    #[test]
    fn rejects_invalid_peer_wire_bitfield_shape() {
        assert!(bitfield_to_pieces(&[0b1010_0000, 0], 8).is_err());
        assert!(bitfield_to_pieces(&[], 8).is_err());
        assert!(bitfield_to_pieces(&[0b1010_0001], 4).is_err());
        assert_eq!(
            bitfield_to_pieces(&[0b1010_0000], 4).unwrap(),
            vec![true, false, true, false]
        );
    }

    #[test]
    fn encodes_piece_flags_to_peer_wire_bitfield_msb_first() {
        assert_eq!(
            pieces_to_bitfield(&[true, false, true, false, false, false, false, false, true]),
            vec![0b1010_0000, 0b1000_0000]
        );
    }

    #[test]
    fn webseed_block_url_accepts_direct_file_and_base_url() {
        let meta = TorrentMetaV1 {
            info_hash: [1; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            name: "sample.iso".into(),
            piece_length: 16_384,
            pieces: vec![[2; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 5,
                path: rt_path::SafeRelPath::from_name("sample.iso", false).unwrap(),
                offset: 0,
            }],
            private: false,
            raw: Vec::new(),
        };

        assert_eq!(
            webseed_block_url(&meta, "https://mirror.example/sample.iso")
                .unwrap()
                .as_str(),
            "https://mirror.example/sample.iso"
        );
        assert_eq!(
            webseed_block_url(&meta, "https://mirror.example/releases/")
                .unwrap()
                .as_str(),
            "https://mirror.example/releases/sample.iso"
        );
        assert!(webseed_block_url(&meta, "ftp://mirror.example/sample.iso").is_none());
    }

    #[test]
    fn webseed_block_url_expands_single_file_directory_prefix() {
        let meta = TorrentMetaV1 {
            info_hash: [1; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            name: "payload-dir".into(),
            piece_length: 16_384,
            pieces: vec![[2; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 5,
                path: rt_path::SafeRelPath::from_components(&["payload-dir", "payload.bin"], false)
                    .unwrap(),
                offset: 0,
            }],
            private: false,
            raw: Vec::new(),
        };

        assert_eq!(
            webseed_block_url(&meta, "https://mirror.example/payload-dir")
                .unwrap()
                .as_str(),
            "https://mirror.example/payload-dir/payload.bin"
        );
    }

    #[test]
    fn parses_ut_pex_added_ipv4_peers() {
        let added = [127, 0, 0, 1, 0x1a, 0xe1, 10, 0, 0, 2, 0x13, 0x88];
        let payload = rt_bencode::encode(&BValue::Dict(vec![(
            b"added".as_slice(),
            BValue::Bytes(&added),
        )]));

        let peers = parse_ut_pex_peers(&payload).unwrap();

        assert_eq!(
            peers,
            vec![
                "127.0.0.1:6881".parse::<SocketAddr>().unwrap(),
                "10.0.0.2:5000".parse::<SocketAddr>().unwrap(),
            ]
        );
    }

    #[test]
    fn private_torrent_extension_handshake_does_not_advertise_pex() {
        let metadata = Arc::new(vec![1, 2, 3, 4]);

        let public = extension_handshake_for_torrent(Some(&metadata), false);
        let private = extension_handshake_for_torrent(Some(&metadata), true);

        assert_eq!(public.ut_metadata_id(), Some(LOCAL_UT_METADATA_ID));
        assert_eq!(public.ut_pex_id(), Some(LOCAL_UT_PEX_ID));
        assert_eq!(private.ut_metadata_id(), Some(LOCAL_UT_METADATA_ID));
        assert_eq!(private.ut_pex_id(), None);
    }

    #[test]
    fn file_hints_capture_size_mtime_and_inode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.bin"), b"hello").unwrap();

        let meta = TorrentMetaV1 {
            info_hash: [1; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            name: "sample.bin".into(),
            piece_length: 16_384,
            pieces: vec![[2; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 3,
                length: 5,
                path: rt_path::SafeRelPath::from_name("sample.bin", false).unwrap(),
                offset: 0,
            }],
            private: false,
            raw: Vec::new(),
        };

        let hints = collect_file_hints(dir.path(), &meta).unwrap();

        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].file_index, 3);
        assert_eq!(hints[0].size, 5);
        assert!(hints[0].mtime_secs > 0);
    }

    #[test]
    fn outstanding_piece_match_requires_exact_length() {
        let mut outstanding = vec![OutstandingRequest::new(BlockRequest {
            piece: 4,
            begin: 16_384,
            length: 16_384,
        })];

        assert!(!take_matching_outstanding(
            &mut outstanding,
            4,
            16_384,
            8_192
        ));
        assert_eq!(outstanding.len(), 1);
        assert!(take_matching_outstanding(
            &mut outstanding,
            4,
            16_384,
            16_384
        ));
        assert!(outstanding.is_empty());
    }

    #[test]
    fn timed_out_requests_are_returned_for_requeue() {
        let mut outstanding = vec![
            OutstandingRequest {
                req: BlockRequest {
                    piece: 1,
                    begin: 0,
                    length: 16_384,
                },
                sent_at: Instant::now() - Duration::from_secs(120),
            },
            OutstandingRequest::new(BlockRequest {
                piece: 2,
                begin: 0,
                length: 16_384,
            }),
        ];

        let timed_out = take_timed_out_requests(&mut outstanding, Duration::from_secs(60));

        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0].piece, 1);
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].req.piece, 2);
    }

    #[test]
    fn piece_assembly_tracks_complete_piece_bytes() {
        let mut assembly = PieceAssembly::new((MAX_BLOCK_SIZE * 2) as usize);
        assembly.insert(0, &[1; MAX_BLOCK_SIZE as usize]).unwrap();
        assert!(!assembly.is_complete());
        assembly
            .insert(MAX_BLOCK_SIZE, &[2; MAX_BLOCK_SIZE as usize])
            .unwrap();
        assert!(assembly.is_complete());
        assert_eq!(assembly.data[0], 1);
        assert_eq!(assembly.data[MAX_BLOCK_SIZE as usize], 2);
    }

    #[test]
    fn piece_assembly_rejects_out_of_range_block() {
        let mut assembly = PieceAssembly::new(4);
        let err = assembly.insert(2, &[1, 2, 3]).unwrap_err();
        assert!(err.to_string().contains("exceeds piece length"));
    }

    #[test]
    fn piece_assembly_rejects_conflicting_duplicate_block() {
        let mut assembly = PieceAssembly::new(MAX_BLOCK_SIZE as usize);
        assembly.insert(0, &[1; MAX_BLOCK_SIZE as usize]).unwrap();
        assembly.insert(0, &[1; MAX_BLOCK_SIZE as usize]).unwrap();

        let err = assembly
            .insert(0, &[2; MAX_BLOCK_SIZE as usize])
            .unwrap_err();
        assert!(err.to_string().contains("conflicting duplicate block"));
    }

    #[test]
    fn piece_assembly_budget_evicts_oldest_incomplete_piece() {
        let now = Instant::now();
        let mut assemblies = HashMap::new();
        assemblies.insert(
            1,
            PieceAssembly {
                last_used: now - Duration::from_secs(30),
                ..PieceAssembly::new(4)
            },
        );
        assemblies.insert(
            2,
            PieceAssembly {
                last_used: now - Duration::from_secs(20),
                ..PieceAssembly::new(4)
            },
        );
        assemblies.insert(
            3,
            PieceAssembly {
                last_used: now,
                ..PieceAssembly::new(4)
            },
        );
        let mut bytes = 12;

        let evictions = evict_piece_assemblies_to_budget(&mut assemblies, &mut bytes, 3, 2, 12);

        assert_eq!(evictions, 1);
        assert_eq!(bytes, 8);
        assert!(!assemblies.contains_key(&1));
        assert!(assemblies.contains_key(&2));
        assert!(assemblies.contains_key(&3));
    }

    #[test]
    fn piece_assembly_budget_preserves_current_piece_when_possible() {
        let now = Instant::now();
        let mut assemblies = HashMap::new();
        assemblies.insert(
            1,
            PieceAssembly {
                last_used: now - Duration::from_secs(60),
                ..PieceAssembly::new(4)
            },
        );
        assemblies.insert(
            2,
            PieceAssembly {
                last_used: now - Duration::from_secs(30),
                ..PieceAssembly::new(4)
            },
        );
        assemblies.insert(
            3,
            PieceAssembly {
                last_used: now - Duration::from_secs(90),
                ..PieceAssembly::new(4)
            },
        );
        let mut bytes = 12;

        let evictions = evict_piece_assemblies_to_budget(&mut assemblies, &mut bytes, 3, 3, 8);

        assert_eq!(evictions, 1);
        assert_eq!(bytes, 8);
        assert!(!assemblies.contains_key(&1));
        assert!(assemblies.contains_key(&2));
        assert!(assemblies.contains_key(&3));
    }

    #[test]
    fn piece_assembly_budget_stops_at_current_piece_only() {
        let mut assemblies = HashMap::new();
        assemblies.insert(7, PieceAssembly::new(16));
        let mut bytes = 16;

        let evictions = evict_piece_assemblies_to_budget(&mut assemblies, &mut bytes, 7, 0, 0);

        assert_eq!(evictions, 0);
        assert_eq!(bytes, 16);
        assert!(assemblies.contains_key(&7));
    }

    #[test]
    fn configured_piece_assembly_cap_is_per_torrent_soft_ceiling() {
        assert_eq!(
            effective_piece_assembly_soft_cap(8 * 1024 * 1024),
            8 * 1024 * 1024
        );
        assert_eq!(
            effective_piece_assembly_soft_cap(512 * 1024 * 1024),
            MAX_IN_MEMORY_PIECE_ASSEMBLY_BYTES_PER_TORRENT
        );
    }

    #[test]
    fn request_pipeline_reduces_near_piece_assembly_cap() {
        assert_eq!(
            memory_aware_request_pipeline(0, 1024),
            PEER_REQUEST_PIPELINE_NORMAL
        );
        assert_eq!(
            memory_aware_request_pipeline(767, 1024),
            PEER_REQUEST_PIPELINE_NORMAL
        );
        assert_eq!(
            memory_aware_request_pipeline(768, 1024),
            PEER_REQUEST_PIPELINE_CONSTRAINED
        );
        assert_eq!(memory_aware_request_pipeline(1, 0), 0);
    }

    #[test]
    fn tracker_peer_cache_cap_scales_with_peer_limit() {
        assert_eq!(tracker_peer_cache_cap(1), TRACKER_PEER_CACHE_MIN);
        assert_eq!(tracker_peer_cache_cap(100), 400);
    }

    #[test]
    fn webseed_body_reservation_uses_webseed_governor_class() {
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::WebseedBody as usize] = 16;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 16,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let lease = reserve_webseed_body_bytes(&governor, 16).unwrap();
        assert_eq!(
            governor.snapshot().classes[MemoryClass::WebseedBody as usize].used_bytes,
            16
        );
        drop(lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::WebseedBody as usize].used_bytes,
            0
        );
        assert!(reserve_webseed_body_bytes(&governor, 17).is_err());
        assert_eq!(
            governor.snapshot().classes[MemoryClass::WebseedBody as usize].denied_allocations,
            1
        );
    }

    #[test]
    fn tracker_peer_cache_drops_new_peers_after_cap() {
        let peers = [
            SocketAddr::from(([127, 0, 0, 1], 6881)),
            SocketAddr::from(([127, 0, 0, 2], 6881)),
            SocketAddr::from(([127, 0, 0, 3], 6881)),
        ];
        let mut known = HashSet::new();
        let mut allowed_private = HashSet::new();

        let dropped =
            remember_tracker_peers_bounded(&mut known, &mut allowed_private, &peers, true, 2);

        assert_eq!(known.len(), 2);
        assert_eq!(allowed_private, known);
        assert_eq!(dropped, 1);

        let duplicate = *known.iter().next().unwrap();
        let dropped =
            remember_tracker_peers_bounded(&mut known, &mut allowed_private, &[duplicate], true, 2);

        assert_eq!(known.len(), 2);
        assert_eq!(allowed_private, known);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn tracker_tiers_preserve_bep12_order_and_dedupe() {
        let meta = TorrentMetaV1 {
            info_hash: [1; 20],
            announce: Some("http://tracker-a/announce".into()),
            announce_list: vec![
                vec![
                    "http://tracker-a/announce".into(),
                    "http://tracker-b/announce".into(),
                ],
                vec!["udp://tracker-c:6969/announce".into()],
            ],
            webseeds: Vec::new(),
            name: "sample.bin".into(),
            piece_length: 16_384,
            pieces: vec![[2; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 5,
                path: rt_path::SafeRelPath::from_name("sample.bin", false).unwrap(),
                offset: 0,
            }],
            private: true,
            raw: Vec::new(),
        };

        let tiers = tracker_tiers_from_meta(&meta);

        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].len(), 2);
        assert_eq!(tiers[0][0].url, "http://tracker-a/announce");
        assert_eq!(tiers[0][1].url, "http://tracker-b/announce");
        assert_eq!(tiers[1][0].url, "udp://tracker-c:6969/announce");
    }

    #[test]
    fn tracker_status_persistence_fields_are_stable() {
        assert_eq!(
            tracker_status_label(&TrackerStatus::NeverAnnounced),
            "never_announced"
        );
        assert_eq!(tracker_status_label(&TrackerStatus::Working), "working");

        let warning = TrackerStatus::Warning("tracker says slow down".to_owned());
        assert_eq!(tracker_status_label(&warning), "warning");
        assert_eq!(
            tracker_warning_message(&warning).as_deref(),
            Some("tracker says slow down")
        );

        let failure = TrackerStatus::Error(TrackerError::Timeout);
        assert_eq!(tracker_status_label(&failure), "error");
        assert_eq!(
            tracker_failure_reason(&failure).as_deref(),
            Some("announce timed out")
        );
    }

    #[test]
    fn tracker_lifecycle_events_clear_after_one_success() {
        assert_eq!(
            tracker_event_after_success(TrackerEvent::Started, TrackerEvent::Started),
            TrackerEvent::Empty
        );
        assert_eq!(
            tracker_event_after_success(TrackerEvent::Completed, TrackerEvent::Completed),
            TrackerEvent::Empty
        );
        assert_eq!(
            tracker_event_after_success(TrackerEvent::Empty, TrackerEvent::Empty),
            TrackerEvent::Empty
        );
    }

    #[test]
    fn stopped_announce_is_consumed_once_per_session() {
        let mut stopped_announced = false;

        assert!(consume_stopped_announce(&mut stopped_announced));
        assert!(!consume_stopped_announce(&mut stopped_announced));

        stopped_announced = false;
        assert!(consume_stopped_announce(&mut stopped_announced));
    }

    #[test]
    fn private_peer_allowlist_only_accepts_tracker_peers() {
        let peer: SocketAddr = "127.0.0.1:6881".parse().unwrap();
        let same_host_ephemeral: SocketAddr = "127.0.0.1:49152".parse().unwrap();
        let other: SocketAddr = "127.0.0.2:6881".parse().unwrap();
        let public_peer: SocketAddr = "198.51.100.10:6881".parse().unwrap();
        let public_same_host_ephemeral: SocketAddr = "198.51.100.10:49152".parse().unwrap();
        let mut allowed = HashSet::new();

        assert!(!private_peer_source_allowed(true, &allowed, peer));
        allowed.insert(peer);
        assert!(private_peer_source_allowed(true, &allowed, peer));
        assert!(private_peer_source_allowed(
            true,
            &allowed,
            same_host_ephemeral
        ));
        assert!(!private_peer_source_allowed(true, &allowed, other));
        assert!(private_peer_source_allowed(false, &allowed, other));

        allowed.insert(public_peer);
        assert!(!private_peer_source_allowed(
            true,
            &allowed,
            public_same_host_ephemeral
        ));
    }

    #[test]
    fn peer_availability_reconcile_counts_only_transitions() {
        let mut availability = Availability::new(4);

        reconcile_peer_availability(
            &mut availability,
            &[false, false, false, false],
            &[true, false, true, false],
        );
        reconcile_peer_availability(
            &mut availability,
            &[true, false, true, false],
            &[true, true, false, false],
        );

        assert_eq!(availability.count(0), 1);
        assert_eq!(availability.count(1), 1);
        assert_eq!(availability.count(2), 0);
        assert_eq!(availability.count(3), 0);
    }

    #[tokio::test]
    async fn upload_block_reads_across_many_file_regions() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        let mut expected = Vec::new();
        let mut offset = 0u64;
        for idx in 0..64u32 {
            let path = rt_path::SafeRelPath::from_name(format!("{idx}.bin"), false).unwrap();
            let bytes = vec![idx as u8; 256];
            std::fs::write(path.resolve(dir.path()), &bytes).unwrap();
            expected.extend_from_slice(&bytes);
            files.push(FileSpan {
                file_index: idx,
                path,
                content_offset: offset,
                length: 256,
            });
            offset += 256;
        }
        let piece_map = PieceMap::new(16 * 1024, files).unwrap();
        let upload = UploadContext {
            save_root: dir.path().to_path_buf(),
            piece_map,
            storage: MountScheduler::new_for_path(
                StorageRootId::new(),
                dir.path(),
                &SchedulerConfig {
                    profile: StorageProfile::Unknown,
                    ..Default::default()
                },
            ),
            resources: ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default()),
            have_pieces: vec![true],
            metadata: None,
            is_private: false,
        };

        let block = read_upload_block(&upload, 0, 0, 16 * 1024).await.unwrap();

        assert_eq!(block.data.as_ref(), expected.as_slice());
        assert_eq!(
            upload.resources.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            16 * 1024
        );
        drop(block);
        assert_eq!(
            upload.resources.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            0
        );
    }

    #[test]
    fn upload_block_reservation_uses_peer_buffer_governor_class() {
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::PeerBuffer as usize] = 16 * 1024;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 16 * 1024,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let lease = reserve_peer_upload_bytes(&governor, 16 * 1024).unwrap();
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            16 * 1024
        );
        drop(lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            0
        );
        assert!(reserve_peer_upload_bytes(&governor, 16 * 1024 + 1).is_err());
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].denied_allocations,
            1
        );
    }
}
